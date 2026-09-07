use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
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
    snippet_truncated: bool,
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
    search_index_with_options(query, mode, false, path_filter, limit, budget_tokens, start)
}

pub fn search_index_with_options(
    query: &str,
    mode: &str,
    ignore_case: bool,
    path_filter: Option<&str>,
    limit: usize,
    budget_tokens: usize,
    start: impl AsRef<Path>,
) -> Result<Envelope> {
    let start = start.as_ref();
    if matches!(mode, "literal" | "regex") {
        return crate::text_index::exact_search(
            query,
            mode,
            ignore_case,
            path_filter,
            limit.max(1),
            budget_tokens,
            start,
        );
    }
    if !matches!(mode, "auto" | "text" | "symbol") {
        bail!("unknown search mode: {mode}");
    }
    if ignore_case {
        bail!(
            "ignore_case is only valid for literal and regex modes; ranked search already folds case"
        );
    }
    let generation = crate::watcher::generation(start);
    let result = ranked_search(query, mode, path_filter, limit.max(1), budget_tokens, start)?;
    Ok(crate::watcher::check_coverage(start, generation, result))
}

fn ranked_search(
    query: &str,
    mode: &str,
    path_filter: Option<&str>,
    limit: usize,
    budget_tokens: usize,
    start: impl AsRef<Path>,
) -> Result<Envelope> {
    let result_limit = limit;
    let limit = limit.saturating_add(1);
    let started = Instant::now();
    let database = find_ctx(start)?.join("index.sqlite");
    let connection = connect(&database, false)?;
    connection.execute_batch("BEGIN DEFERRED")?;
    let identifier = Regex::new(r"^[A-Za-z_][A-Za-z0-9_.]*$")?.is_match(query);
    let symbol_first = mode == "symbol" || (mode == "auto" && identifier);
    let path_pattern = path_filter.map(|value| format!("%{value}%"));
    let mut candidates: Vec<(SymbolRow, f64, String)> = Vec::new();

    if symbol_first {
        let mut statement = connection.prepare(
            "SELECT s.id,f.path,s.start,s.end,s.name,COALESCE(s.sig,''),
                    COALESCE(s.snippet,''),f.is_test,f.is_vendor,s.snippet_truncated
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
                COALESCE(s.snippet,''),f.is_test,f.is_vendor,s.snippet_truncated,bm25(symbols_fts,5.0,3.0,1.0)
         FROM symbols_fts
         JOIN symbols s ON s.id=symbols_fts.rowid
         JOIN files f ON f.id=s.file_id
         WHERE symbols_fts MATCH ?1
           AND (?2 IS NULL OR f.path LIKE ?2)
         ORDER BY bm25(symbols_fts,5.0,3.0,1.0),f.is_test,f.is_vendor,f.path,s.start
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![
            fts_query,
            path_pattern,
            limit.saturating_mul(3).min(i64::MAX as usize) as i64
        ],
        |row| Ok((map_symbol_row(row)?, row.get::<_, f64>(10)?)),
    )?;
    for row in rows {
        let (row, rank) = row?;
        candidates.push((
            row,
            0.85 - 0.75 / (1.0 + rank.abs()),
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
    let omitted = hits.len() > result_limit;
    hits.truncate(result_limit);
    let has_structural: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE lang IS NOT NULL)",
        [],
        |row| row.get(0),
    )?;
    Ok(apply_budget(
        hits,
        budget_tokens,
        started,
        if omitted {
            "partial"
        } else if has_structural {
            "complete"
        } else {
            "text_only"
        },
        omitted.then(|| "additional ranked results omitted by the result limit".to_owned()),
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
        "SELECT f.path, c.content, highlight(content_fts,0,char(1),char(2)), f.is_test, f.lang IS NULL
         FROM content_fts JOIN files f ON f.id=content_fts.rowid
         JOIN file_contents c ON c.file_id=f.id
         WHERE content_fts MATCH ?1 AND (?2 IS NULL OR f.path LIKE ?2)
         ORDER BY bm25(content_fts),f.is_test,f.path LIMIT ?3"
    )?;
    let mut rows = statement.query(params![fts_query(query), path_filter, limit as i64])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let content: String = row.get(1)?;
        let highlighted: String = row.get(2)?;
        // The source can contain SOH: it is text under our admission policy.
        // In that case the first SOH is not necessarily an FTS insertion.
        let offset = if content.contains('\u{1}') {
            let mut marker = "\u{1}ctx-match\u{1}".to_owned();
            while content.contains(&marker) {
                marker.push('\u{1}');
            }
            let marked: String = connection.query_row(
                "SELECT highlight(content_fts,0,?2,'') FROM content_fts
                 WHERE content_fts MATCH ?1
                   AND rowid=(SELECT id FROM files WHERE path=?3)",
                params![fts_query(query), marker, path],
                |row| row.get(0),
            )?;
            marked.find(&marker)
        } else {
            highlighted.find('\u{1}')
        }
        .unwrap_or(0)
        .min(content.len());
        let mut offset = offset;
        while !content.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = content[..offset]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let lines = content.lines().collect::<Vec<_>>();
        let start = line.saturating_sub(2);
        let end = (line + 3).min(lines.len());
        if !hits
            .iter()
            .any(|hit| hit.path == path && hit.start <= line + 1 && hit.end > line)
        {
            hits.push(Hit {
                path,
                start: start + 1,
                end: end.max(start + 1),
                symbol: None,
                kind: if row.get::<_, bool>(3)? {
                    "test"
                } else {
                    "text"
                }
                .into(),
                sig: String::new(),
                snippet: lines.get(start..end).unwrap_or_default().join("\n"),
                snippet_truncated: false,
                score: 0.55,
                why: if row.get::<_, bool>(4)? {
                    "full-content match; no structural analysis"
                } else {
                    "full-content match"
                }
                .into(),
            });
        }
        if hits.len() >= limit {
            break;
        }
    }
    if hits.len() < limit {
        append_excerpt_hits(connection, query, path_filter, limit, hits)?;
    }
    Ok(())
}

fn append_excerpt_hits(
    connection: &Connection,
    query: &str,
    path_filter: Option<&str>,
    limit: usize,
    hits: &mut Vec<Hit>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT f.path,f.is_test,COALESCE(x.excerpt,''),bm25(files_fts),
                f.size > length(CAST(COALESCE(x.excerpt,'') AS BLOB))
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
                row.get::<_, bool>(4)?,
            ))
        },
    )?;
    let mut seen = hits
        .iter()
        .map(|hit| (hit.path.clone(), hit.start, hit.kind.clone()))
        .collect::<HashSet<_>>();
    for row in rows {
        let (path, is_test, snippet, rank, snippet_truncated) = row?;
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
                snippet_truncated,
                score: 0.55 - 0.47 / (1.0 + rank.abs()),
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
        snippet_truncated: row.get(9)?,
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
        snippet_truncated: row.snippet_truncated,
        score: rounded(score),
        why,
    }
}

