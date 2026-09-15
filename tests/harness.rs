use std::fs;
use std::path::Path;

use ctx_code::harness::{install, installation_plan};
use serde_json::Value;

#[test]
fn install_preserves_unreadable_text_configurations() {
    for relative in ["AGENTS.md", ".codex/config.toml"] {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = b"existing settings\xff";
        fs::write(&path, original).unwrap();
        assert!(install(project.path(), "codex", "install").is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }
}

#[test]
fn dry_run_detects_path_without_writing() {
    let project = tempfile::tempdir().unwrap();
    let binaries = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let codex = binaries.path().join("codex");
    fs::write(&codex, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&codex, permissions).unwrap();
    }

    let plan = installation_plan(
        project.path(),
        "auto",
        "install",
        Some(binaries.path().to_str().unwrap()),
        Some(home.path()),
    )
    .unwrap();
    assert!(plan.dry_run);
    assert_eq!(plan.selected, ["codex"]);
    assert_eq!(
        plan.detected["codex"].signals,
        [format!("PATH:{}", codex.display())]
    );
    assert!(!project.path().join(".codex").exists());
}

#[test]
fn install_does_not_create_gitignore_when_missing() {
    let project = tempfile::tempdir().unwrap();
    install(project.path(), "grok", "install").unwrap();
    assert!(project.path().join(".grok/config.toml").is_file());
    assert!(!project.path().join(".gitignore").exists());
}

#[test]
fn installation_is_complete_idempotent_and_preserves_configuration() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    fs::write(root.join("CLAUDE.md"), "# Existing Claude rules\n").unwrap();
    fs::write(root.join("AGENTS.md"), "# Existing agent rules\n").unwrap();
    fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
    fs::write(root.join(".mcp.json"), "{\"keep\":true}\n").unwrap();
    fs::create_dir(root.join(".cursor")).unwrap();
    fs::write(root.join(".cursor/mcp.json"), "{\"keep\":true}\n").unwrap();
    fs::create_dir(root.join(".codex")).unwrap();
    fs::write(
        root.join(".codex/config.toml"),
        "model_reasoning_effort = \"high\"\n",
    )
    .unwrap();
    fs::create_dir(root.join(".grok")).unwrap();
    fs::write(
        root.join(".grok/config.toml"),
        "[models]\ndefault = \"grok-build\"\n\n[mcp_servers.ctx]\ncustom = true\n",
    )
    .unwrap();
    fs::write(root.join("opencode.json"), "{\"theme\":\"dark\"}\n").unwrap();

    let first = install(root, "all", "install").unwrap();
    let second = install(root, "all", "install").unwrap();
    assert_eq!(first, second);
    for path in [
        ".claude/skills/ctx-explore/SKILL.md",
        ".claude/agents/ctx-explorer.md",
        ".claude/settings.json",
        ".agents/skills/ctx-explore/SKILL.md",
        ".codex/agents/ctx-explorer.toml",
        ".grok/config.toml",
        ".opencode/skills/ctx-explore/SKILL.md",
        ".opencode/agents/ctx-explorer.md",
        ".cursor/skills/ctx-explore/SKILL.md",
        ".cursor/agents/ctx-explorer.md",
        ".cursor/hooks.json",
    ] {
        assert!(root.join(path).is_file(), "missing {path}");
    }
    assert_eq!(
        fs::read_to_string(root.join("CLAUDE.md"))
            .unwrap()
            .matches("ctx-explore:begin")
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(root.join("AGENTS.md"))
            .unwrap()
            .matches("ctx-explore:begin")
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(root.join(".gitignore"))
            .unwrap()
            .lines()
            .filter(|line| line.trim() == ".ctx/")
            .count(),
        1
    );
    let claude = read_json(&root.join(".mcp.json"));
    let claude_settings = read_json(&root.join(".claude/settings.json"));
    let cursor = read_json(&root.join(".cursor/mcp.json"));
    let cursor_hooks = read_json(&root.join(".cursor/hooks.json"));
    let opencode = read_json(&root.join("opencode.json"));
    assert_eq!(claude["keep"], true);
    assert_eq!(
        claude["mcpServers"]["ctx"]["args"],
        serde_json::json!(["mcp"])
    );
    assert!(
        claude_settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("ctx hook after-turn")
    );
    assert_eq!(cursor["keep"], true);
    assert_eq!(cursor_hooks["version"], 1);
    assert!(
        cursor_hooks["hooks"]["afterFileEdit"][0]["command"]
            .as_str()
            .unwrap()
            .contains("ctx hook after-turn")
    );
    assert_eq!(opencode["theme"], "dark");
    assert_eq!(
        opencode["mcp"]["servers"]["ctx"]["command"],
        serde_json::json!(["ctx", "mcp"])
    );
    let codex = fs::read_to_string(root.join(".codex/config.toml")).unwrap();
    assert!(codex.contains("model_reasoning_effort = \"high\""));
    assert!(codex.contains("[mcp_servers.ctx]"));
    assert_eq!(codex.matches("[mcp_servers.ctx]").count(), 1);
    assert!(codex.contains("enabled_tools = [\"ctx_pack\"]"));
    assert!(codex.contains("args = [\"mcp\", \"--compact\"]"));
    assert_eq!(codex.matches("[mcp_servers.ctx.tools.ctx_pack]").count(), 1);
    assert!(codex.contains("output_token_limit = 1200"));
    let grok = fs::read_to_string(root.join(".grok/config.toml")).unwrap();
    assert!(grok.contains("default = \"grok-build\""));
    assert!(grok.contains("custom = true"));
    assert!(grok.contains("[mcp_servers.ctx]"));
    assert!(grok.contains("args = [\"mcp\", \"--compact\"]"));
    assert!(grok.contains("enabled = true"));

    let stale = root.join(".agents/skills/ctx-explore/SKILL.md");
    fs::write(&stale, "stale\n").unwrap();
    let updated = install(root, "auto", "update").unwrap();
    assert!(updated.contains(&stale));
    assert!(
        fs::read_to_string(stale)
            .unwrap()
            .starts_with("---\nname: ctx-explore")
    );
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}
