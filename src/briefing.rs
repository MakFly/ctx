use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::gitinfo::git_info;
use crate::map::{RepositoryMap, build_map};
use crate::pack::pack_query;

pub fn generate_briefing(
    root: &Path,
    intent: &str,
    focus: Option<&str>,
    harness: &str,
    out: &Path,
    force: bool,
) -> Result<(Value, bool)> {
    let started = Instant::now();
    let info = git_info(root);
    let json_path = out.join("briefing.json");
    if json_path.is_file()
        && !force
        && !info.dirty
        && let Ok(mut previous) = read_json(&json_path)
        && previous.get("sha").and_then(Value::as_str) == Some(&info.sha)
    {
        previous["skipped"] = Value::Bool(true);
        return Ok((previous, true));
    }
    let repository_map = build_map(root)?;
    let packed = match focus {
        Some(query) => pack_query(query, 2_000, "explore", root)?,
        None => crate::model::Envelope::empty(
            "complete",
            Some("Ajoutez --focus pour obtenir des preuves ciblées.".to_owned()),
        ),
    };
    let hits = packed
        .hits
        .into_iter()
        .filter(|hit| root.join(&hit.path).is_file())
        .collect::<Vec<_>>();
    let stack = detect_stack(root)?;
    let identity_evidence = identity_evidence(root, &repository_map);
    let hotspots = repository_map
        .hubs
        .iter()
        .filter(|hub| root.join(&hub.path).is_file())
        .take(7)
        .map(|hub| {
            json!({
                "path": hub.path,
                "why": format!("hub avec {} liens entrants", hub.in_edges),
                "risk": "couplage"
            })
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let touch = hits
        .iter()
        .filter(|hit| matches!(hit.kind.as_str(), "def" | "call"))
        .filter(|hit| seen.insert(hit.path.clone()))
        .map(|hit| hit.path.clone())
        .take(10)
        .collect::<Vec<_>>();
    let mut seen_tests = HashSet::new();
    let tests = hits
        .iter()
        .filter(|hit| hit.kind == "test" || hit.path.to_ascii_lowercase().contains("test"))
        .filter(|hit| seen_tests.insert(hit.path.clone()))
        .map(|hit| hit.path.clone())
        .collect::<Vec<_>>();
    let mut data = json!({
        "schema": "ctx.briefing.v1",
        "repo": root.file_name().and_then(|value| value.to_str()).unwrap_or("repo"),
        "sha": info.sha,
        "dirty": info.dirty,
        "intent": intent,
        "focus": focus,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "freshness_ms": started.elapsed().as_millis(),
        "harness": harness,
        "identity": {
            "one_liner": one_liner(root, &stack)?,
            "stack": stack,
            "evidence": identity_evidence,
        },
        "map": repository_map,
        "flows": [],
        "contracts": {
            "http": repository_map.router,
            "events": [],
            "tables": [],
        },
        "hotspots": hotspots,
        "change_plan": {
            "touch": touch,
            "avoid": [],
            "tests": tests,
            "blast_radius_files": touch.len(),
            "depth": 2,
        },
        "hits": hits,
        "coverage": packed.coverage,
        "hint": packed.hint,
    });
    data["freshness_ms"] = json!(started.elapsed().as_millis());
    fs::create_dir_all(out)?;
    fs::write(
        out.join("map.json"),
        format!("{}\n", serde_json::to_string_pretty(&repository_map)?),
    )?;
    fs::write(
        &json_path,
        format!("{}\n", serde_json::to_string_pretty(&data)?),
    )?;
    fs::write(out.join("briefing.md"), render_markdown(&data)?)?;
    Ok((data, false))
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_str(
        &fs::read_to_string(path)
            .with_context(|| format!("lecture impossible: {}", path.display()))?,
    )
    .with_context(|| format!("JSON invalide: {}", path.display()))
}

pub fn detect_stack(root: &Path) -> Result<Vec<String>> {
    let manifests = [
        ("pyproject.toml", "Python"),
        ("requirements.txt", "Python"),
        ("package.json", "JavaScript/TypeScript"),
        ("Cargo.toml", "Rust"),
        ("go.mod", "Go"),
        ("composer.json", "PHP"),
    ];
    let mut stack = Vec::new();
    let mut manifest_text = String::new();
    for (name, label) in manifests {
        let path = root.join(name);
        if path.is_file() {
            push_unique(&mut stack, label);
            manifest_text.push_str(
                &fs::read_to_string(path)
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            );
            manifest_text.push('\n');
        }
    }
    for entry in walkdir::WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        match entry.path().extension().and_then(|value| value.to_str()) {
            Some("py" | "pyi" | "pyw") => push_unique(&mut stack, "Python"),
            Some("js" | "jsx" | "ts" | "tsx") => push_unique(&mut stack, "JavaScript/TypeScript"),
            Some("go") => push_unique(&mut stack, "Go"),
            Some("rs") => push_unique(&mut stack, "Rust"),
            Some("php" | "phtml") => push_unique(&mut stack, "PHP"),
            _ => {}
        }
    }
    let frameworks = [
        ("FastAPI", &["fastapi"][..]),
        ("Django", &["django"][..]),
        ("Flask", &["flask"][..]),
        ("Litestar", &["litestar"][..]),
        ("Next.js", &["\"next\""][..]),
        ("Express", &["\"express\""][..]),
        ("NestJS", &["@nestjs/"][..]),
        ("Fastify", &["\"fastify\""][..]),
        ("Hono", &["\"hono\""][..]),
        ("Vue", &["\"vue\""][..]),
        ("Svelte", &["\"svelte\""][..]),
        ("Gin", &["gin-gonic/gin"][..]),
        ("Echo", &["labstack/echo"][..]),
        ("Fiber", &["gofiber/fiber"][..]),
        ("Chi", &["go-chi/chi"][..]),
        ("Axum", &["axum"][..]),
        ("Actix Web", &["actix-web"][..]),
        ("Rocket", &["rocket"][..]),
        ("Laravel", &["laravel/framework"][..]),
        ("Symfony", &["symfony/framework-bundle"][..]),
        ("Slim", &["slim/slim"][..]),
    ];
    for (framework, markers) in frameworks {
        if markers.iter().any(|marker| manifest_text.contains(marker)) {
            push_unique(&mut stack, framework);
        }
    }
    Ok(stack)
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|item| item == value) {
        values.push(value.to_owned());
    }
}

