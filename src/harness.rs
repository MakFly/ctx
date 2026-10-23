use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use toml_edit::{Array, DocumentMut, Item, Table, value};

const BEGIN: &str = "<!-- ctx-explore:begin -->";
const END: &str = "<!-- ctx-explore:end -->";
const TARGETS: &[&str] = &["claude", "codex", "cursor", "grok", "opencode"];
const SKILL: &str = include_str!("../skills/ctx-explore/SKILL.md");
const OPENAI_SKILL: &str = include_str!("../skills/ctx-explore/agents/openai.yaml");
const SKILL_FILES: &[(&str, &str)] = &[
    ("SKILL.md", SKILL),
    ("agents/openai.yaml", OPENAI_SKILL),
    (
        "references/setup.md",
        include_str!("../skills/ctx-explore/references/setup.md"),
    ),
    (
        "references/retrieval.md",
        include_str!("../skills/ctx-explore/references/retrieval.md"),
    ),
];
const AGENTS_SNIPPET: &str = include_str!("../skills/AGENTS.snippet.md");
const CLAUDE_SNIPPET: &str = include_str!("../skills/CLAUDE.snippet.md");
const CLAUDE_AGENT: &str = include_str!("../agents/claude/ctx-explorer.md");
const CODEX_AGENT: &str = include_str!("../agents/codex/ctx-explorer.toml");
const CURSOR_AGENT: &str = include_str!("../agents/cursor/ctx-explorer.md");
const OPENCODE_AGENT: &str = include_str!("../agents/opencode/ctx-explorer.md");
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub detected: bool,
    pub project_installed: bool,
    pub signals: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationPlan {
    pub mode: String,
    pub dry_run: bool,
    pub target: String,
    pub detected: BTreeMap<String, Detection>,
    pub selected: Vec<String>,
    pub files: Vec<String>,
    pub hint: Option<String>,
}

pub fn detect_harnesses(
    root: &Path,
    path_environment: Option<&str>,
    home: Option<&Path>,
) -> BTreeMap<String, Detection> {
    let search_path = path_environment
        .map(str::to_owned)
        .or_else(|| env::var("PATH").ok())
        .unwrap_or_default();
    let user_home = home
        .map(Path::to_path_buf)
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_default();
    TARGETS
        .iter()
        .map(|target| {
            let mut signals = Vec::new();
            let project_markers = project_markers(target)
                .iter()
                .filter(|marker| root.join(marker).exists())
                .copied()
                .collect::<Vec<_>>();
            if let Some(executable) = commands(target)
                .iter()
                .find_map(|command| find_executable(command, &search_path))
            {
                signals.push(format!("PATH:{}", executable.display()));
            }
            signals.extend(
                project_markers
                    .iter()
                    .map(|marker| format!("project:{marker}")),
            );
            signals.extend(
                user_markers(target)
                    .iter()
                    .filter(|marker| user_home.join(marker).exists())
                    .map(|marker| format!("user:{marker}")),
            );
            (
                (*target).to_owned(),
                Detection {
                    detected: !signals.is_empty(),
                    project_installed: !project_markers.is_empty(),
                    signals,
                },
            )
        })
        .collect()
}

pub fn installation_plan(
    root: &Path,
    target: &str,
    mode: &str,
    path_environment: Option<&str>,
    home: Option<&Path>,
) -> Result<InstallationPlan> {
    let detected = detect_harnesses(root, path_environment, home);
    let selected = select_targets(target, mode, &detected)?;
    let hint = selected.is_empty().then(|| {
        if mode == "update" {
            "Aucune intégration projet à mettre à jour; utilisez --target <harness>.".to_owned()
        } else {
            "Aucun harness détecté; utilisez --target all ou un target explicite.".to_owned()
        }
    });
    let files = planned_paths(&selected)
        .into_iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    Ok(InstallationPlan {
        mode: mode.to_owned(),
        dry_run: true,
        target: target.to_owned(),
        detected,
        selected: selected.into_iter().collect(),
        files,
        hint,
    })
}

