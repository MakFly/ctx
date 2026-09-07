use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::find_ctx;
use crate::db::{connect, get_meta};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub path: String,
    pub files: usize,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryPoint {
    pub path: String,
    pub line: usize,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub path: String,
    pub line: usize,
    pub route: String,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hub {
    pub path: String,
    pub in_edges: usize,
    pub pagerank: f64,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryMap {
    pub packages: Vec<Package>,
    pub entrypoints: Vec<EntryPoint>,
    pub router: Vec<Route>,
    pub hubs: Vec<Hub>,
}

pub fn build_map(start: impl AsRef<Path>) -> Result<RepositoryMap> {
    let connection = connect(&find_ctx(start)?.join("index.sqlite"), false)?;
    let root = PathBuf::from(get_meta(
        &connection,
        "repo_root",
        &std::env::current_dir()?.to_string_lossy(),
    )?);
    let mut statement = connection.prepare("SELECT id,path FROM files ORDER BY path")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let valid = rows
        .into_iter()
        .filter(|(_, path)| root.join(path).is_file())
        .collect::<Vec<_>>();

    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, path) in &valid {
        let top = path.split('/').next().unwrap_or(path);
        grouped
            .entry(top.to_owned())
            .or_default()
            .push(path.clone());
    }
    let packages = grouped
        .into_iter()
        .map(|(name, paths)| Package {
            path: if paths.len() == 1 {
                paths[0].clone()
            } else {
                name.clone()
            },
            evidence: format!("{}:1", paths[0]),
            files: paths.len(),
            name,
        })
        .collect();
    let entrypoints = valid
        .iter()
        .filter(|(_, path)| is_entrypoint(path))
        .map(|(_, path)| EntryPoint {
            path: path.clone(),
            line: 1,
            why: "entrypoint heuristic".to_owned(),
        })
        .collect();

    let mut links: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut incoming: HashMap<i64, usize> = HashMap::new();
    let valid_ids = valid.iter().map(|(id, _)| *id).collect::<HashSet<_>>();
    let mut edge_statement = connection.prepare(
        "SELECT DISTINCT e.file_id,s.file_id
         FROM edges e JOIN symbols s ON s.id=e.dst_symbol_id
         WHERE e.file_id != s.file_id",
    )?;
    for row in
        edge_statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
    {
        let (source, destination) = row?;
        if !valid_ids.contains(&source) || !valid_ids.contains(&destination) {
            continue;
        }
        if links.entry(source).or_default().insert(destination) {
            *incoming.entry(destination).or_default() += 1;
        }
    }
    let nodes = valid.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let ranks = pagerank(&nodes, &links, 20);
    let by_id = valid.iter().cloned().collect::<HashMap<_, _>>();
    let baseline = 1.0 / valid.len().max(1) as f64;
    let mut ranked = ranks.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| by_id[&left.0].cmp(&by_id[&right.0]))
    });
    let hubs = ranked
        .into_iter()
        .filter(|(id, rank)| incoming.get(id).copied().unwrap_or(0) > 0 || *rank > baseline)
        .take(10)
        .map(|(id, rank)| Hub {
            path: by_id[&id].clone(),
            in_edges: incoming.get(&id).copied().unwrap_or(0),
            pagerank: (rank * 100_000.0).round() / 100_000.0,
            evidence: format!("{}:1", by_id[&id]),
        })
        .collect();
    let paths = valid
        .iter()
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    Ok(RepositoryMap {
        packages,
        entrypoints,
        router: routes(&root, &paths)?,
        hubs,
    })
}