fn one_liner(root: &Path, stack: &[String]) -> Result<String> {
    for name in ["README.md", "README.rst", "README.txt"] {
        let path = root.join(name);
        if !path.is_file() {
            continue;
        }
        let text = fs::read_to_string(path)?;
        if let Some(paragraph) = text.split("\n\n").find(|value| !value.trim().is_empty()) {
            return Ok(paragraph
                .trim()
                .replace('\n', " ")
                .trim_start_matches('#')
                .trim()
                .chars()
                .take(300)
                .collect());
        }
    }
    let repo = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("repo");
    let suffix = if stack.is_empty() {
        String::new()
    } else {
        format!(" ({})", stack.join(", "))
    };
    Ok(format!("Dépôt {repo}{suffix}"))
}

fn identity_evidence(root: &Path, repository_map: &RepositoryMap) -> Option<String> {
    for name in [
        "README.md",
        "README.rst",
        "README.txt",
        "pyproject.toml",
        "package.json",
        "go.mod",
        "Cargo.toml",
        "composer.json",
    ] {
        if root.join(name).is_file() {
            return Some(format!("{name}:1"));
        }
    }
    repository_map
        .packages
        .first()
        .map(|package| package.evidence.clone())
}

pub fn render_markdown(data: &Value) -> Result<String> {
    let text = |path: &str| {
        data.pointer(path)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let mut lines = vec![
        format!(
            "# Briefing {} @ {}   freshness: {}ms   intent: {}",
            text("/repo"),
            text("/sha"),
            data["freshness_ms"],
            text("/intent")
        ),
        String::new(),
        "## À quoi ça sert".to_owned(),
        String::new(),
    ];
    let mut identity = text("/identity/one_liner").to_owned();
    if let Some(evidence) = data.pointer("/identity/evidence").and_then(Value::as_str) {
        identity.push_str(&format!(" (`{evidence}`)"));
    }
    lines.push(identity);
    let repository_map = &data["map"];
    let entries = repository_map["entrypoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let packages = repository_map["packages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !entries.is_empty() || !packages.is_empty() {
        lines.extend([
            String::new(),
            "## Carte (où aller)".to_owned(),
            String::new(),
        ]);
        for entry in entries {
            lines.push(format!(
                "- `{}:{}` — {}",
                entry["path"].as_str().unwrap_or_default(),
                entry["line"],
                entry["why"].as_str().unwrap_or_default()
            ));
        }
        for package in packages.into_iter().take(10) {
            lines.push(format!(
                "- `{}` — {} ({} fichiers)",
                package["evidence"].as_str().unwrap_or_default(),
                package["name"].as_str().unwrap_or_default(),
                package["files"]
            ));
        }
    }
    let contracts = data["contracts"]["http"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !contracts.is_empty() {
        lines.extend([String::new(), "## Contrats".to_owned(), String::new()]);
        for item in contracts {
            lines.push(format!(
                "- `{}:{}` — {} {}",
                item["path"].as_str().unwrap_or_default(),
                item["line"],
                item["method"].as_str().unwrap_or_default(),
                item["route"].as_str().unwrap_or_default()
            ));
        }
    }
    let hotspots = data["hotspots"].as_array().cloned().unwrap_or_default();
    if !hotspots.is_empty() {
        lines.extend([String::new(), "## Hotspots".to_owned(), String::new()]);
        for item in hotspots {
            lines.push(format!(
                "- `{}:1` — {} ({})",
                item["path"].as_str().unwrap_or_default(),
                item["why"].as_str().unwrap_or_default(),
                item["risk"].as_str().unwrap_or_default()
            ));
        }
    }
    if matches!(text("/intent"), "change" | "impact") {
        lines.extend([
            String::new(),
            format!(
                "## Pour changer {}",
                data["focus"].as_str().unwrap_or("le sujet")
            ),
            String::new(),
        ]);
        let touch = data["change_plan"]["touch"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if touch.is_empty() {
            lines.push("Aucun fichier cible établi; affiner `--focus`.".to_owned());
        } else {
            for path in touch {
                lines.push(format!(
                    "- Examiner `{}:1`",
                    path.as_str().unwrap_or_default()
                ));
            }
        }
    }
    let hits = data["hits"].as_array().cloned().unwrap_or_default();
    if !hits.is_empty() {
        lines.extend([String::new(), "## Preuves".to_owned(), String::new()]);
        for hit in hits {
            lines.push(format!(
                "- `{}:{}` — {}: {}",
                hit["path"].as_str().unwrap_or_default(),
                hit["start"],
                hit["symbol"].as_str().unwrap_or("fichier"),
                hit["why"].as_str().unwrap_or_default()
            ));
        }
    } else if let Some(hint) = data["hint"].as_str() {
        lines.extend([
            String::new(),
            "## Preuves".to_owned(),
            String::new(),
            hint.to_owned(),
        ]);
    }
    Ok(format!("{}\n", lines.join("\n")))
}

pub fn default_out(root: &Path) -> PathBuf {
    crate::config::ctx_dir(root)
}