pub fn install(root: &Path, target: &str, mode: &str) -> Result<Vec<PathBuf>> {
    let detected = detect_harnesses(root, None, None);
    let selected = select_targets(target, mode, &detected)?;
    if selected.is_empty() {
        bail!(
            "{}",
            if mode == "update" {
                "aucune intégration projet à mettre à jour; utilisez --target <harness>"
            } else {
                "aucun harness détecté; utilisez --target all ou un target explicite"
            }
        );
    }
    ensure_ctx_gitignore(root)?;
    let mut installed = Vec::new();
    for target in &selected {
        if let Some(directory) = skill_directory(target) {
            installed.extend(copy_skill(root, &root.join(directory))?);
        }
    }
    if selected.contains("claude") {
        write(
            root,
            root.join(".claude/agents/ctx-explorer.md"),
            CLAUDE_AGENT,
        )?;
        install_json_mcp(root, &root.join(".mcp.json"))?;
        install_claude_metrics_hooks(root, &root.join(".claude/settings.json"))?;
        append_once(root, &root.join("CLAUDE.md"), CLAUDE_SNIPPET)?;
        installed.extend([
            root.join(".claude/agents/ctx-explorer.md"),
            root.join(".mcp.json"),
            root.join(".claude/settings.json"),
            root.join("CLAUDE.md"),
        ]);
    }
    if selected.contains("codex") {
        write(
            root,
            root.join(".codex/agents/ctx-explorer.toml"),
            CODEX_AGENT,
        )?;
        install_codex_mcp(root, &root.join(".codex/config.toml"))?;
        append_once(root, &root.join("AGENTS.md"), AGENTS_SNIPPET)?;
        installed.extend([
            root.join(".codex/agents/ctx-explorer.toml"),
            root.join(".codex/config.toml"),
            root.join("AGENTS.md"),
        ]);
    }
    if selected.contains("grok") {
        let config = root.join(".grok/config.toml");
        install_grok_mcp(root, &config)?;
        append_once(root, &root.join("AGENTS.md"), AGENTS_SNIPPET)?;
        installed.extend([config, root.join("AGENTS.md")]);
    }
    if selected.contains("opencode") {
        write(
            root,
            root.join(".opencode/agents/ctx-explorer.md"),
            OPENCODE_AGENT,
        )?;
        install_opencode_mcp(root, &root.join("opencode.json"))?;
        installed.extend([
            root.join(".opencode/agents/ctx-explorer.md"),
            root.join("opencode.json"),
        ]);
    }
    if selected.contains("cursor") {
        write(
            root,
            root.join(".cursor/agents/ctx-explorer.md"),
            CURSOR_AGENT,
        )?;
        install_json_mcp(root, &root.join(".cursor/mcp.json"))?;
        install_cursor_metrics_hooks(root, &root.join(".cursor/hooks.json"))?;
        installed.extend([
            root.join(".cursor/agents/ctx-explorer.md"),
            root.join(".cursor/mcp.json"),
            root.join(".cursor/hooks.json"),
        ]);
    }
    Ok(installed)
}

fn select_targets(
    target: &str,
    mode: &str,
    detected: &BTreeMap<String, Detection>,
) -> Result<BTreeSet<String>> {
    let selected = match target {
        "all" => TARGETS.iter().map(|value| (*value).to_owned()).collect(),
        "both" => ["claude", "codex"].into_iter().map(str::to_owned).collect(),
        "auto" => detected
            .iter()
            .filter(|(_, details)| {
                if mode == "update" {
                    details.project_installed
                } else {
                    details.detected
                }
            })
            .map(|(name, _)| name.clone())
            .collect(),
        value if TARGETS.contains(&value) => [value.to_owned()].into_iter().collect(),
        _ => bail!("target inconnu: {target}"),
    };
    Ok(selected)
}

