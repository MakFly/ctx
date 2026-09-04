from __future__ import annotations

import shutil
from pathlib import Path

import pytest


@pytest.fixture()
def mini_repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    source = Path(__file__).parent / "fixtures" / "mini_repo"
    root = tmp_path / "mini_repo"
    shutil.copytree(source, root)
    monkeypatch.chdir(root)
    monkeypatch.setenv("CTX_DIR", str(root / ".ctx"))
    return root
