from __future__ import annotations

import math
import re
import time
from pathlib import Path
from typing import Any

from .config import find_ctx
from .db import connect, get_meta

IDENTIFIER = re.compile(r"^[A-Za-z_][A-Za-z0-9_.]*$")


def empty_envelope(*, coverage: str = "complete", hint: str | None = None, started: float | None = None) -> dict[str, Any]:
    freshness = int((time.perf_counter() - started) * 1000) if started else 0
    return {"hits": [], "tokens": 0, "freshness_ms": freshness, "coverage": coverage, "hint": hint}


def estimate_tokens(hit: dict[str, Any]) -> int:
    return max(1, math.ceil(len(" ".join(str(v) for v in hit.values() if v is not None)) / 4))


def apply_budget(hits: list[dict[str, Any]], budget_tokens: int, *, started: float, coverage: str = "complete", hint: str | None = None) -> dict[str, Any]:
    kept: list[dict[str, Any]] = []
    tokens = 0
    for hit in hits:
        cost = estimate_tokens(hit)
        if kept and tokens + cost > budget_tokens:
            coverage = "partial"
            break
        if cost > budget_tokens:
            available_chars = max(80, budget_tokens * 4 - 160)
            hit = {**hit, "snippet": str(hit.get("snippet", ""))[:available_chars]}
            cost = min(budget_tokens, estimate_tokens(hit))
            coverage = "partial"
        kept.append(hit)
        tokens += cost
    return {
        "hits": kept,
        "tokens": tokens,
        "freshness_ms": int((time.perf_counter() - started) * 1000),
        "coverage": coverage,
        "hint": hint,
    }


def _fts_query(query: str) -> str:
    terms = re.findall(r"[\w]+", query, flags=re.UNICODE)
    return " OR ".join(f'"{term}"' for term in terms) or '""'


def _hit(row: Any, *, score: float, why: str) -> dict[str, Any]:
    kind = "test" if row["is_test"] else "def"
    return {
        "path": row["path"], "start": row["start"] or 1, "end": row["end"] or row["start"] or 1,
        "symbol": row["name"], "kind": kind, "sig": row["sig"] or "",
        "snippet": row["snippet"] or "", "score": round(score, 4), "why": why,
    }


def search_index(query: str, *, mode: str = "auto", path_filter: str | None = None,
                 limit: int = 20, budget_tokens: int = 1500, start: Path | str = ".") -> dict[str, Any]:
    started = time.perf_counter()
    db_path = find_ctx(start) / "index.sqlite"
    conn = connect(db_path)
    try:
        effective = "symbol" if mode == "auto" and IDENTIFIER.fullmatch(query) else mode
        rows: list[tuple[Any, float, str]] = []
        params: list[Any] = []
        path_sql = ""
        if path_filter:
            path_sql = " AND f.path LIKE ?"
            params.append(f"%{path_filter}%")
        if effective in {"symbol", "auto"}:
            exact = conn.execute(
                "SELECT s.*,f.path,f.is_test,f.is_vendor FROM symbols s JOIN files f ON f.id=s.file_id "
                "WHERE (s.name=? OR s.qualname=?)" + path_sql + " ORDER BY f.is_test,f.is_vendor,f.path,s.start",
                (query, query, *params),
            ).fetchall()
            rows.extend((row, 1.0 - row["is_test"] * 0.12 - row["is_vendor"] * 0.25, "exact symbol") for row in exact)
        try:
            fts = conn.execute(
                "SELECT s.*,f.path,f.is_test,f.is_vendor,bm25(symbols_fts,5.0,3.0,1.0) rank "
                "FROM symbols_fts JOIN symbols s ON s.id=symbols_fts.rowid JOIN files f ON f.id=s.file_id "
                "WHERE symbols_fts MATCH ?" + path_sql + " ORDER BY rank,f.is_test,f.is_vendor,f.path,s.start LIMIT ?",
                (_fts_query(query), *params, limit * 3),
            ).fetchall()
            rows.extend((row, max(0.1, 0.85 / (1 + abs(float(row["rank"])))), "symbol/text match") for row in fts)
        except Exception:
            pass
        seen: set[int] = set()
        hits: list[dict[str, Any]] = []
        for row, score, why in sorted(rows, key=lambda item: (-item[1], item[0]["is_test"], item[0]["is_vendor"], item[0]["path"], item[0]["start"])):
            if row["id"] in seen:
                continue
            seen.add(row["id"])
            hits.append(_hit(row, score=score, why=why))
            if len(hits) >= limit:
                break
        if len(hits) < limit:
            try:
                file_rows = conn.execute(
                    "SELECT f.id,f.path,f.is_test,f.is_vendor,1 start,1 end,'' name,'' sig,x.excerpt snippet,bm25(files_fts) rank "
                    "FROM files_fts JOIN files f ON f.id=files_fts.rowid LEFT JOIN file_excerpts x ON x.file_id=f.id "
                    "WHERE files_fts MATCH ?" + path_sql + " ORDER BY rank,f.is_test,f.path LIMIT ?",
                    (_fts_query(query), *params, limit),
                ).fetchall()
                for row in file_rows:
                    kind = "test" if row["is_test"] else ("doc" if Path(row["path"]).suffix in {".md", ".txt"} else "config")
                    hit = _hit(row, score=max(.08, .55 / (1 + abs(float(row["rank"])))), why="file text match")
                    hit["kind"] = kind
                    hit["symbol"] = None
                    key = (row["path"], 1, kind)
                    if not any((item["path"], item["start"], item["kind"]) == key for item in hits):
                        hits.append(hit)
                    if len(hits) >= limit:
                        break
            except Exception:
                pass
        coverage = "complete" if any(row["lang"] for row in conn.execute("SELECT lang FROM files")) else "text_only"
        return apply_budget(hits, budget_tokens, started=started, coverage=coverage)
    finally:
        conn.close()


def repository_root(start: Path | str = ".") -> Path:
    conn = connect(find_ctx(start) / "index.sqlite")
    try:
        return Path(get_meta(conn, "repo_root", str(Path(start).resolve())))
    finally:
        conn.close()
