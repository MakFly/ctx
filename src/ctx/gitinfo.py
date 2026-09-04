from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class GitInfo:
    sha: str
    dirty: bool


def git_info(root: Path) -> GitInfo:
    try:
        sha = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"], cwd=root,
            text=True, capture_output=True, check=True, timeout=2,
        ).stdout.strip()
        dirty = bool(subprocess.run(
            ["git", "status", "--porcelain"], cwd=root,
            text=True, capture_output=True, check=True, timeout=3,
        ).stdout.strip())
        return GitInfo(sha=sha, dirty=dirty)
    except (FileNotFoundError, subprocess.SubprocessError):
        return GitInfo(sha="nogit", dirty=False)
