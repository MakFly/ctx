use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use regex::Regex;
use rusqlite::{OptionalExtension, params};

use crate::config::find_ctx;
use crate::db::{connect, get_meta};
use crate::graph::graph_query;
use crate::model::{Envelope, Hit};
use crate::search::{apply_budget, estimate_tokens, fit_snippet, search_index};

pub fn pack_query(
    query: &str,
    budget_tokens: usize,
    intent: &str,
    start: impl AsRef<Path>,
) -> Result<Envelope> {
    let start = start.as_ref();
    let generation = crate::watcher::generation(start);
    let result = pack_query_inner(query, budget_tokens, intent, start)?;
    Ok(crate::watcher::check_coverage(start, generation, result))
}

fn pack_query_inner(
    query: &str,
    budget_tokens: usize,
    intent: &str,
    start: impl AsRef<Path>,
) -> Result<Envelope> {
    let started = Instant::now();
    let start = start.as_ref();
    if let Some((path, span_start, span_end)) = parse_expand(query) {
        return expand_span(&path, span_start, span_end, start, started);
    }
    let explore = intent != "edit";
    let exact = exact_symbols(query, start)?;
    if !exact.is_empty() {
        let mut hits = Vec::new();
        let mut relations = Vec::new();
        let mut incomplete = false;
        for (name, mut definitions) in exact {
            incomplete |= definitions.coverage != "complete";
            for hit in &mut definitions.hits {
                set_why(hit, "definition", &name);
            }
            hits.extend(definitions.hits);
            for operation in ["callers", "callees"] {
                let mut related = graph_query(operation, &name, 1, 500, start)?;
                incomplete |= related.coverage != "complete";
                for hit in &mut related.hits {
                    set_why(hit, relation_name(operation), &name);
                }
                relations.extend(related.hits);
            }
        }
        let mut hits = unique_hits(hits);
        let mut shortened = false;
        if explore {
            for hit in &mut hits {
                shortened |= strip_to_signature(hit);
            }
        } else {
            let definitions_cost: usize = hits.iter().map(estimate_tokens).sum();
            if definitions_cost > budget_tokens {
                let share = budget_tokens / hits.len().max(1);
                for hit in &mut hits {
                    fit_snippet(hit, share);
                    shortened |= hit.snippet_truncated;
                }
            }
        }
        hits.extend(relations);
        let mut hits = unique_hits(hits);
        if explore {
            for hit in &mut hits {
                if !is_definition(hit) {
                    digest_hit(hit);
                }
            }
        }
        let mut envelope = apply_budget(
            hits,
            budget_tokens,
            started,
            if incomplete { "partial" } else { "complete" },
            None,
        );
        attach_expand_hint(&mut envelope, shortened || incomplete);
        return Ok(envelope);
    }

    let search = search_index(query, "auto", None, 10, budget_tokens.max(600), start)?;
    let mut incomplete = search.coverage != "complete";
    let mut hits = search.hits.clone();
    let symbols =
        hits.iter()
            .filter_map(|hit| hit.symbol.clone())
            .fold(Vec::new(), |mut values, name| {
                if values.len() < 2 && !values.contains(&name) {
                    values.push(name);
                }
                values
            });
    for name in symbols {
        for operation in ["def", "callers", "callees"] {
            let mut related = graph_query(operation, &name, 1, 500, start)?;
            incomplete |= related.coverage != "complete";
            for hit in &mut related.hits {
                let relation = if operation == "def" {
                    "definition"
                } else {
                    relation_name(operation)
                };
                set_why(hit, relation, &name);
            }
            hits.extend(related.hits);
        }
    }
    let priority = |kind: &str| match kind {
        "def" => 0,
        "call" | "ref" => 1,
        "test" => 2,
        "doc" | "config" => 3,
        _ => 4,
    };
    let mut file_rank: HashMap<String, (usize, i64, String)> = HashMap::new();
    for hit in &hits {
        let candidate = (
            priority(&hit.kind),
            -(hit.score * 1_000_000.0) as i64,
            hit.path.clone(),
        );
        file_rank
            .entry(hit.path.clone())
            .and_modify(|current| {
                if candidate < *current {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    let mut paths = file_rank.keys().cloned().collect::<Vec<_>>();
    paths.sort_by_key(|path| file_rank.get(path).cloned());
    let positions = paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| (path, index))
        .collect::<HashMap<_, _>>();
    hits.sort_by(|left, right| {
        positions[&left.path]
            .cmp(&positions[&right.path])
            .then_with(|| priority(&left.kind).cmp(&priority(&right.kind)))
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.start.cmp(&right.start))
    });
    let mut hits = unique_hits(hits);
    if explore {
        for hit in &mut hits {
            if is_definition(hit) {
                strip_to_signature(hit);
            } else {
                digest_hit(hit);
            }
        }
    }
    let mut envelope = apply_budget(
        hits,
        budget_tokens,
        started,
        if incomplete {
            "partial"
        } else {
            &search.coverage
        },
        search.hint,
    );
    attach_expand_hint(&mut envelope, explore || incomplete);
    Ok(envelope)
}

fn parse_expand(query: &str) -> Option<(String, usize, usize)> {
    let pattern = Regex::new(r"expand\s+(\S+):(\d+)(?:-(\d+))?").ok()?;
    let captured = pattern.captures(query)?;
    let path = captured.get(1)?.as_str().trim_matches('"').to_owned();
    let start: usize = captured.get(2)?.as_str().parse().ok()?;
    let end = captured
        .get(3)
        .and_then(|value| value.as_str().parse().ok())
        .unwrap_or(start)
        .max(start);
    if path.is_empty() {
        None
    } else {
        Some((path, start, end))
    }
}

fn expand_span(
    path: &str,
    start: usize,
    end: usize,
    repo: &Path,
    started: Instant,
) -> Result<Envelope> {
    let connection = connect(&find_ctx(repo)?.join("index.sqlite"), false)?;
    let root = PathBuf::from(get_meta(
        &connection,
        "repo_root",
        &repo.to_string_lossy(),
    )?);
    let indexed: Option<(String, String, String, bool)> = connection
        .prepare(
            "SELECT s.name, COALESCE(s.sig,''), COALESCE(s.snippet,''), f.is_test
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE f.path=?1 AND s.start=?2 AND s.end=?3
             ORDER BY s.id LIMIT 1",
        )
        .and_then(|mut statement| {
            statement
                .query_row(params![path, start as i64, end as i64], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .optional()
        })?;
    let file_snippet = fs::read_to_string(root.join(path)).ok().map(|text| {
        let lines = text.lines().collect::<Vec<_>>();
        let from = start.saturating_sub(1).min(lines.len());
        let to = end.min(lines.len()).max(from);
        lines[from..to].join("\n")
    });
    let (symbol, sig, fallback, is_test) = match indexed {
        Some((name, sig, snippet, is_test)) => (Some(name), sig, snippet, is_test),
        None => (None, String::new(), String::new(), false),
    };
    let snippet = file_snippet.unwrap_or(fallback);
    let hit = Hit {
        path: path.to_owned(),
        start,
        end,
        symbol,
        kind: if is_test {
            "test".to_owned()
        } else {
            "def".to_owned()
        },
        sig,
        snippet,
        snippet_truncated: false,
        score: 1.0,
        why: format!("expanded {path}:{start}-{end}"),
    };
    Ok(Envelope {
        tokens: estimate_tokens(&hit),
        hits: vec![hit],
        freshness_ms: started.elapsed().as_millis(),
        coverage: "complete".to_owned(),
        hint: None,
    })
}

fn is_definition(hit: &Hit) -> bool {
    matches!(hit.kind.as_str(), "def" | "test")
}

fn strip_to_signature(hit: &mut Hit) -> bool {
    if hit.sig.is_empty() {
        return hit.snippet_truncated;
    }
    let longer = hit.snippet.chars().count() > hit.sig.chars().count()
        || (hit.snippet_truncated && !hit.snippet.is_empty());
    hit.snippet.clear();
    hit.snippet_truncated = longer;
    longer
}

fn digest_hit(hit: &mut Hit) {
    hit.snippet.clear();
}

fn expand_invocation(hit: &Hit) -> String {
    if hit.start == hit.end {
        format!(r#"ctx_pack query="expand {}:{}""#, hit.path, hit.start)
    } else {
        format!(
            r#"ctx_pack query="expand {}:{start}-{end}""#,
            hit.path,
            start = hit.start,
            end = hit.end
        )
    }
}

fn attach_expand_hint(envelope: &mut Envelope, force_partial: bool) {
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for hit in &envelope.hits {
        let omitted = hit.snippet_truncated
            || (is_definition(hit) && hit.snippet.is_empty() && !hit.sig.is_empty());
        if !omitted {
            continue;
        }
        let invocation = expand_invocation(hit);
        if seen.insert(invocation.clone()) {
            lines.push(invocation);
        }
    }
    if lines.is_empty() {
        if force_partial {
            envelope.coverage = "partial".to_owned();
        }
        return;
    }
    envelope.coverage = "partial".to_owned();
    envelope.hint = Some(lines.join("\n"));
}

fn exact_symbols(query: &str, start: &Path) -> Result<Vec<(String, Envelope)>> {
    let identifier = Regex::new(r"[A-Za-z_][A-Za-z0-9_.]*")?;
    let mut candidates = HashSet::new();
    let mut matches = Vec::new();
    for value in identifier
        .find_iter(query)
        .map(|item| item.as_str().trim_matches('.'))
        .filter(|value| !value.is_empty())
    {
        if candidates.len() >= 24 || !candidates.insert(value.to_owned()) {
            continue;
        }
        let definitions = graph_query("def", value, 1, 500, start)?;
        if definitions.hits.is_empty() {
            continue;
        }
        matches.push((value.to_owned(), definitions));
        if matches.len() >= 6 {
            break;
        }
    }
    Ok(matches)
}

fn relation_name(operation: &str) -> &'static str {
    if operation == "callers" {
        "caller"
    } else {
        "callee"
    }
}

fn set_why(hit: &mut Hit, relation: &str, name: &str) {
    let citation = if hit.start == hit.end {
        format!("{}:{}", hit.path, hit.start)
    } else {
        format!("{}:{}-{}", hit.path, hit.start, hit.end)
    };
    hit.why = format!("{relation} of {name}; cite {citation} exactly");
}

fn unique_hits(hits: Vec<Hit>) -> Vec<Hit> {
    let mut seen = HashSet::new();
    hits.into_iter()
        .filter(|hit| {
            seen.insert((
                hit.path.clone(),
                hit.start,
                hit.end,
                hit.kind.clone(),
                hit.symbol.clone(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_expand;

    #[test]
    fn parse_expand_accepts_hint_invocation() {
        let (path, start, end) = parse_expand(r#"ctx_pack query="expand long.py:1-12""#).unwrap();
        assert_eq!((path.as_str(), start, end), ("long.py", 1, 12));
        let (path, start, end) = parse_expand("expand auth.py:42").unwrap();
        assert_eq!((path.as_str(), start, end), ("auth.py", 42, 42));
        assert!(parse_expand("login retry_payment").is_none());
    }
}
