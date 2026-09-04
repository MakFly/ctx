use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};

use crate::config::find_ctx;
use crate::db::{connect, get_meta};
use crate::model::{Envelope, Hit};

#[derive(Debug)]
struct SymbolRow {
    id: i64,
    path: String,
    start: usize,
    end: usize,
    name: String,
    signature: String,
    snippet: String,
    is_test: bool,
    is_vendor: bool,
}

pub fn search_index(
    query: &str,
    mode: &str,
    path_filter: Option<&str>,
    limit: usize,
    budget_tokens: usize,
    start: impl AsRef<Path>,
) -> Result<Envelope> {
    let started = Instant::now();
    let database = find_ctx(start)?.join("index.sqlite");
    let connection = connect(&database, false)?;
    let identifier = Regex::new(r"^[A-Za-z_][A-Za-z0-9_.]*$")?.is_match(query);
    let symbol_first = mode == "symbol" || (mode == "auto" && identifier);
    let path_pattern = path_filter.map(|value| format!("%{value}%"));
    let mut candidates: Vec<(SymbolRow, f64, String)> = Vec::new();

    if symbol_first {
        let mut statement = connection.prepare(
            "SELECT s.id,f.path,s.start,s.end,s.name,COALESCE(s.sig,''),
                    COALESCE(s.snippet,''),f.is_test,f.is_vendor
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE (s.name=?1 OR s.qualname=?1)
               AND (?2 IS NULL OR f.path LIKE ?2)
             ORDER BY f.is_test,f.is_vendor,f.path,s.start",
        )?;
        for row in statement.query_map(params![query, path_pattern], map_symbol_row)? {
            let row = row?;
            let score =
                1.0 - (row.is_test as u8 as f64 * 0.12) - (row.is_vendor as u8 as f64 * 0.25);
            candidates.push((row, score, "exact symbol".to_owned()));
        }
    }

    let fts_query = fts_query(query);
    let mut statement = connection.prepare(
        "SELECT s.id,f.path,s.start,s.end,s.name,COALESCE(s.sig,''),
                COALESCE(s.snippet,''),f.is_test,f.is_vendor,bm25(symbols_fts,5.0,3.0,1.0)
         FROM symbols_fts
         JOIN symbols s ON s.id=symbols_fts.rowid
         JOIN files f ON f.id=s.file_id
         WHERE symbols_fts MATCH ?1
           AND (?2 IS NULL OR f.path LIKE ?2)
         ORDER BY bm25(symbols_fts,5.0,3.0,1.0),f.is_test,f.is_vendor,f.path,s.start
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![fts_query, path_pattern, (limit * 3) as i64],
        |row| Ok((map_symbol_row(row)?, row.get::<_, f64>(9)?)),
    )?;
    for row in rows {
        let (row, rank) = row?;
        candidates.push((
            row,
            (0.85 / (1.0 + rank.abs())).max(0.1),
            "symbol/text match".to_owned(),
        ));
    }

    candidates.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.is_test.cmp(&right.0.is_test))
            .then_with(|| left.0.is_vendor.cmp(&right.0.is_vendor))
            .then_with(|| left.0.path.cmp(&right.0.path))
            .then_with(|| left.0.start.cmp(&right.0.start))
    });
    let mut seen = HashSet::new();
    let mut hits = Vec::new();
    for (row, score, why) in candidates {
        if !seen.insert(row.id) {
            continue;
        }
        hits.push(symbol_hit(row, score, why));
        if hits.len() >= limit {
            break;
        }
    }

    if hits.len() < limit {
        append_file_hits(
            &connection,
            query,
            path_pattern.as_deref(),
            limit,
            &mut hits,
        )?;
    }
    let has_structural: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE lang IS NOT NULL)",
        [],
        |row| row.get(0),
    )?;
    Ok(apply_budget(
        hits,
        budget_tokens,
        started,
        if has_structural {
            "complete"
        } else {
            "text_only"
        },
        None,
    ))
}