fn planned_paths(selected: &BTreeSet<String>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if selected.contains("claude") {
        paths.extend([
            ".claude/agents/ctx-explorer.md",
            ".mcp.json",
            ".claude/settings.json",
            "CLAUDE.md",
        ]);
    }
    if selected.contains("codex") {
        paths.extend([
            ".codex/agents/ctx-explorer.toml",
            ".codex/config.toml",
            "AGENTS.md",
        ]);
    }
    if selected.contains("grok") {
        paths.extend([".grok/config.toml", "AGENTS.md"]);
    }
    if selected.contains("opencode") {
        paths.extend([".opencode/agents/ctx-explorer.md", "opencode.json"]);
    }
    if selected.contains("cursor") {
        paths.extend([
            ".cursor/agents/ctx-explorer.md",
            ".cursor/mcp.json",
            ".cursor/hooks.json",
        ]);
    }
    let mut unique = Vec::new();
    for path in paths {
        let path = PathBuf::from(path);
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    for target in selected {
        if let Some(directory) = skill_directory(target) {
            unique.extend(
                SKILL_FILES
                    .iter()
                    .map(|(relative, _)| Path::new(directory).join(relative)),
            );
        }
    }
    unique
}

fn skill_directory(target: &str) -> Option<&'static str> {
    match target {
        "claude" => Some(".claude/skills/ctx-explore"),
        "codex" => Some(".agents/skills/ctx-explore"),
        "grok" => Some(".grok/skills/ctx-explore"),
        "opencode" => Some(".opencode/skills/ctx-explore"),
        "cursor" => Some(".cursor/skills/ctx-explore"),
        _ => None,
    }
}

fn copy_skill(root: &Path, destination: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (relative, content) in SKILL_FILES {
        let path = destination.join(relative);
        write(root, path.clone(), content)?;
        written.push(path);
    }
    Ok(written)
}

fn append_once(root: &Path, path: &Path, snippet: &str) -> Result<()> {
    let current = read_existing(path)?;
    let block = format!("{BEGIN}\n{}\n{END}", snippet.trim_end());
    let updated = match (current.find(BEGIN), current.find(END)) {
        (Some(start), Some(end)) if start <= end => {
            let after = end + END.len();
            format!(
                "{}\n\n{}{}",
                current[..start].trim_end(),
                block,
                &current[after..]
            )
            .trim_start()
            .to_owned()
                + "\n"
        }
        _ if current.trim().is_empty() => format!("{block}\n"),
        _ => format!("{}\n\n{block}\n", current.trim_end()),
    };
    write(root, path.to_path_buf(), &updated)
}

fn load_json(path: &Path) -> Result<Map<String, Value>> {
    if !path.is_file() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("JSON invalide, installation annulée: {}", path.display()))?;
    value
        .as_object()
        .cloned()
        .with_context(|| format!("la racine JSON doit être un objet: {}", path.display()))
}

fn write_json(root: &Path, path: &Path, value: &Map<String, Value>) -> Result<()> {
    write(
        root,
        path.to_path_buf(),
        &format!(
            "{}\n",
            serde_json::to_string_pretty(&Value::Object(value.clone()))?
        ),
    )
}

fn install_json_mcp(root: &Path, path: &Path) -> Result<()> {
    let mut config = load_json(path)?;
    let servers = config
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("mcpServers doit être un objet: {}", path.display()))?;
    servers.insert(
        "ctx".to_owned(),
        json!({"type": "stdio", "command": "ctx", "args": ["mcp"]}),
    );
    write_json(root, path, &config)
}

fn install_claude_metrics_hooks(root: &Path, path: &Path) -> Result<()> {
    let mut config = load_json(path)?;
    let hooks = config
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("hooks doit être un objet: {}", path.display()))?;
    // Metrics after each turn, and the search hooks: a bare shell search runs under
    // `ctx compact --run`, and a Grep content result comes back grouped by file with
    // the source it points at.
    for (event, matcher, command) in [
        ("Stop", None, "ctx hook after-turn --harness claude"),
        ("SessionEnd", None, "ctx hook after-turn --harness claude"),
        (
            "PreToolUse",
            Some("Bash"),
            "ctx hook rewrite --harness claude",
        ),
        (
            "PostToolUse",
            Some("Grep"),
            "ctx hook compact-output --harness claude",
        ),
    ] {
        let entries = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .with_context(|| format!("hooks.{event} doit être un tableau: {}", path.display()))?;
        let present = entries.iter().any(|entry| {
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|nested| {
                    nested
                        .iter()
                        .any(|hook| hook.get("command").and_then(Value::as_str) == Some(command))
                })
        });
        if !present {
            let mut entry = json!({
                "hooks": [{
                    "type": "command",
                    "command": command
                }]
            });
            if let Some(matcher) = matcher {
                entry["matcher"] = json!(matcher);
            }
            entries.push(entry);
        }
    }
    write_json(root, path, &config)
}

