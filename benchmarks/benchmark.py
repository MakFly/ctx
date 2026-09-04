#!/usr/bin/env python3
"""Reproducible synthetic benchmark for ctx indexing and retrieval."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import tempfile
import time
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from ctx.graph import graph_query
from ctx.index import index_repository
from ctx.pack import pack_query
from ctx.search import search_index

LANGUAGES = ("python", "typescript", "go", "rust", "php")
EXTENSIONS = {
    "python": ".py",
    "typescript": ".ts",
    "go": ".go",
    "rust": ".rs",
    "php": ".php",
}


def source_for(language: str, index: int) -> str:
    name = f"service_{index:05d}"
    helper = f"normalize_{index:05d}"
    payload = "payment retry ledger customer order transaction " * 8
    if language == "python":
        return (
            f'"""Synthetic module {index}: {payload}"""\n\n'
            f"def {helper}(value: str) -> str:\n"
            f"    return value.strip().lower()\n\n"
            f"def {name}(order_id: str) -> str:\n"
            f'    """Retry a payment and record the ledger transaction."""\n'
            f"    normalized = {helper}(order_id)\n"
            f'    return f"payment:{'{'}normalized{'}'}"\n'
        )
    if language == "typescript":
        return (
            f"// Synthetic module {index}: {payload}\n"
            f"export function {helper}(value: string): string {{\n"
            f"  return value.trim().toLowerCase();\n"
            f"}}\n\n"
            f"export function {name}(orderId: string): string {{\n"
            f"  const normalized = {helper}(orderId);\n"
            "  return `payment:${normalized}`;\n"
            f"}}\n"
        )
    if language == "go":
        return (
            f"package pkg{index % 20:02d}\n\n"
            f"// Synthetic module {index}: {payload}\n"
            f"func {helper}(value string) string {{ return value }}\n\n"
            f"func {name}(orderID string) string {{\n"
            f"    return {helper}(orderID)\n"
            f"}}\n"
        )
    if language == "rust":
        return (
            f"// Synthetic module {index}: {payload}\n"
            f"pub fn {helper}(value: &str) -> String {{ value.trim().to_lowercase() }}\n\n"
            f"pub fn {name}(order_id: &str) -> String {{\n"
            f"    {helper}(order_id)\n"
            f"}}\n"
        )
    return (
        f"<?php\n// Synthetic module {index}: {payload}\n"
        f"function {helper}(string $value): string {{\n"
        f"    return strtolower(trim($value));\n"
        f"}}\n\n"
        f"function {name}(string $orderId): string {{\n"
        f"    return {helper}($orderId);\n"
        f"}}\n"
    )


def generate_repo(root: Path, file_count: int) -> tuple[list[str], str]:
    python_symbols: list[str] = []
    for index in range(file_count):
        language = LANGUAGES[index % len(LANGUAGES)]
        extension = EXTENSIONS[language]
        folder = root / f"package_{index % 20:02d}"
        folder.mkdir(parents=True, exist_ok=True)
        path = folder / f"module_{index:05d}{extension}"
        path.write_text(source_for(language, index), encoding="utf-8")
        if language == "python":
            python_symbols.append(f"service_{index:05d}")
    target = python_symbols[len(python_symbols) // 2]
    (root / "callsite.py").write_text(
        f"def process_order(order_id: str) -> str:\n"
        f"    return {target}(order_id)\n",
        encoding="utf-8",
    )
    (root / "README.md").write_text(
        "# Synthetic benchmark repository\n\nPayment retry ledger services.\n",
        encoding="utf-8",
    )
    return python_symbols, target


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    weight = position - lower
    return ordered[lower] * (1 - weight) + ordered[upper] * weight


def summarize(values: list[float]) -> dict[str, float]:
    return {
        "min_ms": round(min(values), 3),
        "p50_ms": round(statistics.median(values), 3),
        "p95_ms": round(percentile(values, 0.95), 3),
        "p99_ms": round(percentile(values, 0.99), 3),
        "max_ms": round(max(values), 3),
        "mean_ms": round(statistics.fmean(values), 3),
    }


def measure(operation: Callable[[int], Any], iterations: int, warmups: int) -> dict[str, float]:
    for index in range(warmups):
        operation(index)
    timings: list[float] = []
    for index in range(iterations):
        started = time.perf_counter()
        operation(index)
        timings.append((time.perf_counter() - started) * 1_000)
    return summarize(timings)


def rg_operation(root: Path, symbols: list[str]) -> Callable[[int], None]:
    def run(index: int) -> None:
        subprocess.run(
            ["rg", "-l", "--fixed-strings", symbols[index % len(symbols)], "."],
            cwd=root,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )

    return run


def environment() -> dict[str, Any]:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cpu": platform.processor() or platform.machine(),
        "logical_cpus": os.cpu_count(),
        "ctx_index_workers": os.environ.get("CTX_INDEX_WORKERS", "auto"),
    }


def run_benchmark(file_count: int, iterations: int, warmups: int) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="ctx-benchmark-") as temporary:
        root = Path(temporary) / "repo"
        root.mkdir()
        symbols, target = generate_repo(root, file_count)
        os.environ["CTX_DIR"] = str(root / ".ctx")

        started = time.perf_counter()
        cold = index_repository(root)
        cold_ms = (time.perf_counter() - started) * 1_000

        started = time.perf_counter()
        unchanged = index_repository(root)
        unchanged_ms = (time.perf_counter() - started) * 1_000

        metrics: dict[str, Any] = {
            "index_cold": {
                "elapsed_ms": round(cold_ms, 3),
                "files_per_second": round(cold.files / (cold_ms / 1_000), 1),
            },
            "index_unchanged": {
                "elapsed_ms": round(unchanged_ms, 3),
                "changed_files": unchanged.changed,
            },
            "search_symbol": measure(
                lambda index: search_index(symbols[index % len(symbols)], start=root),
                iterations,
                warmups,
            ),
            "search_text": measure(
                lambda _index: search_index("payment retry ledger", start=root),
                iterations,
                warmups,
            ),
            "graph_def": measure(
                lambda _index: graph_query("def", target, start=root),
                iterations,
                warmups,
            ),
            "graph_callers": measure(
                lambda _index: graph_query("callers", target, start=root),
                iterations,
                warmups,
            ),
            "pack": measure(
                lambda _index: pack_query("retry payment ledger", start=root),
                iterations,
                warmups,
            ),
        }
        if shutil.which("rg"):
            metrics["ripgrep_exact_file_match"] = measure(
                rg_operation(root, symbols),
                iterations,
                warmups,
            )

        total_bytes = sum(
            path.stat().st_size
            for path in root.rglob("*")
            if path.is_file() and ".ctx" not in path.parts
        )
        return {
            "schema": 1,
            "generated_at": datetime.now(UTC).isoformat(),
            "methodology": {
                "fixture": "generated mixed Python/TypeScript/Go/Rust/PHP repository",
                "generated_code_files": file_count,
                "indexed_files": cold.files,
                "source_bytes": total_bytes,
                "iterations": iterations,
                "warmups": warmups,
                "filesystem": "temporary directory",
            },
            "environment": environment(),
            "index": {
                "symbols": cold.symbols,
                "edges": cold.edges,
                "database_bytes": cold.database.stat().st_size,
            },
            "metrics": metrics,
            "notes": [
                "All retrieval measurements are warm-process timings.",
                "ripgrep is a raw exact-match baseline and is not equivalent to ctx pack or graph.",
                "Synthetic results do not predict performance on every real repository.",
            ],
        }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--files", type=int, default=1_000)
    parser.add_argument("--iterations", type=int, default=100)
    parser.add_argument("--warmups", type=int, default=10)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.files < 10 or args.iterations < 1 or args.warmups < 0:
        parser.error("use --files >= 10, --iterations >= 1, and --warmups >= 0")

    result = run_benchmark(args.files, args.iterations, args.warmups)
    rendered = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")


if __name__ == "__main__":
    main()