pub fn apply_budget(
    hits: Vec<Hit>,
    budget_tokens: usize,
    started: Instant,
    coverage: &str,
    hint: Option<String>,
) -> Envelope {
    let mut accumulator = EvidenceBudget::new(budget_tokens);
    for hit in hits {
        if !accumulator.push(hit) {
            break;
        }
    }
    accumulator.finish(started, coverage, hint)
}

/// Incremental equivalent of apply_budget. Callers may keep inspecting matches
/// after closure to preserve limit/error reporting without constructing snippets.
pub(crate) struct EvidenceBudget {
    kept: Vec<Hit>,
    tokens: usize,
    budget: usize,
    hint: Option<&'static str>,
    closed: bool,
}
impl EvidenceBudget {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            kept: Vec::new(),
            tokens: 0,
            budget,
            hint: None,
            closed: false,
        }
    }
    pub(crate) fn closed(&self) -> bool {
        self.closed
    }
    pub(crate) fn push(&mut self, mut hit: Hit) -> bool {
        if self.closed {
            return false;
        }
        let mut cost = estimate_tokens(&hit);
        if hit.snippet_truncated {
            self.hint = Some("partial evidence: source excerpts are truncated");
        }
        if !self.kept.is_empty() && cost > self.budget.saturating_sub(self.tokens) {
            self.hint = Some("partial evidence: token budget omitted remaining hits");
            self.closed = true;
            return false;
        }
        if cost > self.budget {
            fit_snippet(&mut hit, self.budget);
            cost = estimate_tokens(&hit);
            self.hint = Some("partial evidence: token budget shortened or omitted source");
            if cost > self.budget {
                self.closed = true;
                return false;
            }
        }
        self.kept.push(hit);
        self.tokens += cost;
        true
    }
    pub(crate) fn finish(self, started: Instant, coverage: &str, hint: Option<String>) -> Envelope {
        Envelope {
            hits: self.kept,
            tokens: self.tokens,
            freshness_ms: started.elapsed().as_millis(),
            coverage: if self.hint.is_some() {
                "partial"
            } else {
                coverage
            }
            .to_owned(),
            hint: self.hint.map(str::to_owned).or(hint),
        }
    }
}

pub fn estimate_tokens(hit: &Hit) -> usize {
    serde_json::to_string(hit)
        .map(|value| value.chars().count().div_ceil(4).max(1))
        .unwrap_or(1)
}

pub(crate) fn fit_snippet(hit: &mut Hit, budget: usize) {
    if estimate_tokens(hit) <= budget || hit.snippet.is_empty() {
        return;
    }
    hit.snippet_truncated = true;
    let original = std::mem::take(&mut hit.snippet);
    let boundaries = original
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(original.len()))
        .collect::<Vec<_>>();
    let (mut low, mut high) = (0, boundaries.len() - 1);
    while low < high {
        let middle = (low + high).div_ceil(2);
        hit.snippet = original[..boundaries[middle]].to_owned();
        if estimate_tokens(hit) <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    hit.snippet = original[..boundaries[low]].to_owned();
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
