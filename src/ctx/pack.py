from __future__ import annotations

import time
from pathlib import Path
from typing import Any

from .graph import graph_query
from .search import apply_budget, search_index


def pack_query(query: str, *, budget_tokens: int = 2000, intent: str = "explore",
               start: Path | str = ".") -> dict[str, Any]:
    started = time.perf_counter()
    search = search_index(query, limit=20, budget_tokens=max(600, budget_tokens), start=start)
    hits = list(search["hits"])
    symbols = []
    for hit in hits:
        name = hit.get("symbol")
        if name and name not in symbols:
            symbols.append(name)
        if len(symbols) == 3:
            break
    for name in symbols:
        for op in ("def", "callers"):
            result = graph_query(op, name, budget_tokens=500, start=start)
            hits.extend(result["hits"])
    priority = {"def": 0, "call": 1, "ref": 1, "test": 2, "doc": 3, "config": 3}
    file_rank: dict[str, tuple[int, float, str]] = {}
    for hit in hits:
        candidate = (priority.get(hit["kind"], 4), -hit["score"], hit["path"])
        file_rank[hit["path"]] = min(file_rank.get(hit["path"], candidate), candidate)
    ordered_paths = {path: position for position, path in enumerate(sorted(file_rank, key=file_rank.get))}
    hits.sort(key=lambda h: (ordered_paths[h["path"]], priority.get(h["kind"], 4), -h["score"], h["start"]))
    unique: list[dict[str, Any]] = []
    seen: set[tuple[str, int]] = set()
    for hit in hits:
        key = (hit["path"], hit["start"])
        if key not in seen:
            seen.add(key)
            hit["why"] = f"{intent}: {hit['why']}"
            unique.append(hit)
    return apply_budget(unique, budget_tokens, started=started, coverage=search["coverage"], hint=search["hint"])
