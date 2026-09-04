use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

const BEGIN: &str = "<!-- ctx-explore:begin -->";
const END: &str = "<!-- ctx-explore:end -->";
const TARGETS: &[&str] = &["claude", "codex", "cursor", "opencode"];
const SKILL: &str = include_str!("../skills/ctx-explore/SKILL.md");
const OPENAI_SKILL: &str = include_str!("../skills/ctx-explore/agents/openai.yaml");
const AGENTS_SNIPPET: &str = include_str!("../skills/AGENTS.snippet.md");
const CLAUDE_SNIPPET: &str = include_str!("../skills/CLAUDE.snippet.md");
const CLAUDE_AGENT: &str = include_str!("../agents/claude/ctx-explorer.md");
const CODEX_AGENT: &str = include_str!("../agents/codex/ctx-explorer.toml");
const CURSOR_AGENT: &str = include_str!("../agents/cursor/ctx-explorer.md");
const OPENCODE_AGENT: &str = include_str!("../agents/opencode/ctx-explorer.md");

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
    let mut installed = Vec::new();
    if selected.contains("claude") {
        let skill = root.join(".claude/skills/ctx-explore");
        copy_skill(&skill)?;
        write(root.join(".claude/agents/ctx-explorer.md"), CLAUDE_AGENT)?;
        install_json_mcp(&root.join(".mcp.json"))?;
        append_once(&root.join("CLAUDE.md"), CLAUDE_SNIPPET)?;
        installed.extend([
            skill.join("SKILL.md"),
            root.join(".claude/agents/ctx-explorer.md"),
            root.join(".mcp.json"),
            root.join("CLAUDE.md"),
        ]);
    }
    if selected.contains("codex") {
        let skill = root.join(".agents/skills/ctx-explore");
        copy_skill(&skill)?;
        write(root.join(".codex/agents/ctx-explorer.toml"), CODEX_AGENT)?;
        install_codex_mcp(&root.join(".codex/config.toml"))?;
        append_once(&root.join("AGENTS.md"), AGENTS_SNIPPET)?;
        installed.extend([
            skill.join("SKILL.md"),
            root.join(".codex/agents/ctx-explorer.toml"),
            root.join(".codex/config.toml"),
            root.join("AGENTS.md"),
        ]);
    }
    if selected.contains("opencode") {
        let skill = root.join(".opencode/skills/ctx-explore");
        copy_skill(&skill)?;
        write(
            root.join(".opencode/agents/ctx-explorer.md"),
            OPENCODE_AGENT,
        )?;
        install_opencode_mcp(&root.join("opencode.json"))?;
        installed.extend([
            skill.join("SKILL.md"),
            root.join(".opencode/agents/ctx-explorer.md"),
            root.join("opencode.json"),
        ]);
    }
    if selected.contains("cursor") {
        let skill = root.join(".cursor/skills/ctx-explore");
        copy_skill(&skill)?;
        write(root.join(".cursor/agents/ctx-explorer.md"), CURSOR_AGENT)?;
        install_json_mcp(&root.join(".cursor/mcp.json"))?;
        installed.extend([
            skill.join("SKILL.md"),
            root.join(".cursor/agents/ctx-explorer.md"),
            root.join(".cursor/mcp.json"),
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
            ".claude/skills/ctx-explore/SKILL.md",
            ".claude/agents/ctx-explorer.md",
            ".mcp.json",
            "CLAUDE.md",
        ]);
    }
    if selected.contains("codex") {
        paths.extend([
            ".agents/skills/ctx-explore/SKILL.md",
            ".codex/agents/ctx-explorer.toml",
            ".codex/config.toml",
            "AGENTS.md",
        ]);
    }
    if selected.contains("opencode") {
        paths.extend([
            ".opencode/skills/ctx-explore/SKILL.md",
            ".opencode/agents/ctx-explorer.md",
            "opencode.json",
        ]);
    }
    if selected.contains("cursor") {
        paths.extend([
            ".cursor/skills/ctx-explore/SKILL.md",
            ".cursor/agents/ctx-explorer.md",
            ".cursor/mcp.json",
        ]);
    }
    paths.into_iter().map(PathBuf::from).collect()
}

fn copy_skill(destination: &Path) -> Result<()> {
    write(destination.join("SKILL.md"), SKILL)?;
    write(destination.join("agents/openai.yaml"), OPENAI_SKILL)
}

fn append_once(path: &Path, snippet: &str) -> Result<()> {
    let current = fs::read_to_string(path).unwrap_or_default();
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
    write(path.to_path_buf(), &updated)
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

fn write_json(path: &Path, value: &Map<String, Value>) -> Result<()> {
    write(
        path.to_path_buf(),
        &format!(
            "{}\n",
            serde_json::to_string_pretty(&Value::Object(value.clone()))?
        ),
    )
}

fn install_json_mcp(path: &Path) -> Result<()> {
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
    write_json(path, &config)
}

fn install_opencode_mcp(path: &Path) -> Result<()> {
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
    write_json(path, &config)
}

fn install_codex_mcp(path: &Path) -> Result<()> {
    let current = fs::read_to_string(path).unwrap_or_default();
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
    write(path.to_path_buf(), &text)
}

fn write(path: PathBuf, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
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
