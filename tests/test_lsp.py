import json
import sys
from pathlib import Path

from ctx.graph import graph_query
from ctx.db import connect
from ctx.index import index_repository
from ctx.lsp import _select_asset, sources, status
from ctx.lsp_enrich import enrich


def test_lsp_sources_use_official_github_repositories():
    repositories = {item["language"]: item["repository"] for item in sources()}
    assert repositories == {
        "python": "DetachHead/basedpyright",
        "typescript": "microsoft/typescript-go",
        "go": "golang/tools",
        "rust": "rust-lang/rust-analyzer",
        "php": "phpactor/phpactor",
    }
    assert all(item["github"].startswith("https://github.com/") for item in sources())


def test_lsp_status_detects_existing_server(tmp_path: Path):
    binary = tmp_path / "gopls"
    binary.write_text("#!/bin/sh\n", encoding="utf-8")
    binary.chmod(0o755)
    result = status(tmp_path, path_env=str(tmp_path))
    go = next(item for item in result["servers"] if item["language"] == "go")
    assert go["available"] is True
    assert go["origin"] == "path"
    assert go["command"] == [str(binary)]


def test_release_asset_selection(monkeypatch):
    monkeypatch.setattr("ctx.lsp._platform", lambda: ("linux", "x64"))
    assets = [
        {"name": "typescript-linux-arm64.tgz"},
        {"name": "typescript-linux-x64.tgz"},
    ]
    assert _select_asset("typescript", assets) == assets[1]
    rust = [{"name": "rust-analyzer-x86_64-unknown-linux-gnu.gz"}]
    assert _select_asset("rust", rust) == rust[0]


def test_lsp_enrichment_adds_high_confidence_reference(tmp_path: Path, monkeypatch):
    root = tmp_path / "repo"
    root.mkdir()
    (root / "target.py").write_text("def foo():\n    return 1\n", encoding="utf-8")
    caller = root / "caller.py"
    caller.write_text("foo()\n", encoding="utf-8")
    monkeypatch.setenv("CTX_DIR", str(root / ".ctx"))
    index_repository(root)

    server = tmp_path / "fake_lsp.py"
    server.write_text(
        """import json, sys
def send(value):
    raw=json.dumps(value,separators=(',',':')).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(raw)}\\r\\n\\r\\n'.encode()+raw)
    sys.stdout.buffer.flush()
while True:
    headers={}
    while (line := sys.stdin.buffer.readline()) not in (b'\\r\\n', b'\\n', b''):
        key, _, value=line.decode().partition(':'); headers[key.lower()]=value.strip()
    if not headers: break
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method')
    if method == 'exit': break
    if 'id' not in message: continue
    if method == 'initialize': result={'capabilities': {'referencesProvider': True}}
    elif method == 'textDocument/references':
        result=[{'uri': sys.argv[1], 'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 3}}}]
    else: result=None
    send({'jsonrpc':'2.0','id':message['id'],'result':result})
""",
        encoding="utf-8",
    )
    manifest = root / ".ctx" / "lsp" / "servers.json"
    manifest.parent.mkdir(parents=True)
    manifest.write_text(json.dumps({"python": {"command": [sys.executable, str(server), caller.as_uri()]}}), encoding="utf-8")

    result = enrich(root, ["python"], max_symbols=10, timeout=5)

    assert result["edges"] == 1
    callers = graph_query("callers", "foo", start=root)
    hit = next(hit for hit in callers["hits"] if hit["path"] == "caller.py")
    assert hit["why"] == "lsp ref of foo"
    assert hit["score"] == 1.0

    index_repository(root)
    with connect(root / ".ctx" / "index.sqlite") as conn:
        assert conn.execute("SELECT count(*) FROM edges WHERE source='lsp'").fetchone()[0] == 1
    (root / "target.py").write_text("def foo():\n    return 2\n", encoding="utf-8")
    index_repository(root)
    with connect(root / ".ctx" / "index.sqlite") as conn:
        assert conn.execute("SELECT count(*) FROM edges WHERE source='lsp'").fetchone()[0] == 0
