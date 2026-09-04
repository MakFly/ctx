from __future__ import annotations

import json
import os
import shutil
from pathlib import Path
from typing import Any

BEGIN = "<!-- ctx-explore:begin -->"
END = "<!-- ctx-explore:end -->"
TARGETS = {"claude", "codex", "opencode", "cursor"}
COMMANDS = {
    "claude": ("claude",),
    "codex": ("codex",),
    "opencode": ("opencode",),
    "cursor": ("cursor", "cursor-agent"),
}
PROJECT_MARKERS = {
    "claude": (".claude", ".mcp.json"),
    "codex": (".codex",),
    "opencode": (".opencode", "opencode.json", "opencode.jsonc"),
    "cursor": (".cursor",),
}
USER_MARKERS = {
    "claude": (".claude",),
    "codex": (".codex", ".agents/skills"),
    "opencode": (".config/opencode",),
    "cursor": (".cursor",),
}


def _resource_root() -> Path:
    source_root = Path(__file__).resolve().parents[2]
    if (source_root / "skills").is_dir() and (source_root / "agents").is_dir():
        return source_root
    return Path(__file__).resolve().parent / "resources"


def _append_once(path: Path, snippet: str) -> None:
    current = path.read_text(encoding="utf-8") if path.exists() else ""
    block = f"{BEGIN}\n{snippet.rstrip()}\n{END}"
    if BEGIN in current and END in current:
        before, rest = current.split(BEGIN, 1)
        _, after = rest.split(END, 1)
        updated = before.rstrip() + "\n\n" + block + after
    else:
        updated = current.rstrip() + ("\n\n" if current.strip() else "") + block + "\n"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(updated, encoding="utf-8")


def _copy_skill(destination: Path) -> None:
    source = _resource_root() / "skills" / "ctx-explore"
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, destination, dirs_exist_ok=True)


