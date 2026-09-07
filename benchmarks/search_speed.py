#!/usr/bin/env python3
"""Paired CLI benchmark: valid trigram generation versus scan fallback.

Uses an existing binary, never builds. Only mutates disposable repositories.
Reports raw samples and checks complete hit parity; no timing assertions.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import sqlite3
import statistics
import subprocess
import tempfile
import time


def stats(samples):
    ordered = sorted(samples)
    return {"p50_ms": statistics.median(samples),
            "p95_ms": ordered[math.ceil(.95 * len(ordered)) - 1],
            "samples_ms": samples}


def run(binary, root, *args):
    env = os.environ.copy()
    env.pop("CTX_DIR", None)
    started = time.perf_counter()
    result = subprocess.run([str(binary), *args], cwd=root, env=env,
                            capture_output=True, text=True, check=True, timeout=120)
    return (time.perf_counter() - started) * 1000, result.stdout


def corpus(root, count, size):
    for i in range(count):
        folder = root / f"group_{i % 20}"
        folder.mkdir(exist_ok=True)
        body = (f"common_record file_{i:06d} ordinary payload abcdefghijklmnopqrstuvwxyz\n"
                * (size // 70 + 1))[:size]
        if i == count // 2:
            body += "\nrare_validation_needle_7391\nÉCLAIR_unique\n"
        (folder / f"document_{i:06d}.txt").write_text(body)


def benchmark(binary, count, size, iterations, warmups, fastapi_repo=None, scope="all"):
    with tempfile.TemporaryDirectory(prefix="ctx-speed-") as directory:
        root = Path(directory)
        provenance = {"kind": "synthetic_text"}
        if fastapi_repo is None:
            corpus(root, count, size)
        else:
            revision = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=fastapi_repo, text=True).strip()
            paths = subprocess.check_output(
                ["git", "ls-tree", "-r", "--name-only", revision], cwd=fastapi_repo,
                text=True).splitlines()
            paths = [p for p in paths if p.endswith(".py") and
                     (scope == "all" or p.startswith("fastapi/"))]
            if not paths or "fastapi/dependencies/utils.py" not in paths:
                raise ValueError("Expected a FastAPI Git checkout")
            for name in paths:
                target = root / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(subprocess.check_output(
                    ["git", "show", f"{revision}:{name}"], cwd=fastapi_repo))
            count = len(paths)
            provenance = {"kind": "fastapi_python", "revision": revision,
                          "scope": scope, "files": paths,
                          "note": "Tracked Python blobs from HEAD only; no docs, assets or working-tree changes."}
        source_bytes = sum(p.stat().st_size for p in root.rglob("*") if p.is_file())
        cold, _ = run(binary, root, "index", ".")
        unchanged, unchanged_output = run(binary, root, "index", ".")
        db_path = root / ".ctx/index.sqlite"
        with sqlite3.connect(db_path) as db:
            generation = db.execute("SELECT value FROM meta WHERE key='text_generation'").fetchone()[0]
        def select(indexed):
            with sqlite3.connect(db_path) as db:
                db.execute("UPDATE meta SET value=? WHERE key='text_generation'",
                           (generation if indexed else "",))
        scenarios = [
            ("rare_literal", "literal", "rare_validation_needle_7391", False),
            ("common_literal", "literal", "common_record", False),
            ("absent_literal", "literal", "missing_validation_needle_9842", False),
            ("selective_regex", "regex", "rare_validation_needle_[0-9]+", False),
            ("unprunable_regex", "regex", "[0-9]{4}", False),
            ("short_literal", "literal", "ab", False),
            ("unicode_ignore_case", "literal", "éclair_unique", True),
        ]
        if fastapi_repo is not None:
            scenarios = [
                ("function_identifier", "literal", "solve_dependencies", False),
                ("exact_signature", "literal", "async def solve_dependencies(", False),
                ("common_identifier", "literal", "Depends", False),
                ("absent_literal", "literal", "ctx_nonexistent_function_9842", False),
                ("function_regex", "regex", r"def (solve_dependencies|get_dependant)\(", False),
                ("decorator_regex", "regex", r"@(app|router)\.(get|post)\(", False),
                ("short_literal", "literal", "if", False),
                ("identifier_ignore_case", "literal", "HTTPException", True),
            ]
        rows = []
        try:
            for name, mode, query, ignore_case in scenarios:
                samples = {False: [], True: []}
                expected = None
                for iteration in range(warmups + iterations):
                    # Alternate order to reduce systematic warm-cache/order bias.
                    for indexed in ([False, True] if iteration % 2 == 0 else [True, False]):
                        select(indexed)  # outside timed region
                        args = ["search", query, "--mode", mode, "--json",
                                "--limit", "10000000", "--budget-tokens", "1000000000"]
                        if ignore_case:
                            args.append("--ignore-case")
                        elapsed, output = run(binary, root, *args)
                        result = json.loads(output)
                        evidence = {key: result[key] for key in ("hits", "tokens", "coverage", "hint")}
                        if expected is None:
                            expected = evidence
                        if evidence != expected:
                            raise AssertionError(f"Evidence mismatch: {name}, indexed={indexed}")
                        if iteration >= warmups:
                            samples[indexed].append(elapsed)
                expected_hits = (0 if name == "absent_literal" else
                                 1 if name in ("rare_literal", "selective_regex", "unicode_ignore_case") else count)
                if fastapi_repo is not None:
                    expected_hits = 0 if name == "absent_literal" else len(expected["hits"])
                    if name != "absent_literal" and not expected_hits:
                        raise AssertionError(f"Code scenario has no hits: {name}")
                if len(expected["hits"]) != expected_hits or expected["hint"] is not None:
                    raise AssertionError(f"Unexpected or truncated evidence: {name}")
                scan, trigram = stats(samples[False]), stats(samples[True])
                row = {"name": name, "mode": mode, "query": query,
                       "ignore_case": ignore_case, "hits": len(expected["hits"]),
                       "parity": True, "scan_fallback": scan, "trigram": trigram,
                       "speedup": scan["p50_ms"] / trigram["p50_ms"]}
                rows.append(row)
                print(f'{count} files / {name}: {row["speedup"]:.2f}x', flush=True)
        finally:
            select(True)
        return {"files": count, "source_bytes": source_bytes, "provenance": provenance,
                "cold_index_ms": cold, "unchanged_index_ms": unchanged,
                "unchanged_index_output": unchanged_output,
                "index_bytes": sum(p.stat().st_size for p in (root / ".ctx").rglob("*") if p.is_file()),
                "scenarios": rows}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/ctx"))
    parser.add_argument("--fastapi-repo", type=Path, help="Benchmark committed Python code in a local FastAPI checkout (core and all Python files)")
    parser.add_argument("--files", type=int, nargs="+", default=[100, 1000])
    parser.add_argument("--bytes-per-file", type=int, default=16384)
    parser.add_argument("--iterations", type=int, default=10)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if min(args.files) < 1 or args.bytes_per_file < 100 or args.iterations < 2 or args.warmups < 0:
        parser.error("positive corpus sizes, >=2 iterations and >=0 warmups required")
    binary = args.binary.resolve(strict=True)
    result = {"schema": 1, "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "platform": platform.platform(), "cpu": platform.processor(),
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "iterations": args.iterations, "warmups": args.warmups,
              "methodology": "Paired alternating CLI processes, warm OS cache, unlimited output budgets. Scan fallback uses an empty generation reference in the same disposable SQLite index. Includes process startup, SQLite, traversal, matching and JSON serialization; excludes metadata toggle and Python JSON parsing. Not an isolated posting-list, MCP, LLM/token, RSS or cold-filesystem benchmark. Ratios >1 favor trigram; no speed threshold is asserted.",
              "corpora": ([benchmark(binary, 0, 0, args.iterations, args.warmups,
                                     args.fastapi_repo.resolve(strict=True), scope)
                           for scope in ("core", "all")] if args.fastapi_repo else
                          [benchmark(binary, n, args.bytes_per_file, args.iterations, args.warmups) for n in args.files])}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