fn is_entrypoint(path: &str) -> bool {
    const BASENAMES: &[&str] = &[
        "main.py",
        "app.py",
        "manage.py",
        "wsgi.py",
        "asgi.py",
        "server.ts",
        "server.tsx",
        "server.js",
        "app.ts",
        "app.tsx",
        "app.js",
        "main.go",
        "main.rs",
        "index.php",
        "artisan",
    ];
    const PATHS: &[&str] = &[
        "src/index.ts",
        "src/index.tsx",
        "src/index.js",
        "src/main.rs",
        "public/index.php",
        "bin/console",
    ];
    let basename = path.rsplit('/').next().unwrap_or(path);
    BASENAMES.contains(&basename)
        || PATHS.contains(&path)
        || path.starts_with("cmd/")
        || path.starts_with("bin/")
}

fn pagerank(nodes: &[i64], links: &HashMap<i64, HashSet<i64>>, rounds: usize) -> HashMap<i64, f64> {
    if nodes.is_empty() {
        return HashMap::new();
    }
    let damping = 0.85;
    let count = nodes.len() as f64;
    let mut rank = nodes
        .iter()
        .map(|node| (*node, 1.0 / count))
        .collect::<HashMap<_, _>>();
    for _ in 0..rounds {
        let mut next = nodes
            .iter()
            .map(|node| (*node, (1.0 - damping) / count))
            .collect::<HashMap<_, _>>();
        for source in nodes {
            let targets = links.get(source);
            if let Some(targets) = targets.filter(|targets| !targets.is_empty()) {
                for destination in targets {
                    *next.entry(*destination).or_default() +=
                        damping * rank[source] / targets.len() as f64;
                }
            } else {
                for destination in nodes {
                    *next.entry(*destination).or_default() += damping * rank[source] / count;
                }
            }
        }
        rank = next;
    }
    rank
}

