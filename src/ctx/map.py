from __future__ import annotations

from collections import Counter, defaultdict
import bisect
from pathlib import Path
import re
from typing import Any

from .config import find_ctx
from .db import connect, get_meta

ENTRY_BASENAMES = {
    "main.py", "app.py", "manage.py", "wsgi.py", "asgi.py",
    "server.ts", "server.tsx", "server.js", "app.ts", "app.tsx", "app.js",
    "main.go", "main.rs", "index.php", "artisan",
}
ENTRY_PATHS = {
    "src/index.ts", "src/index.tsx", "src/index.js", "src/main.rs",
    "public/index.php", "bin/console",
}


def build_map(start: Path | str = ".") -> dict[str, Any]:
    conn = connect(find_ctx(start) / "index.sqlite")
    try:
        root = Path(get_meta(conn, "repo_root", str(Path(start).resolve())))
        files = conn.execute("SELECT id,path FROM files ORDER BY path").fetchall()
        valid = [row for row in files if (root / row["path"]).is_file()]
        packages: dict[str, list[str]] = defaultdict(list)
        for row in valid:
            top = row["path"].split("/", 1)[0]
            packages[top].append(row["path"])
        package_rows = [
            {"name": name, "path": paths[0] if len(paths) == 1 else name, "files": len(paths), "evidence": f"{paths[0]}:1"}
            for name, paths in sorted(packages.items())
        ]
        entrypoints = []
        for row in valid:
            path = row["path"]
            if path in ENTRY_PATHS or Path(path).name in ENTRY_BASENAMES or path.startswith(("cmd/", "bin/")):
                entrypoints.append({"path": path, "line": 1, "why": "entrypoint heuristic"})
        incoming = Counter()
        links: dict[int, set[int]] = defaultdict(set)
        rows = conn.execute(
            "SELECT DISTINCT e.file_id src,s.file_id dst FROM edges e JOIN symbols s ON s.id=e.dst_symbol_id WHERE e.file_id != s.file_id"
        ).fetchall()
        for row in rows:
            links[row["src"]].add(row["dst"])
            incoming[row["dst"]] += 1
        ranks = _pagerank([row["id"] for row in valid], links)
        by_id = {row["id"]: row["path"] for row in valid}
        hubs = [
            {"path": by_id[file_id], "in_edges": incoming[file_id], "pagerank": round(rank, 5), "evidence": f"{by_id[file_id]}:1"}
            for file_id, rank in sorted(ranks.items(), key=lambda item: (-item[1], by_id[item[0]]))[:10]
            if incoming[file_id] or rank > (1 / max(1, len(valid)))
        ]
        router = _routes(root, [row["path"] for row in valid])
        return {"packages": package_rows, "entrypoints": entrypoints, "router": router, "hubs": hubs}
    finally:
        conn.close()


def _pagerank(nodes: list[int], links: dict[int, set[int]], rounds: int = 20) -> dict[int, float]:
    if not nodes:
        return {}
    damping = .85
    rank = {node: 1 / len(nodes) for node in nodes}
    for _ in range(rounds):
        next_rank = {node: (1 - damping) / len(nodes) for node in nodes}
        for src in nodes:
            targets = links.get(src, set())
            if targets:
                for dst in targets:
                    next_rank[dst] = next_rank.get(dst, 0) + damping * rank[src] / len(targets)
            else:
                for dst in nodes:
                    next_rank[dst] += damping * rank[src] / len(nodes)
        rank = next_rank
    return rank


def _routes(root: Path, paths: list[str]) -> list[dict[str, Any]]:
    patterns = [
        # FastAPI, Flask, Litestar, Express, Fastify, Hono, Koa routers, Gin, Echo, Fiber, Chi, Slim.
        re.compile(r"(?:@\w+\.|\.|->)(get|post|put|delete|patch|options|head)\s*\(\s*['\"]([^'\"]+)", re.I),
        # NestJS, Actix and Rocket decorators/attributes.
        re.compile(r"(?:@(Get|Post|Put|Delete|Patch|Options|Head)|#\[(get|post|put|delete|patch))\s*\(\s*['\"]([^'\"]+)", re.I),
        # Laravel static routes.
        re.compile(r"Route::(get|post|put|delete|patch|options|any)\s*\(\s*['\"]([^'\"]+)", re.I),
        # Go net/http.
        re.compile(r"(?:Handle|HandleFunc)\s*\(\s*['\"]([^'\"]+)", re.I),
        # Django URL configuration.
        re.compile(r"(?:path|re_path)\s*\(\s*['\"]([^'\"]+)", re.I),
        # Axum and Symfony attributes (method may follow the path).
        re.compile(r"\.route\s*\(\s*['\"]([^'\"]+)['\"]\s*,\s*(get|post|put|delete|patch)\s*\(", re.I),
        re.compile(r"#\[Route\s*\(\s*['\"]([^'\"]+)[\s\S]{0,200}?methods\s*:\s*\[\s*['\"]([A-Z]+)", re.I),
    ]
    found: list[dict[str, Any]] = []
    for rel in paths:
        if Path(rel).suffix.lower() not in {".py", ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".vue", ".svelte", ".go", ".rs", ".php", ".phtml"}:
            continue
        text = (root / rel).read_text(encoding="utf-8", errors="replace")
        starts = [0, *(match.end() for match in re.finditer("\n", text))]
        for index, pattern in enumerate(patterns):
            for match in pattern.finditer(text):
                groups = [group for group in match.groups() if group]
                if index == 0:
                    method, route = groups[0], groups[1]
                elif index == 1:
                    method, route = groups[-2], groups[-1]
                elif index == 2:
                    method, route = groups[0], groups[1]
                elif index in {3, 4}:
                    method, route = "ANY", groups[0]
                else:
                    route, method = groups[0], groups[1]
                found.append({"path": rel, "line": bisect.bisect_right(starts, match.start()), "route": route, "method": method.upper()})
        found.extend(_next_routes(rel, text, starts))
    unique = {(item["path"], item["line"], item["method"], item["route"]): item for item in found}
    return sorted(unique.values(), key=lambda item: (item["path"], item["line"], item["method"]))[:50]


def _next_routes(rel: str, text: str, starts: list[int]) -> list[dict[str, Any]]:
    path = Path(rel)
    if path.name not in {"route.ts", "route.tsx", "route.js", "route.jsx"} or "app" not in path.parts:
        return []
    app_index = path.parts.index("app")
    segments = []
    for segment in path.parts[app_index + 1:-1]:
        if segment.startswith("(") and segment.endswith(")"):
            continue
        segments.append(f":{segment[1:-1]}" if segment.startswith("[") and segment.endswith("]") else segment)
    route = "/" + "/".join(segments)
    return [
        {"path": rel, "line": bisect.bisect_right(starts, match.start()), "route": route, "method": match.group(1).upper()}
        for match in re.finditer(r"^\s*export\s+(?:async\s+)?function\s+(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD)\b", text, re.M)
    ]
