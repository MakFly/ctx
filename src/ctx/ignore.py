from __future__ import annotations

from pathlib import Path
from fnmatch import fnmatch
from typing import Any

try:
    import pathspec
except ImportError:  # Packaging installs pathspec; this keeps the core usable in minimal Python.
    pathspec = None  # type: ignore[assignment]

DEFAULT_IGNORES = (
    ".git/", ".ctx/", ".venv/", "venv/", "node_modules/", "dist/", "build/",
    ".next/", ".nuxt/", ".turbo/", ".cache/", ".output/", "out/",
    "coverage/", ".pytest_cache/", ".mypy_cache/", "__pycache__/", "*.pyc",
    "*.min.js", "*.map", "vendor/",
)


class _FallbackSpec:
    def __init__(self, lines: list[str]) -> None:
        self.patterns = [line.strip().lstrip("/") for line in lines if line.strip() and not line.lstrip().startswith("#")]

    def match_file(self, value: str) -> bool:
        normalized = value.rstrip("/")
        for pattern in self.patterns:
            candidate = pattern.rstrip("/")
            if fnmatch(normalized, candidate) or fnmatch(normalized, f"{candidate}/*"):
                return True
            if "/" not in candidate and candidate in normalized.split("/"):
                return True
        return False


def load_ignore(root: Path) -> Any:
    lines = list(DEFAULT_IGNORES)
    for name in (".gitignore", ".cursorignore", ".ctxignore"):
        path = root / name
        if path.is_file():
            lines.extend(path.read_text(encoding="utf-8", errors="replace").splitlines())
    if pathspec is None:
        return _FallbackSpec(lines)
    return pathspec.PathSpec.from_lines("gitwildmatch", lines)


def is_ignored(path: Path, root: Path, spec: Any) -> bool:
    rel = path.relative_to(root).as_posix()
    return spec.match_file(rel + ("/" if path.is_dir() else ""))