fn install_cursor_metrics_hooks(root: &Path, path: &Path) -> Result<()> {
    let mut config = load_json(path)?;
    config.entry("version").or_insert_with(|| json!(1));
    let hooks = config
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("hooks doit être un objet: {}", path.display()))?;
    for (event, command) in [
        ("afterFileEdit", "ctx hook after-turn --harness cursor"),
        ("sessionEnd", "ctx hook after-turn --harness cursor"),
    ] {
        let entries = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .with_context(|| format!("hooks.{event} doit être un tableau: {}", path.display()))?;
        let present = entries
            .iter()
            .any(|entry| entry.get("command").and_then(Value::as_str) == Some(command));
        if !present {
            entries.push(json!({"command": command}));
        }
    }
    write_json(root, path, &config)
}

fn install_opencode_mcp(root: &Path, path: &Path) -> Result<()> {
    let mut config = load_json(path)?;
    config
        .entry("$schema")
        .or_insert_with(|| json!("https://opencode.ai/config.json"));
    let mcp = config
        .entry("mcp")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("mcp doit être un objet: {}", path.display()))?;
    let legacy = mcp.contains_key("servers")
        || !mcp.values().any(|value| {
            value
                .as_object()
                .is_some_and(|object| object.contains_key("type"))
        });
    if legacy {
        let servers = mcp
            .entry("servers")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .with_context(|| format!("mcp.servers doit être un objet: {}", path.display()))?;
        servers.insert(
            "ctx".to_owned(),
            json!({"type": "local", "command": ["ctx", "mcp"]}),
        );
    } else {
        mcp.insert(
            "ctx".to_owned(),
            json!({"type": "local", "command": ["ctx", "mcp"], "enabled": true}),
        );
    }
    write_json(root, path, &config)
}

fn install_codex_mcp(root: &Path, path: &Path) -> Result<()> {
    let current = read_existing(path)?;
    let mut lines = current.lines().map(str::to_owned).collect::<Vec<_>>();
    if let Some(start) = lines
        .iter()
        .position(|line| line.trim() == "[mcp_servers.ctx]")
    {
        let end = (start + 1..lines.len())
            .find(|index| {
                let line = lines[*index].trim_start();
                line.starts_with('[') && !line.starts_with("[mcp_servers.ctx.")
            })
            .unwrap_or(lines.len());
        lines.drain(start..end);
    }
    let mut text = lines.join("\n").trim_end().to_owned();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(
        "[mcp_servers.ctx]\ncommand = \"ctx\"\nargs = [\"mcp\", \"--compact\"]\nrequired = true\nenabled_tools = [\"ctx_pack\"]\n\n[mcp_servers.ctx.tools.ctx_pack]\noutput_token_limit = 1200\n",
    );
    toml_edit::DocumentMut::from_str(&text)
        .with_context(|| format!("TOML invalide, installation annulée: {}", path.display()))?;
    write(root, path.to_path_buf(), &text)
}

fn install_grok_mcp(root: &Path, path: &Path) -> Result<()> {
    let current = read_existing(path)?;
    let mut document = DocumentMut::from_str(&current)
        .with_context(|| format!("TOML invalide, installation annulée: {}", path.display()))?;
    let servers = document
        .entry("mcp_servers")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .with_context(|| format!("mcp_servers doit être une table: {}", path.display()))?;
    let ctx = servers
        .entry("ctx")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .with_context(|| format!("mcp_servers.ctx doit être une table: {}", path.display()))?;
    ctx.insert("command", value("ctx"));
    let mut args = Array::new();
    args.push("mcp");
    args.push("--compact");
    ctx.insert("args", Item::Value(args.into()));
    ctx.insert("enabled", value(true));
    ctx.insert("startup_timeout_sec", value(30));
    ctx.insert("tool_timeout_sec", value(6000));
    write(root, path.to_path_buf(), &document.to_string())
}

fn ensure_ctx_gitignore(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    if !path.is_file() {
        return Ok(());
    }
    let current = fs::read_to_string(&path)?;
    if current.lines().any(ignores_ctx_directory) {
        return Ok(());
    }
    let mut updated = current.trim_end().to_owned();
    if !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str(".ctx/\n");
    write(root, path, &updated)?;
    Ok(())
}

fn ignores_ctx_directory(line: &str) -> bool {
    matches!(
        line.trim(),
        ".ctx" | ".ctx/" | "/.ctx" | "/.ctx/" | "**/.ctx/"
    )
}

