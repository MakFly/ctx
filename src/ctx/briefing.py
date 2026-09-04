from __future__ import annotations

import json
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .gitinfo import git_info
from .map import build_map
from .pack import pack_query


def generate_briefing(root: Path, *, intent: str, focus: str | None, harness: str,
                       out: Path, force: bool = False) -> tuple[dict[str, Any], bool]:
    started = time.perf_counter()
    info = git_info(root)
    json_path = out / "briefing.json"
    md_path = out / "briefing.md"
    if json_path.exists() and not force and not info.dirty:
        try:
            previous = json.loads(json_path.read_text(encoding="utf-8"))
            if previous.get("sha") == info.sha:
                return {**previous, "skipped": True}, True
        except (OSError, json.JSONDecodeError):
            pass
    repo_map = build_map(root)
    packed = pack_query(focus, budget_tokens=2000, intent="explore", start=root) if focus else {
        "hits": [], "coverage": "complete", "hint": "Ajoutez --focus pour obtenir des preuves ciblées."
    }
    hits = [hit for hit in packed["hits"] if (root / hit["path"]).is_file()]
    stack = detect_stack(root)
    identity_evidence = _identity_evidence(root, repo_map)
    identity = {"one_liner": one_liner(root, stack), "stack": stack, "evidence": identity_evidence}
    hotspots = [
        {"path": hub["path"], "why": f"hub avec {hub['in_edges']} liens entrants", "risk": "couplage"}
        for hub in repo_map.get("hubs", []) if (root / hub["path"]).is_file()
    ][:7]
    touch = list(dict.fromkeys(hit["path"] for hit in hits if hit["kind"] in {"def", "call"}))[:10]
    tests = [hit["path"] for hit in hits if hit["kind"] == "test" or "test" in hit["path"].lower()]
    data: dict[str, Any] = {
        "schema": "ctx.briefing.v1", "repo": root.name, "sha": info.sha, "dirty": info.dirty,
        "intent": intent, "focus": focus, "generated_at": datetime.now(timezone.utc).isoformat(),
        "freshness_ms": int((time.perf_counter() - started) * 1000), "harness": harness,
        "identity": identity, "map": repo_map, "flows": [],
        "contracts": {"http": repo_map.get("router", []), "events": [], "tables": []},
        "hotspots": hotspots,
        "change_plan": {"touch": touch, "avoid": [], "tests": list(dict.fromkeys(tests)),
                        "blast_radius_files": len(touch), "depth": 2},
        "hits": hits, "coverage": packed["coverage"], "hint": packed.get("hint"),
    }
    data["freshness_ms"] = int((time.perf_counter() - started) * 1000)
    out.mkdir(parents=True, exist_ok=True)
    (out / "map.json").write_text(json.dumps(repo_map, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    json_path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    md_path.write_text(render_markdown(data), encoding="utf-8")
    return data, False


def detect_stack(root: Path) -> list[str]:
    stack: list[str] = []
    manifests = {
        "pyproject.toml": "Python", "requirements.txt": "Python", "package.json": "JavaScript/TypeScript",
        "Cargo.toml": "Rust", "go.mod": "Go", "composer.json": "PHP",
    }
    for filename, label in manifests.items():
        if (root / filename).is_file() and label not in stack:
            stack.append(label)
    suffixes = {path.suffix for path in root.rglob("*") if path.is_file() and len(path.relative_to(root).parts) <= 3}
    if ".py" in suffixes and "Python" not in stack:
        stack.append("Python")
    if suffixes & {".ts", ".tsx", ".js"} and "JavaScript/TypeScript" not in stack:
        stack.append("JavaScript/TypeScript")
    for suffix, label in ((".go", "Go"), (".rs", "Rust"), (".php", "PHP")):
        if suffix in suffixes and label not in stack:
            stack.append(label)
    framework_markers = {
        "FastAPI": ("fastapi",), "Django": ("django",), "Flask": ("flask",), "Litestar": ("litestar",),
        "Next.js": ("\"next\"",), "Express": ("\"express\"",), "NestJS": ("@nestjs/",),
        "Fastify": ("\"fastify\"",), "Hono": ("\"hono\"",), "Vue": ("\"vue\"",), "Svelte": ("\"svelte\"",),
        "Gin": ("gin-gonic/gin",), "Echo": ("labstack/echo",), "Fiber": ("gofiber/fiber",), "Chi": ("go-chi/chi",),
        "Axum": ("axum",), "Actix Web": ("actix-web",), "Rocket": ("rocket",),
        "Laravel": ("laravel/framework",), "Symfony": ("symfony/framework-bundle",), "Slim": ("slim/slim",),
    }
    manifest_names = ("pyproject.toml", "requirements.txt", "package.json", "go.mod", "Cargo.toml", "composer.json")
    manifest_text = "\n".join((root / name).read_text(encoding="utf-8", errors="replace").lower() for name in manifest_names if (root / name).is_file())
    for framework, markers in framework_markers.items():
        if any(marker.lower() in manifest_text for marker in markers):
            stack.append(framework)
    return stack


def one_liner(root: Path, stack: list[str]) -> str:
    readme = next((p for p in (root / "README.md", root / "README.rst", root / "README.txt") if p.is_file()), None)
    if readme:
        paragraphs = [p.strip().replace("\n", " ") for p in readme.read_text(encoding="utf-8", errors="replace").split("\n\n") if p.strip()]
        if paragraphs:
            return paragraphs[0].lstrip("# ")[:300]
    suffix = f" ({', '.join(stack)})" if stack else ""
    return f"Dépôt {root.name}{suffix}"


def _identity_evidence(root: Path, repo_map: dict[str, Any]) -> str | None:
    for name in ("README.md", "README.rst", "README.txt", "pyproject.toml", "package.json", "go.mod", "Cargo.toml", "composer.json"):
        if (root / name).is_file():
            return f"{name}:1"
    packages = repo_map.get("packages", [])
    return packages[0].get("evidence") if packages else None


def render_markdown(data: dict[str, Any]) -> str:
    header = f"# Briefing {data['repo']} @ {data['sha']}   freshness: {data['freshness_ms']}ms   intent: {data['intent']}"
    identity = data["identity"]
    identity_line = identity["one_liner"]
    if identity.get("evidence"):
        identity_line += f" (`{identity['evidence']}`)"
    parts = [header, "", "## À quoi ça sert", "", identity_line]
    repo_map = data["map"]
    evidence = []
    if repo_map.get("entrypoints") or repo_map.get("packages"):
        parts += ["", "## Carte (où aller)", ""]
        for entry in repo_map.get("entrypoints", []):
            parts.append(f"- `{entry['path']}:{entry['line']}` — {entry['why']}")
            evidence.append(f"{entry['path']}:{entry['line']}")
        for package in repo_map.get("packages", [])[:10]:
            parts.append(f"- `{package['evidence']}` — {package['name']} ({package['files']} fichiers)")
            evidence.append(package["evidence"])
    if data.get("flows"):
        parts += ["", "## Flux (max 7)", ""] + [f"- {flow['name']}: {' -> '.join(flow['steps'])}" for flow in data["flows"][:7]]
    contracts = data.get("contracts", {})
    if any(contracts.values()):
        parts += ["", "## Contrats", ""]
        for item in contracts.get("http", []):
            parts.append(f"- `{item['path']}:{item['line']}` — {item['method']} {item['route']}")
            evidence.append(f"{item['path']}:{item['line']}")
    if data.get("hotspots"):
        parts += ["", "## Hotspots", ""]
        for item in data["hotspots"]:
            parts.append(f"- `{item['path']}:1` — {item['why']} ({item['risk']})")
            evidence.append(f"{item['path']}:1")
    if data["intent"] in {"change", "impact"}:
        plan = data["change_plan"]
        parts += ["", f"## Pour changer {data.get('focus') or 'le sujet'}", ""]
        if plan["touch"]:
            parts.extend(f"- Examiner `{path}:1`" for path in plan["touch"])
        else:
            parts.append("Aucun fichier cible établi; affiner `--focus`.")
    if data.get("hits"):
        parts += ["", "## Preuves", ""]
        for hit in data["hits"]:
            parts.append(f"- `{hit['path']}:{hit['start']}` — {hit['symbol']}: {hit['why']}")
    elif data.get("hint"):
        parts += ["", "## Preuves", "", data["hint"]]
    return "\n".join(parts) + "\n"
