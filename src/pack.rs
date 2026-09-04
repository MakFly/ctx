use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use regex::Regex;

use crate::graph::graph_query;
use crate::model::{Envelope, Hit};
use crate::search::{apply_budget, estimate_tokens, search_index};

pub fn pack_query(
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
        for (name, mut definitions) in exact {
            for hit in &mut definitions {
                set_why(hit, "definition", &name);
            }
            hits.extend(definitions);
            for operation in ["callers", "callees"] {
                let mut related = graph_query(operation, &name, 1, 500, start)?.hits;
                for hit in &mut related {
                    set_why(hit, relation_name(operation), &name);
                }
                relations.extend(related);
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
                if estimate_tokens(hit) > share && !hit.snippet.is_empty() {
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
                        if estimate_tokens(hit) <= share {
                            low = middle;
                        } else {
                            high = middle - 1;
                        }
                    }
                    hit.snippet = original[..boundaries[low]].to_owned();
                    shortened |= hit.snippet.len() < original.len();
                }
            }
        }
        hits.extend(relations);
        let mut envelope = apply_budget(
            unique_hits(hits),
            budget_tokens,
            started,
            "complete",
            Some(
                "answer-ready: exact definitions, callers, and callees are included; cite each hit's path:start-end from why exactly, and do not call another retrieval tool unless a requested fact is absent"
                    .to_owned(),
            ),
        );
        if shortened || envelope.coverage != "complete" {
            envelope.coverage = "partial".to_owned();
            envelope.hint = Some(
                "partial context: budget omitted or shortened evidence; cite the returned spans and retrieve any missing requested facts"
                    .to_owned(),
            );
        }
        return Ok(envelope);
    }

    let search = search_index(query, "auto", None, 10, budget_tokens.max(600), start)?;
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
            let mut related = graph_query(operation, &name, 1, 500, start)?.hits;
            for hit in &mut related {
                let relation = if operation == "def" {
                    "definition"
                } else {
                    relation_name(operation)
                };
                set_why(hit, relation, &name);
            }
            hits.extend(related);
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
        &search.coverage,
        search.hint,
    ))
}

fn exact_symbols(query: &str, start: &Path) -> Result<Vec<(String, Vec<Hit>)>> {
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
        let name = definitions.hits[0]
            .symbol
            .clone()
            .unwrap_or_else(|| value.to_owned());
        if matches
            .iter()
            .any(|(existing, _): &(String, Vec<Hit>)| existing == &name)
        {
            continue;
        }
        matches.push((name, definitions.hits));
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
