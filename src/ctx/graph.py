from __future__ import annotations

import time
from pathlib import Path
from typing import Any

from .config import find_ctx
from .db import connect
from .search import apply_budget


def _row_hit(row: Any, kind: str, why: str, score: float = 1.0) -> dict[str, Any]:
    return {
        "path": row["path"], "start": row["start"], "end": row["end"],
        "symbol": row["name"], "kind": kind, "sig": row["sig"] or "",
        "snippet": row["snippet"] or "", "score": score, "why": why,
    }


def graph_query(op: str, symbol: str, *, depth: int = 2, budget_tokens: int = 1500,
                start: Path | str = ".") -> dict[str, Any]:
    started = time.perf_counter()
    conn = connect(find_ctx(start) / "index.sqlite")
    coverage = "complete"
    try:
        definitions = conn.execute(
            "SELECT s.*,f.path,f.is_test,f.is_vendor FROM symbols s JOIN files f ON f.id=s.file_id "
            "WHERE s.name=? OR s.qualname=? ORDER BY f.is_test,f.is_vendor,f.path,s.start", (symbol, symbol),
        ).fetchall()
        hits: list[dict[str, Any]] = []
        if op == "def":
            hits = [_row_hit(row, "def", "definition") for row in definitions]
            if len(definitions) > 1:
                coverage = "partial"
        elif op in {"refs", "callers"}:
            dst_ids = [row["id"] for row in definitions]
            if dst_ids:
                placeholders = ",".join("?" for _ in dst_ids)
                rows = conn.execute(
                    f"SELECT e.line,e.kind,e.source,e.confidence,f.path,f.is_test,COALESCE(s.name,e.dst_name) name,"
                    "COALESCE(s.start,e.line) start,COALESCE(s.end,e.line) end,COALESCE(s.sig,'') sig,COALESCE(s.snippet,'') snippet "
                    "FROM edges e JOIN files f ON f.id=e.file_id LEFT JOIN symbols s ON s.id=e.src_symbol_id "
                    f"WHERE e.dst_symbol_id IN ({placeholders}) OR e.dst_name=? ORDER BY (e.source='lsp') DESC,f.is_test,f.path,e.line",
                    (*dst_ids, symbol),
                ).fetchall()
            else:
                coverage = "partial"
                rows = conn.execute(
                    "SELECT e.line,e.kind,e.source,e.confidence,f.path,f.is_test,COALESCE(s.name,e.dst_name) name,"
                    "COALESCE(s.start,e.line) start,COALESCE(s.end,e.line) end,COALESCE(s.sig,'') sig,COALESCE(s.snippet,'') snippet "
                    "FROM edges e JOIN files f ON f.id=e.file_id LEFT JOIN symbols s ON s.id=e.src_symbol_id "
                    "WHERE e.dst_name=? ORDER BY (e.source='lsp') DESC,f.is_test,f.path,e.line", (symbol,),
                ).fetchall()
            hits = [
                _row_hit(
                    row, "call" if row["kind"] == "call" else "ref",
                    f"{row['source']} {row['kind']} of {symbol}",
                    float(row["confidence"]) if row["source"] == "lsp" else .9,
                )
                for row in rows
            ]
            if len(definitions) > 1:
                coverage = "partial"
        elif op == "callees":
            ids = [row["id"] for row in definitions]
            rows = [] if not ids else conn.execute(
                f"SELECT d.*,f.path,f.is_test FROM edges e JOIN symbols d ON d.id=e.dst_symbol_id JOIN files f ON f.id=d.file_id WHERE e.src_symbol_id IN ({','.join('?' for _ in ids)}) ORDER BY f.path,d.start",
                ids,
            ).fetchall()
            hits = [_row_hit(row, "call", f"called by {symbol}", .9) for row in rows]
            coverage = "partial"
        elif op in {"path", "impact"}:
            base = [_row_hit(row, "def", "definition") for row in definitions]
            callers = graph_query("callers", symbol, depth=max(1, depth - 1), budget_tokens=budget_tokens, start=start)
            hits = base + callers["hits"]
            coverage = "partial"
        else:
            raise ValueError(f"opération graph inconnue: {op}")
        unique: dict[tuple[str, int, str], dict[str, Any]] = {}
        for hit in hits:
            unique.setdefault((hit["path"], hit["start"], hit["kind"]), hit)
        return apply_budget(list(unique.values()), budget_tokens, started=started, coverage=coverage,
                            hint="résolution statique best-effort" if coverage == "partial" else None)
    finally:
        conn.close()
