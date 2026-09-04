from __future__ import annotations

import os
from pathlib import Path

MAX_FILE_SIZE = 1_048_576
EXCERPT_SIZE = 800
SUPPORTED_EXTENSIONS = {
    ".py": "python",
    ".pyi": "python",
    ".pyw": "python",
    ".js": "javascript",
    ".jsx": "javascript",
    ".mjs": "javascript",
    ".cjs": "javascript",
    ".ts": "typescript",
    ".tsx": "tsx",
    ".mts": "typescript",
    ".cts": "typescript",
    ".vue": "typescript",
    ".svelte": "typescript",
    ".astro": "typescript",
    ".go": "go",
    ".rs": "rust",
    ".php": "php",
    ".phtml": "php",
}
TEXT_EXTENSIONS = set(SUPPORTED_EXTENSIONS) | {
    ".md", ".txt", ".toml", ".json", ".yaml", ".yml", ".ini", ".cfg",
    ".html", ".css", ".scss", ".sql", ".sh",
}


def repo_root(path: Path | str = ".") -> Path:
    return Path(path).expanduser().resolve()


def ctx_dir(root: Path) -> Path:
    override = os.environ.get("CTX_DIR")
    if override:
        value = Path(override).expanduser()
        return (value if value.is_absolute() else root / value).resolve()
    return root / ".ctx"


def find_ctx(start: Path | str = ".") -> Path:
    override = os.environ.get("CTX_DIR")
    if override:
        value = Path(override).expanduser()
        return (value if value.is_absolute() else Path(start).resolve() / value).resolve()
    current = Path(start).resolve()
    for candidate in (current, *current.parents):
        if (candidate / ".ctx" / "index.sqlite").exists():
            return candidate / ".ctx"
    return current / ".ctx"
