from __future__ import annotations

from fnmatch import fnmatch
from pathlib import Path

from .config import find_ctx
from .db import connect
from .graph import graph_query
from .pack import pack_query
from .search import search_index


def create_server():
    from mcp.server.fastmcp import FastMCP
    server = FastMCP("ctx")

    @server.tool()
    def ctx_search(query: str, mode: str = "auto", path: str | None = None,
                   limit: int = 20, budget_tokens: int = 1500) -> dict:
        """Search code symbols and bounded excerpts."""
        return search_index(query, mode=mode, path_filter=path, limit=limit, budget_tokens=budget_tokens)

    @server.tool()
    def ctx_graph(op: str, symbol: str, depth: int = 2) -> dict:
        """Find definitions, references, callers, callees, paths, or impact."""
        return graph_query(op, symbol, depth=depth)

    @server.tool()
    def ctx_pack(query: str, budget_tokens: int = 2000, intent: str = "explore") -> dict:
        """Build a ranked, bounded context pack."""
        return pack_query(query, budget_tokens=budget_tokens, intent=intent)

    @server.tool()
    def ctx_file(q: str, limit: int = 20) -> dict:
        """Find indexed paths by glob or fuzzy substring."""
        conn = connect(find_ctx() / "index.sqlite")
        try:
            paths = [row[0] for row in conn.execute("SELECT path FROM files ORDER BY path")]
        finally:
            conn.close()
        needle = q.lower().replace("*", "")
        matches = [path for path in paths if fnmatch(path, q) or needle in path.lower()][:limit]
        hits = [{"path": path, "start": 1, "end": 1, "symbol": None, "kind": "config",
                 "sig": "", "snippet": "", "score": 1.0, "why": "path match"} for path in matches]
        return {"hits": hits, "tokens": sum(max(1, len(path) // 4) for path in matches),
                "freshness_ms": 0, "coverage": "complete", "hint": None}

    return server


def run() -> None:
    create_server().run(transport="stdio")
