use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use regex::Regex;

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
        // Reserve space for every requested definition before spending the
        // budget on any one function body or its graph neighbours.
        let definitions_cost: usize = hits.iter().map(estimate_tokens).sum();
        let mut shortened = false;
        if definitions_cost > budget_tokens {
            let share = budget_tokens / hits.len().max(1);
            for hit in &mut hits {
                fit_snippet(hit, share);
                shortened |= hit.snippet_truncated;
            }
        }
        hits.extend(relations);
        let mut envelope = apply_budget(
            unique_hits(hits),
            budget_tokens,
            started,
            if incomplete { "partial" } else { "complete" },
            Some(
                "answer-ready: exact definitions, callers, and callees are included; cite each hit's path:start-end from why exactly, and do not call another retrieval tool unless a requested fact is absent"
                    .to_owned(),
            ),
        );
        if shortened || incomplete || envelope.coverage != "complete" {
            envelope.coverage = "partial".to_owned();
            envelope.hint = Some(
                "partial context: static relations are best-effort or evidence was omitted/shortened; verify any missing requested facts"
                    .to_owned(),
            );
        }
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
    let _ = intent;
    Ok(apply_budget(
        unique_hits(hits),
        budget_tokens,
        started,
        if incomplete {
            "partial"
        } else {
            &search.coverage
        },
        search.hint,
    ))
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