fn append_file_hits(
    connection: &Connection,
    query: &str,
    path_filter: Option<&str>,
    limit: usize,
    hits: &mut Vec<Hit>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT f.path,f.is_test,COALESCE(x.excerpt,''),bm25(files_fts)
         FROM files_fts
         JOIN files f ON f.id=files_fts.rowid
         LEFT JOIN file_excerpts x ON x.file_id=f.id
         WHERE files_fts MATCH ?1
           AND (?2 IS NULL OR f.path LIKE ?2)
         ORDER BY bm25(files_fts),f.is_test,f.path LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![fts_query(query), path_filter, limit as i64],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
            ))
        },
    )?;
    let mut seen = hits
        .iter()
        .map(|hit| (hit.path.clone(), hit.start, hit.kind.clone()))
        .collect::<HashSet<_>>();
    for row in rows {
        let (path, is_test, snippet, rank) = row?;
        let kind = if is_test {
            "test"
        } else if matches!(
            Path::new(&path)
                .extension()
                .and_then(|value| value.to_str()),
            Some("md" | "txt")
        ) {
            "doc"
        } else {
            "config"
        };
        if seen.insert((path.clone(), 1, kind.to_owned())) {
            hits.push(Hit {
                path,
                start: 1,
                end: 1,
                symbol: None,
                kind: kind.to_owned(),
                sig: String::new(),
                snippet,
                score: (0.55 / (1.0 + rank.abs())).max(0.08),
                why: "file text match".to_owned(),
            });
        }
        if hits.len() >= limit {
            break;
        }
    }
    Ok(())
}

fn map_symbol_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolRow> {
    Ok(SymbolRow {
        id: row.get(0)?,
        path: row.get(1)?,
        start: row.get::<_, i64>(2)?.max(1) as usize,
        end: row.get::<_, i64>(3)?.max(1) as usize,
        name: row.get(4)?,
        signature: row.get(5)?,
        snippet: row.get(6)?,
        is_test: row.get(7)?,
        is_vendor: row.get(8)?,
    })
}

fn symbol_hit(row: SymbolRow, score: f64, why: String) -> Hit {
    Hit {
        path: row.path,
        start: row.start,
        end: row.end,
        symbol: Some(row.name),
        kind: if row.is_test { "test" } else { "def" }.to_owned(),
        sig: row.signature,
        snippet: row.snippet,
        score: rounded(score),
        why,
    }
}

pub fn apply_budget(
    hits: Vec<Hit>,
    budget_tokens: usize,
    started: Instant,
    mut coverage: &str,
    hint: Option<String>,
) -> Envelope {
    let mut kept = Vec::new();
    let mut tokens = 0;
    for mut hit in hits {
        let mut cost = estimate_tokens(&hit);
        if !kept.is_empty() && tokens + cost > budget_tokens {
            coverage = "partial";
            break;
        }
        if cost > budget_tokens {
            let available = budget_tokens.saturating_mul(4).saturating_sub(160).max(80);
            truncate_string(&mut hit.snippet, available);
            cost = estimate_tokens(&hit).min(budget_tokens);
            coverage = "partial";
        }
        kept.push(hit);
        tokens += cost;
    }
    Envelope {
        hits: kept,
        tokens,
        freshness_ms: started.elapsed().as_millis(),
        coverage: coverage.to_owned(),
        hint,
    }
}

pub fn estimate_tokens(hit: &Hit) -> usize {
    serde_json::to_string(hit)
        .map(|value| value.chars().count().div_ceil(4).max(1))
        .unwrap_or(1)
}

fn truncate_string(value: &mut String, max_chars: usize) {
    if value.chars().count() > max_chars {
        *value = value.chars().take(max_chars).collect();
    }
}

fn fts_query(query: &str) -> String {
    let terms = Regex::new(r"[\p{L}\p{N}_]+")
        .expect("valid regex")
        .find_iter(query)
        .map(|term| format!(r#""{}""#, term.as_str().replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        r#""""#.to_owned()
    } else {
        terms.join(" OR ")
    }
}

fn rounded(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

pub fn repository_root(start: impl AsRef<Path>) -> Result<PathBuf> {
    let connection = connect(&find_ctx(start)?.join("index.sqlite"), false)?;
    Ok(PathBuf::from(get_meta(
        &connection,
        "repo_root",
        &std::env::current_dir()?.to_string_lossy(),
    )?))
}

pub fn indexed_sha(start: impl AsRef<Path>) -> Result<Option<String>> {
    let connection = connect(&find_ctx(start)?.join("index.sqlite"), false)?;
    connection
        .query_row(
            "SELECT value FROM meta WHERE key='indexed_sha'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}
