#!/usr/bin/env python3
"""Reproducible real-repository benchmark orchestration for ctx and peers."""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
DEFAULT_REPOS = HERE / "repos.json"


def load_manifest(path: Path) -> list[dict[str, Any]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if payload.get("schema") != 1 or not isinstance(payload.get("repositories"), list):
        raise ValueError(f"unsupported repository manifest: {path}")
    return payload["repositories"]


def checked_run(
    arguments: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        arguments,
        cwd=cwd,
        env=env,
        check=True,
        stdin=subprocess.DEVNULL,
        text=True,
        capture_output=capture,
    )


def prepare(repositories: list[dict[str, Any]], workspace: Path) -> None:
    source_root = workspace / "repos"
    source_root.mkdir(parents=True, exist_ok=True)
    for repository in repositories:
        destination = source_root / repository["id"]
        expected = repository["commit"]
        if destination.exists():
            actual = git_head(destination)
            if actual != expected:
                raise RuntimeError(
                    f"{repository['id']}: expected {expected}, found {actual}; "
                    "use a fresh workspace"
                )
            print(f"verified {repository['id']} @ {actual}")
            continue
        destination.mkdir()
        checked_run(["git", "init", "--quiet"], cwd=destination)
        checked_run(["git", "remote", "add", "origin", repository["url"]], cwd=destination)
        checked_run(
            ["git", "fetch", "--quiet", "--depth", "1", "origin", expected],
            cwd=destination,
        )
        checked_run(["git", "checkout", "--quiet", "--detach", "FETCH_HEAD"], cwd=destination)
        actual = git_head(destination)
        if actual != expected:
            raise RuntimeError(f"{repository['id']}: fetched {actual}, expected {expected}")
        print(f"prepared {repository['id']} @ {actual}")


def git_head(repository: Path) -> str:
    return checked_run(["git", "rev-parse", "HEAD"], cwd=repository).stdout.strip()


def verify(repositories: list[dict[str, Any]], workspace: Path) -> None:
    failures: list[str] = []
    for repository in repositories:
        root = workspace / "repos" / repository["id"]
        if not root.is_dir():
            failures.append(f"{repository['id']}: repository is missing")
            continue
        actual = git_head(root)
        if actual != repository["commit"]:
            failures.append(
                f"{repository['id']}: commit {actual} != {repository['commit']}"
            )
        for check in repository["checks"]:
            path = root / check["path"]
            if not path.is_file():
                failures.append(f"{repository['id']}: missing {check['path']}")
                continue
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
            line = check["line"]
            if line < 1 or line > len(lines):
                failures.append(f"{repository['id']}: invalid {check['path']}:{line}")
                continue
            needle = check["symbol"].split(".")[-1]
            window = "\n".join(lines[line - 1 : min(line + 2, len(lines))])
            if needle not in window:
                failures.append(
                    f"{repository['id']}: {needle!r} absent near {check['path']}:{line}"
                )
        print(f"checked {repository['id']}: {len(repository['checks'])} facts")
    if failures:
        raise RuntimeError("ground-truth verification failed:\n- " + "\n- ".join(failures))


def timed_command(
    arguments: list[str], *, cwd: Path, env: dict[str, str]
) -> tuple[float, int | None, str, str]:
    time_binary = Path("/usr/bin/time")
    with tempfile.NamedTemporaryFile(prefix="ctx-bench-time-", delete=False) as handle:
        timing_path = Path(handle.name)
    command = arguments
    if time_binary.is_file():
        command = [
            str(time_binary),
            "-f",
            "max_rss_kib=%M",
            "-o",
            str(timing_path),
            *arguments,
        ]
    started = time.perf_counter()
    try:
        completed = checked_run(command, cwd=cwd, env=env)
        elapsed = time.perf_counter() - started
        rss: int | None = None
        if time_binary.is_file():
            match = re.search(r"max_rss_kib=(\d+)", timing_path.read_text())
            rss = int(match.group(1)) if match else None
        return elapsed, rss, completed.stdout, completed.stderr
    finally:
        timing_path.unlink(missing_ok=True)


def count_source(repository: Path) -> tuple[int, int]:
    files = 0
    size = 0
    for root, directories, names in os.walk(repository):
        directories[:] = [name for name in directories if name not in {".git", ".ctx"}]
        for name in names:
            path = Path(root) / name
            try:
                size += path.stat().st_size
                files += 1
            except OSError:
                pass
    return files, size


def ctx_index(
    repositories: list[dict[str, Any]],
    workspace: Path,
    ctx_binary: Path,
    output: Path,
) -> None:
    if output.exists():
        raise RuntimeError(f"refusing to overwrite result: {output}")
    run_root = workspace / "runs" / f"ctx-{datetime.now(UTC).strftime('%Y%m%dT%H%M%SZ')}"
    run_root.mkdir(parents=True)
    results: list[dict[str, Any]] = []
    for repository in repositories:
        source = workspace / "repos" / repository["id"]
        index_dir = run_root / "indexes" / repository["id"]
        index_dir.mkdir(parents=True)
        environment = os.environ.copy()
        environment["CTX_DIR"] = str(index_dir)
        cold, cold_rss, _, _ = timed_command(
            [str(ctx_binary), "index", str(source)], cwd=source, env=environment
        )
        warm, warm_rss, _, _ = timed_command(
            [str(ctx_binary), "index", str(source)], cwd=source, env=environment
        )
        status = json.loads(
            checked_run(
                [str(ctx_binary), "status", "--json"], cwd=source, env=environment
            ).stdout
        )
        files, source_bytes = count_source(source)
        result = {
            "id": repository["id"],
            "language": repository["language"],
            "commit": repository["commit"],
            "source_files": files,
            "source_bytes": source_bytes,
            "cold_index_seconds": round(cold, 6),
            "cold_max_rss_kib": cold_rss,
            "unchanged_index_seconds": round(warm, 6),
            "unchanged_max_rss_kib": warm_rss,
            "indexed_files": status["files"],
            "symbols": status["symbols"],
            "edges": status["edges"],
            "database_bytes": status["size_bytes"],
        }
        results.append(result)
        print(
            f"{repository['id']}: cold={cold:.3f}s unchanged={warm:.3f}s "
            f"indexed={status['files']}"
        )
    payload = {
        "schema": 1,
        "generated_at": datetime.now(UTC).isoformat(),
        "tool": {
            "name": "ctx",
            "version": checked_run([str(ctx_binary), "--version"]).stdout.strip(),
        },
        "environment": {
            "platform": platform.platform(),
            "python": platform.python_version(),
            "logical_cpus": os.cpu_count(),
        },
        "methodology": {
            "repositories": len(repositories),
            "cold_runs": 1,
            "unchanged_runs": 1,
            "indexes_outside_source_tree": True,
        },
        "repositories": results,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def score_answer(answer: str, checks: list[dict[str, Any]]) -> dict[str, Any]:
    facts = []
    for check in checks:
        path_found = check["path"] in answer
        symbol_found = check["symbol"].split(".")[-1] in answer
        line_pattern = rf"{re.escape(check['path'])}(?::|%3A){check['line']}(?:\D|$)"
        line_found = re.search(line_pattern, answer) is not None
        facts.append(
            {
                **check,
                "path_found": path_found,
                "symbol_found": symbol_found,
                "citation_found": line_found,
                "passed": path_found and symbol_found and line_found,
            }
        )
    passed = sum(1 for fact in facts if fact["passed"])
    return {"passed": passed, "total": len(facts), "facts": facts}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_REPOS)
    subparsers = parser.add_subparsers(dest="command", required=True)
    for name in ("prepare", "verify"):
        command = subparsers.add_parser(name)
        command.add_argument("--workspace", type=Path, required=True)
    index = subparsers.add_parser("ctx-index")
    index.add_argument("--workspace", type=Path, required=True)
    index.add_argument("--ctx-binary", type=Path, required=True)
    index.add_argument("--output", type=Path, required=True)
    score = subparsers.add_parser("score")
    score.add_argument("--repo", required=True)
    score.add_argument("--answer", type=Path, required=True)

    arguments = parser.parse_args()
    repositories = load_manifest(arguments.manifest)
    if arguments.command == "prepare":
        prepare(repositories, arguments.workspace.resolve())
    elif arguments.command == "verify":
        verify(repositories, arguments.workspace.resolve())
    elif arguments.command == "ctx-index":
        verify(repositories, arguments.workspace.resolve())
        ctx_index(
            repositories,
            arguments.workspace.resolve(),
            arguments.ctx_binary.resolve(),
            arguments.output.resolve(),
        )
    else:
        repository = next(
            (item for item in repositories if item["id"] == arguments.repo), None
        )
        if repository is None:
            parser.error(f"unknown repository: {arguments.repo}")
        result = score_answer(arguments.answer.read_text(encoding="utf-8"), repository["checks"])
        print(json.dumps(result, indent=2))
        if result["passed"] != result["total"]:
            raise SystemExit(1)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"benchmark error: {error}", file=sys.stderr)
        raise SystemExit(2) from None