fn routes(root: &Path, paths: &[String]) -> Result<Vec<Route>> {
    let generic = Regex::new(
        r#"(?i)(?:@\w+\.|\.|->)(get|post|put|delete|patch|options|head)\s*\(\s*['"]([^'"]+)"#,
    )?;
    let laravel =
        Regex::new(r#"(?i)Route::(get|post|put|delete|patch|options|any)\s*\(\s*['"]([^'"]+)"#)?;
    let decorator = Regex::new(
        r#"(?i)(?:@(Get|Post|Put|Delete|Patch|Options|Head)|#\[(get|post|put|delete|patch))\s*\(\s*['"]([^'"]+)"#,
    )?;
    let django = Regex::new(r#"(?i)(?:path|re_path)\s*\(\s*['"]([^'"]+)"#)?;
    let net_http = Regex::new(r#"(?i)(?:Handle|HandleFunc)\s*\(\s*['"]([^'"]+)"#)?;
    let axum = Regex::new(
        r#"(?i)\.route\s*\(\s*['"]([^'"]+)['"]\s*,\s*(get|post|put|delete|patch)\s*\("#,
    )?;
    let symfony =
        Regex::new(r#"(?is)#\[Route\s*\(\s*['"]([^'"]+).{0,200}?methods\s*:\s*\[\s*['"]([A-Z]+)"#)?;
    let next_method = Regex::new(
        r#"(?m)^\s*export\s+(?:async\s+)?function\s+(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\b"#,
    )?;
    let mut output = Vec::new();
    for relative in paths {
        if crate::config::language(Path::new(relative)).is_none() {
            continue;
        }
        let text = fs::read_to_string(root.join(relative)).unwrap_or_default();
        for capture in generic.captures_iter(&text) {
            push_route(&mut output, relative, &text, &capture, 1, 2);
        }
        for capture in laravel.captures_iter(&text) {
            push_route(&mut output, relative, &text, &capture, 1, 2);
        }
        for capture in decorator.captures_iter(&text) {
            output.push(Route {
                path: relative.clone(),
                line: line_number(
                    &text,
                    capture.get(0).map(|value| value.start()).unwrap_or(0),
                ),
                route: capture[3].to_owned(),
                method: capture
                    .get(1)
                    .or_else(|| capture.get(2))
                    .map(|value| value.as_str().to_ascii_uppercase())
                    .unwrap_or_else(|| "ANY".to_owned()),
            });
        }
        for capture in django.captures_iter(&text) {
            push_fixed_route(&mut output, relative, &text, &capture, "ANY", 1);
        }
        for capture in net_http.captures_iter(&text) {
            push_fixed_route(&mut output, relative, &text, &capture, "ANY", 1);
        }
        for capture in axum.captures_iter(&text) {
            push_route(&mut output, relative, &text, &capture, 2, 1);
        }
        for capture in symfony.captures_iter(&text) {
            push_route(&mut output, relative, &text, &capture, 2, 1);
        }
        if is_next_route(relative) {
            let route = next_route(relative);
            for capture in next_method.captures_iter(&text) {
                output.push(Route {
                    path: relative.clone(),
                    line: line_number(
                        &text,
                        capture.get(0).map(|value| value.start()).unwrap_or(0),
                    ),
                    route: route.clone(),
                    method: capture[1].to_ascii_uppercase(),
                });
            }
        }
    }
    output.sort_by(|left, right| {
        (&left.path, left.line, &left.method, &left.route).cmp(&(
            &right.path,
            right.line,
            &right.method,
            &right.route,
        ))
    });
    output.dedup_by(|left, right| {
        left.path == right.path
            && left.line == right.line
            && left.method == right.method
            && left.route == right.route
    });
    output.truncate(50);
    Ok(output)
}

fn push_route(
    output: &mut Vec<Route>,
    path: &str,
    text: &str,
    capture: &regex::Captures<'_>,
    method_index: usize,
    route_index: usize,
) {
    output.push(Route {
        path: path.to_owned(),
        line: line_number(text, capture.get(0).map(|value| value.start()).unwrap_or(0)),
        route: capture[route_index].to_owned(),
        method: capture[method_index].to_ascii_uppercase(),
    });
}

fn push_fixed_route(
    output: &mut Vec<Route>,
    path: &str,
    text: &str,
    capture: &regex::Captures<'_>,
    method: &str,
    route_index: usize,
) {
    output.push(Route {
        path: path.to_owned(),
        line: line_number(text, capture.get(0).map(|value| value.start()).unwrap_or(0)),
        route: capture[route_index].to_owned(),
        method: method.to_owned(),
    });
}

fn line_number(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn is_next_route(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or_default();
    matches!(
        filename,
        "route.ts" | "route.tsx" | "route.js" | "route.jsx"
    ) && path.split('/').any(|part| part == "app")
}

fn next_route(path: &str) -> String {
    let parts = path.split('/').collect::<Vec<_>>();
    let Some(app_index) = parts.iter().position(|part| *part == "app") else {
        return "/".to_owned();
    };
    let segments = parts[app_index + 1..parts.len().saturating_sub(1)]
        .iter()
        .filter(|segment| !(segment.starts_with('(') && segment.ends_with(')')))
        .map(|segment| {
            if segment.starts_with('[') && segment.ends_with(']') {
                format!(":{}", &segment[1..segment.len() - 1])
            } else {
                (*segment).to_owned()
            }
        })
        .collect::<Vec<_>>();
    format!("/{}", segments.join("/"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::routes;

    #[test]
    fn detects_framework_decorators_and_attributes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("controller.ts"),
            "@Controller()\nclass LoginController {\n  @Post('/login')\n  login() {}\n}\n",
        )
        .unwrap();
        fs::write(
            root.path().join("route.rs"),
            "#[get(\"/health\")]\nfn health() {}\n",
        )
        .unwrap();
        let found = routes(
            root.path(),
            &["controller.ts".to_owned(), "route.rs".to_owned()],
        )
        .unwrap();
        assert!(
            found
                .iter()
                .any(|route| route.method == "POST" && route.route == "/login")
        );
        assert!(
            found
                .iter()
                .any(|route| route.method == "GET" && route.route == "/health")
        );
    }
}
