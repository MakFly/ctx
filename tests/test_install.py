from __future__ import annotations

import json
import tomllib
from pathlib import Path

from ctx.install_harness import install, installation_plan


def test_dry_run_detects_path_without_writing(tmp_path: Path) -> None:
    project = tmp_path / "project"
    binaries = tmp_path / "bin"
    home = tmp_path / "home"
    project.mkdir()
    binaries.mkdir()
    home.mkdir()
    codex = binaries / "codex"
    codex.write_text("#!/bin/sh\n", encoding="utf-8")
    codex.chmod(0o755)

    plan = installation_plan(project, "auto", path_env=str(binaries), home=home)

    assert plan["dry_run"] is True
    assert plan["selected"] == ["codex"]
    assert plan["detected"]["codex"]["signals"] == [f"PATH:{codex}"]
    assert not (project / ".codex").exists()


def test_install_all_harnesses_is_complete_and_idempotent(tmp_path: Path) -> None:
    (tmp_path / "CLAUDE.md").write_text("# Existing Claude rules\n", encoding="utf-8")
    (tmp_path / "AGENTS.md").write_text("# Existing agent rules\n", encoding="utf-8")
    (tmp_path / ".mcp.json").write_text('{"keep": true}\n', encoding="utf-8")
    (tmp_path / ".cursor").mkdir()
    (tmp_path / ".cursor" / "mcp.json").write_text('{"keep": true}\n', encoding="utf-8")
    (tmp_path / ".codex").mkdir()
    (tmp_path / ".codex" / "config.toml").write_text('model_reasoning_effort = "high"\n', encoding="utf-8")
    (tmp_path / "opencode.json").write_text('{"theme": "dark"}\n', encoding="utf-8")

    first = install(tmp_path, "all")
    second = install(tmp_path, "all")
    assert first == second

    expected = [
        ".claude/skills/ctx-explore/SKILL.md", ".claude/agents/ctx-explorer.md",
        ".agents/skills/ctx-explore/SKILL.md", ".codex/agents/ctx-explorer.toml",
        ".opencode/skills/ctx-explore/SKILL.md", ".opencode/agents/ctx-explorer.md",
        ".cursor/skills/ctx-explore/SKILL.md", ".cursor/agents/ctx-explorer.md",
    ]
    assert all((tmp_path / path).is_file() for path in expected)
    assert (tmp_path / "CLAUDE.md").read_text().count("ctx-explore:begin") == 1
    assert (tmp_path / "AGENTS.md").read_text().count("ctx-explore:begin") == 1

    claude = json.loads((tmp_path / ".mcp.json").read_text())
    cursor = json.loads((tmp_path / ".cursor" / "mcp.json").read_text())
    opencode = json.loads((tmp_path / "opencode.json").read_text())
    codex = tomllib.loads((tmp_path / ".codex" / "config.toml").read_text())
    assert claude["keep"] is True and claude["mcpServers"]["ctx"]["args"] == ["mcp"]
    assert cursor["keep"] is True and cursor["mcpServers"]["ctx"]["command"] == "ctx"
    assert opencode["theme"] == "dark" and opencode["mcp"]["servers"]["ctx"]["command"] == ["ctx", "mcp"]
    assert codex["model_reasoning_effort"] == "high" and codex["mcp_servers"]["ctx"]["required"] is True

    stale_skill = tmp_path / ".agents" / "skills" / "ctx-explore" / "SKILL.md"
    stale_skill.write_text("stale\n", encoding="utf-8")
    updated = install(tmp_path, "auto", mode="update")
    assert stale_skill in updated
    assert stale_skill.read_text(encoding="utf-8").startswith("---\nname: ctx-explore")