fn read_existing(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("lecture impossible: {}", path.display())),
    }
}

fn write(root: &Path, path: PathBuf, contents: &str) -> Result<()> {
    let canonical_root = fs::canonicalize(root).with_context(|| {
        format!(
            "impossible de résoudre le dossier du projet: {}",
            root.display()
        )
    })?;
    let target = resolve_project_target(&canonical_root, &path)?;
    if !target.starts_with(&canonical_root) {
        bail!(
            "écriture refusée: la destination sort du dossier du projet: {}",
            path.display()
        );
    }

    let parent = target
        .parent()
        .with_context(|| format!("destination sans dossier parent: {}", target.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("impossible de créer le dossier: {}", parent.display()))?;

    let mut temporary = None;
    for _ in 0..100 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".ctx-write-{}-{sequence}.tmp", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "impossible de créer un fichier temporaire dans {}",
                        parent.display()
                    )
                });
            }
        }
    }
    let (temporary_path, mut file) =
        temporary.context("impossible de créer un fichier temporaire pour l’écriture")?;
    let result = (|| -> Result<()> {
        file.write_all(contents.as_bytes())?;
        // The rename replaces the inode: keep the existing file's permissions.
        if let Ok(metadata) = fs::metadata(&target) {
            file.set_permissions(metadata.permissions())?;
        }
        drop(file);
        fs::rename(&temporary_path, &target)
            .with_context(|| format!("impossible de remplacer le fichier: {}", target.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn resolve_project_target(canonical_root: &Path, path: &Path) -> Result<PathBuf> {
    let mut current = path.to_path_buf();
    let mut remaining = Vec::new();
    loop {
        match fs::canonicalize(&current) {
            Ok(canonical) => {
                let target = remaining
                    .iter()
                    .rev()
                    .fold(canonical, |target, component| target.join(component));
                if !target.starts_with(canonical_root) {
                    bail!(
                        "écriture refusée: la destination sort du dossier du projet: {}",
                        path.display()
                    );
                }
                return Ok(target);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if fs::symlink_metadata(&current)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    bail!(
                        "écriture refusée: impossible de résoudre le lien symbolique: {}",
                        current.display()
                    );
                }
                let component = current.file_name().with_context(|| {
                    format!("impossible de résoudre la destination: {}", path.display())
                })?;
                remaining.push(component.to_os_string());
                current = current
                    .parent()
                    .with_context(|| {
                        format!("impossible de résoudre la destination: {}", path.display())
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("impossible de résoudre la destination: {}", path.display())
                });
            }
        }
    }
}

fn find_executable(command: &str, search_path: &str) -> Option<PathBuf> {
    env::split_paths(search_path).find_map(|directory| {
        executable_candidates(&directory, command)
            .into_iter()
            .find(|path| is_executable(path))
    })
}

fn executable_candidates(directory: &Path, command: &str) -> Vec<PathBuf> {
    let base = directory.join(command);
    #[cfg(windows)]
    {
        let extensions = env::var_os("PATHEXT")
            .and_then(|value| value.into_string().ok())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
        let mut candidates = vec![base.clone()];
        if base.extension().is_none() {
            candidates.extend(
                extensions
                    .split(';')
                    .filter(|extension| !extension.is_empty())
                    .map(|extension| directory.join(format!("{command}{extension}"))),
            );
        }
        candidates
    }
    #[cfg(not(windows))]
    {
        vec![base]
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn commands(target: &str) -> &'static [&'static str] {
    match target {
        "claude" => &["claude"],
        "codex" => &["codex"],
        "opencode" => &["opencode"],
        "cursor" => &["cursor", "cursor-agent"],
        _ => &[],
    }
}

fn project_markers(target: &str) -> &'static [&'static str] {
    match target {
        "claude" => &[".claude", ".mcp.json"],
        "codex" => &[".codex"],
        "opencode" => &[".opencode", "opencode.json", "opencode.jsonc"],
        "cursor" => &[".cursor"],
        _ => &[],
    }
}

fn user_markers(target: &str) -> &'static [&'static str] {
    match target {
        "claude" => &[".claude"],
        "codex" => &[".codex", ".agents/skills"],
        "opencode" => &[".config/opencode"],
        "cursor" => &[".cursor"],
        _ => &[],
    }
}
