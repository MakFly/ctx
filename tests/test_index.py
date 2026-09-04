import subprocess

from ctx.index import index_repository, walk_files


def test_index_creates_database_and_symbols(mini_repo):
    generated = mini_repo / "apps" / "web" / ".next"
    generated.mkdir(parents=True)
    (generated / "bundle.js").write_text("function generated() {}", encoding="utf-8")
    result = index_repository(mini_repo)
    assert result.database.exists()
    assert result.files == 10
    assert result.symbols >= 16
    again = index_repository(mini_repo)
    assert again.changed == 0


def test_git_walk_honors_nested_gitignore(tmp_path):
    root = tmp_path / "repo"
    generated = root / "apps" / "web" / "generated"
    generated.mkdir(parents=True)
    (root / "apps" / "web" / ".gitignore").write_text("generated/\n", encoding="utf-8")
    (root / "app.py").write_text("def main(): pass\n", encoding="utf-8")
    (generated / "cache.py").write_text("def stale(): pass\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)

    paths = {path.relative_to(root).as_posix() for path in walk_files(root)}

    assert "app.py" in paths
    assert "apps/web/generated/cache.py" not in paths