def _load_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ValueError(f"JSON invalide, installation annulée: {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError(f"la racine JSON doit être un objet: {path}")
    return value


def _write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def _install_json_mcp(path: Path) -> None:
    config = _load_json(path)
    servers = config.setdefault("mcpServers", {})
    if not isinstance(servers, dict):
        raise ValueError(f"mcpServers doit être un objet: {path}")
    servers["ctx"] = {"type": "stdio", "command": "ctx", "args": ["mcp"]}
    _write_json(path, config)


def _install_opencode_mcp(path: Path) -> None:
    config = _load_json(path)
    config.setdefault("$schema", "https://opencode.ai/config.json")
    mcp = config.setdefault("mcp", {})
    if not isinstance(mcp, dict):
        raise ValueError(f"mcp doit être un objet: {path}")
    # Preserve the legacy v1 shape when the project already uses it; new configs use v2.
    if "servers" in mcp:
        servers = mcp["servers"]
    elif any(isinstance(value, dict) and "type" in value for value in mcp.values()):
        servers = mcp
    else:
        servers = mcp.setdefault("servers", {})
    if not isinstance(servers, dict):
        raise ValueError(f"mcp.servers doit être un objet: {path}")
    if servers is mcp:
        servers["ctx"] = {"type": "local", "command": ["ctx", "mcp"], "enabled": True}
    else:
        servers["ctx"] = {"type": "local", "command": ["ctx", "mcp"]}
    _write_json(path, config)


def _install_codex_mcp(path: Path) -> None:
    current = path.read_text(encoding="utf-8") if path.exists() else ""
    lines = current.splitlines()
    start = next((i for i, line in enumerate(lines) if line.strip() == "[mcp_servers.ctx]"), None)
    if start is not None:
        end = next((i for i in range(start + 1, len(lines)) if lines[i].lstrip().startswith("[")), len(lines))
        del lines[start:end]
    block = ["[mcp_servers.ctx]", 'command = "ctx"', 'args = ["mcp"]', "required = true"]
    text = "\n".join(lines).rstrip()
    text = (text + "\n\n" if text else "") + "\n".join(block) + "\n"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _copy_agent(source_name: str, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(_resource_root() / "agents" / source_name / destination.name, destination)


def detect_harnesses(root: Path, *, path_env: str | None = None, home: Path | None = None) -> dict[str, dict[str, Any]]:
    """Detect harnesses from PATH and explicit project/user configuration markers."""
    search_path = os.environ.get("PATH", "") if path_env is None else path_env
    user_home = Path.home() if home is None else home
    result: dict[str, dict[str, Any]] = {}
    for target in sorted(TARGETS):
        signals: list[str] = []
        project_signals = [marker for marker in PROJECT_MARKERS[target] if (root / marker).exists()]
        for command in COMMANDS[target]:
            executable = shutil.which(command, path=search_path)
            if executable:
                signals.append(f"PATH:{executable}")
                break
        signals.extend(f"project:{marker}" for marker in project_signals)
        signals.extend(
            f"user:{marker}" for marker in USER_MARKERS[target] if (user_home / marker).exists()
        )
        result[target] = {
            "detected": bool(signals),
            "project_installed": bool(project_signals),
            "signals": signals,
        }
    return result


def select_targets(root: Path, target: str, *, mode: str = "install",
                   path_env: str | None = None, home: Path | None = None) -> tuple[set[str], dict[str, dict[str, Any]]]:
    detected = detect_harnesses(root, path_env=path_env, home=home)
    if target == "all":
        return set(TARGETS), detected
    if target == "both":
        return {"claude", "codex"}, detected
    if target == "auto":
        key = "project_installed" if mode == "update" else "detected"
        return {name for name, details in detected.items() if details[key]}, detected
    if target not in TARGETS:
        raise ValueError(f"target inconnu: {target}")
    return {target}, detected


def planned_paths(root: Path, selected: set[str]) -> list[Path]:
    paths: list[Path] = []
    if "claude" in selected:
        paths += [root / ".claude/skills/ctx-explore/SKILL.md", root / ".claude/agents/ctx-explorer.md",
                  root / ".mcp.json", root / "CLAUDE.md"]
    if "codex" in selected:
        paths += [root / ".agents/skills/ctx-explore/SKILL.md", root / ".codex/agents/ctx-explorer.toml",
                  root / ".codex/config.toml", root / "AGENTS.md"]
    if "opencode" in selected:
        paths += [root / ".opencode/skills/ctx-explore/SKILL.md", root / ".opencode/agents/ctx-explorer.md",
                  root / "opencode.json"]
    if "cursor" in selected:
        paths += [root / ".cursor/skills/ctx-explore/SKILL.md", root / ".cursor/agents/ctx-explorer.md",
                  root / ".cursor/mcp.json"]
    return paths


def installation_plan(root: Path, target: str = "auto", *, mode: str = "install",
                      path_env: str | None = None, home: Path | None = None) -> dict[str, Any]:
    selected, detected = select_targets(root, target, mode=mode, path_env=path_env, home=home)
    return {
        "mode": mode,
        "dry_run": True,
        "target": target,
        "detected": detected,
        "selected": sorted(selected),
        "files": [str(path.relative_to(root)) for path in planned_paths(root, selected)],
        "hint": None if selected else (
            "Aucune intégration projet à mettre à jour; utilisez --target <harness>."
            if mode == "update" else "Aucun harness détecté; utilisez --target all ou un target explicite."
        ),
    }


def install(root: Path, target: str, *, mode: str = "install") -> list[Path]:
    selected, _ = select_targets(root, target, mode=mode)
    if not selected:
        raise ValueError(
            "aucune intégration projet à mettre à jour; utilisez --target <harness>"
            if mode == "update" else "aucun harness détecté; utilisez --target all ou un target explicite"
        )
    installed: list[Path] = []
    assets = _resource_root() / "skills"

    if "claude" in selected:
        skill = root / ".claude" / "skills" / "ctx-explore"
        agent = root / ".claude" / "agents" / "ctx-explorer.md"
        _copy_skill(skill)
        _copy_agent("claude", agent)
        _install_json_mcp(root / ".mcp.json")
        _append_once(root / "CLAUDE.md", (assets / "CLAUDE.snippet.md").read_text(encoding="utf-8"))
        installed += [skill / "SKILL.md", agent, root / ".mcp.json", root / "CLAUDE.md"]

    if "codex" in selected:
        skill = root / ".agents" / "skills" / "ctx-explore"
        agent = root / ".codex" / "agents" / "ctx-explorer.toml"
        _copy_skill(skill)
        _copy_agent("codex", agent)
        _install_codex_mcp(root / ".codex" / "config.toml")
        _append_once(root / "AGENTS.md", (assets / "AGENTS.snippet.md").read_text(encoding="utf-8"))
        installed += [skill / "SKILL.md", agent, root / ".codex" / "config.toml", root / "AGENTS.md"]

    if "opencode" in selected:
        skill = root / ".opencode" / "skills" / "ctx-explore"
        agent = root / ".opencode" / "agents" / "ctx-explorer.md"
        _copy_skill(skill)
        _copy_agent("opencode", agent)
        _install_opencode_mcp(root / "opencode.json")
        installed += [skill / "SKILL.md", agent, root / "opencode.json"]

    if "cursor" in selected:
        skill = root / ".cursor" / "skills" / "ctx-explore"
        agent = root / ".cursor" / "agents" / "ctx-explorer.md"
        _copy_skill(skill)
        _copy_agent("cursor", agent)
        _install_json_mcp(root / ".cursor" / "mcp.json")
        installed += [skill / "SKILL.md", agent, root / ".cursor" / "mcp.json"]

    return installed
