use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::Result;

use crate::graph::graph_query;
use crate::model::{Envelope, Hit};
use crate::search::{apply_budget, search_index};

pub fn pack_query(
    query: &str,
    budget_tokens: usize,
    intent: &str,
    start: impl AsRef<Path>,
) -> Result<Envelope> {
    let started = Instant::now();
    let start = start.as_ref();
    let search = search_index(query, "auto", None, 20, budget_tokens.max(600), start)?;
    let mut hits = search.hits.clone();
    let symbols =
        hits.iter()
            .filter_map(|hit| hit.symbol.clone())
            .fold(Vec::new(), |mut values, name| {
                if values.len() < 3 && !values.contains(&name) {
                    values.push(name);
                }
                values
            });
    for name in symbols {
        for operation in ["def", "callers"] {
            hits.extend(graph_query(operation, &name, 1, 500, start)?.hits);
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
    let mut seen = HashSet::new();
    let unique = hits
        .into_iter()
        .filter_map(|mut hit| {
            if !seen.insert((hit.path.clone(), hit.start)) {
                return None;
            }
            hit.why = format!("{intent}: {}", hit.why);
            Some(hit)
        })
        .collect::<Vec<Hit>>();
    Ok(apply_budget(
        unique,
        budget_tokens,
        started,
        &search.coverage,
        search.hint,
    ))
}
