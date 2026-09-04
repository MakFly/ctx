from __future__ import annotations

import os
import subprocess
import time
from concurrent.futures import ProcessPoolExecutor
from dataclasses import dataclass
from pathlib import Path

from .config import EXCERPT_SIZE, MAX_FILE_SIZE, SUPPORTED_EXTENSIONS, TEXT_EXTENSIONS, ctx_dir
from .db import connect, rebuild_fts, set_meta
from .hashutil import content_hash
from .ignore import is_ignored, load_ignore
from .parse import Edge, Symbol, parse_source
from .gitinfo import git_info


@dataclass(frozen=True)
class IndexResult:
    files: int
    changed: int
    symbols: int
    edges: int
    database: Path


@dataclass(frozen=True)
class PreparedFile:
    path: str
    lang: str | None
    size: int
    mtime: float
    digest: str
    is_test: int
    is_vendor: int
    excerpt: str
    symbols: list[Symbol]
    edges: list[Edge]


def _binary(data: bytes) -> bool:
    return b"\0" in data[:8_192]


def _is_test_file(rel: str) -> int:
    path = Path(rel)
    lower = path.name.lower()
    return int(
        any(part.lower() in {"test", "tests", "spec", "specs", "__tests__"} for part in path.parts[:-1])
        or lower.startswith("test_")
        or lower.endswith(("_test.go", "_test.py", "test.php"))
        or any(marker in lower for marker in (".test.", ".spec."))
    )


def walk_files(root: Path) -> list[Path]:
    spec = load_ignore(root)
    git_paths = _git_file_list(root)
    if git_paths is not None:
        found = []
        for rel in git_paths:
            path = root / rel
            if is_ignored(path, root, spec) or path.suffix.lower() not in TEXT_EXTENSIONS:
                continue
            try:
                if path.is_file() and path.stat().st_size <= MAX_FILE_SIZE:
                    found.append(path)
            except OSError:
                continue
        return sorted(found, key=lambda path: path.relative_to(root).as_posix())
    found: list[Path] = []
    for base, dirs, names in os.walk(root):
        base_path = Path(base)
        dirs[:] = sorted(d for d in dirs if not is_ignored(base_path / d, root, spec))
        for name in sorted(names):
            path = base_path / name
            if is_ignored(path, root, spec) or path.suffix.lower() not in TEXT_EXTENSIONS:
                continue
            try:
                if path.stat().st_size <= MAX_FILE_SIZE:
                    found.append(path)
            except OSError:
                continue
    return found


def _git_file_list(root: Path) -> list[Path] | None:
    try:
        result = subprocess.run(
            ["git", "ls-files", "-co", "--exclude-standard", "-z", "--", "."],
            cwd=root, capture_output=True, check=True, timeout=10,
        )
    except (FileNotFoundError, subprocess.SubprocessError):
        return None
    return [Path(raw.decode("utf-8", errors="surrogateescape")) for raw in result.stdout.split(b"\0") if raw]


def index_repository(root: Path) -> IndexResult:
    root = root.resolve()
    if not root.is_dir():
        raise ValueError(f"répertoire introuvable: {root}")
    database = ctx_dir(root) / "index.sqlite"
    conn = connect(database, create=True)
    current_paths: set[str] = set()
    changed = 0
    removed = False
    try:
        paths = walk_files(root)
        if conn.execute("SELECT count(*) FROM files").fetchone()[0] == 0:
            prepared = _prepare_files(root, paths)
            _insert_cold(conn, prepared)
            current_paths = {item.path for item in prepared}
            changed = len(prepared)
        else:
            changed, current_paths = _update_existing(conn, root, paths)
        for row in conn.execute("SELECT id,path FROM files").fetchall():
            if row["path"] not in current_paths:
                removed = True
                conn.execute("DELETE FROM edges WHERE file_id=?", (row["id"],))
                conn.execute("DELETE FROM symbols WHERE file_id=?", (row["id"],))
                conn.execute("DELETE FROM files WHERE id=?", (row["id"],))
        if changed or removed:
            conn.execute("DELETE FROM edges WHERE source='lsp'")
        _resolve_edges(conn)
        rebuild_fts(conn)
        set_meta(conn, "repo_root", str(root))
        set_meta(conn, "indexed_at", str(time.time()))
        set_meta(conn, "indexed_sha", git_info(root).sha)
        conn.commit()
        counts = [conn.execute(f"SELECT count(*) FROM {table}").fetchone()[0] for table in ("files", "symbols", "edges")]
        return IndexResult(counts[0], changed, counts[1], counts[2], database)
    finally:
        conn.close()


def _update_existing(conn, root: Path, paths: list[Path]) -> tuple[int, set[str]]:
    current_paths: set[str] = set()
    changed = 0
    for path in paths:
            rel = path.relative_to(root).as_posix()
            stat = path.stat()
            row = conn.execute("SELECT id,size,mtime,content_hash FROM files WHERE path=?", (rel,)).fetchone()
            current_paths.add(rel)
            if row and row["size"] == stat.st_size and abs(float(row["mtime"]) - stat.st_mtime) < 1e-6:
                continue
            data = path.read_bytes()
            if _binary(data):
                continue
            digest = content_hash(data)
            if row and row["content_hash"] == digest:
                conn.execute("UPDATE files SET size=?,mtime=? WHERE id=?", (stat.st_size, stat.st_mtime, row["id"]))
                continue
            changed += 1
            text = data.decode("utf-8", errors="replace")
            lang = SUPPORTED_EXTENSIONS.get(path.suffix.lower())
            rel_parts = Path(rel).parts
            is_test = _is_test_file(rel)
            is_vendor = int("vendor" in rel_parts or "node_modules" in rel_parts)
            conn.execute(
                "INSERT INTO files(path,lang,size,mtime,content_hash,is_test,is_vendor) VALUES(?,?,?,?,?,?,?) "
                "ON CONFLICT(path) DO UPDATE SET lang=excluded.lang,size=excluded.size,mtime=excluded.mtime,content_hash=excluded.content_hash,is_test=excluded.is_test,is_vendor=excluded.is_vendor",
                (rel, lang, stat.st_size, stat.st_mtime, digest, is_test, is_vendor),
            )
            file_id = conn.execute("SELECT id FROM files WHERE path=?", (rel,)).fetchone()[0]
            conn.execute(
                "INSERT INTO file_excerpts(file_id,excerpt) VALUES(?,?) ON CONFLICT(file_id) DO UPDATE SET excerpt=excluded.excerpt",
                (file_id, text[:EXCERPT_SIZE]),
            )
            conn.execute("DELETE FROM edges WHERE file_id=?", (file_id,))
            conn.execute("DELETE FROM symbols WHERE file_id=?", (file_id,))
            if lang:
                symbols, edges = parse_source(text, lang)
                conn.executemany(
                    "INSERT INTO symbols(file_id,name,qualname,kind,start,end,sig,snippet) VALUES(?,?,?,?,?,?,?,?)",
                    [
                        (file_id, symbol.name, symbol.qualname, symbol.kind, symbol.start, symbol.end,
                         symbol.sig, symbol.snippet[:EXCERPT_SIZE])
                        for symbol in symbols
                    ],
                )
                ids = {
                    row["name"]: int(row["id"])
                    for row in conn.execute("SELECT id,name FROM symbols WHERE file_id=? ORDER BY id", (file_id,))
                }
                conn.executemany(
                    "INSERT INTO edges(src_symbol_id,dst_name,kind,file_id,line) VALUES(?,?,?,?,?)",
                    [
                        (ids.get(edge.src_name or ""), edge.dst_name, edge.kind, file_id, edge.line)
                        for edge in edges
                    ],
                )
    return changed, current_paths


def _prepare_file(args: tuple[str, str]) -> PreparedFile | None:
    root_raw, path_raw = args
    root = Path(root_raw)
    path = Path(path_raw)
    rel = path.relative_to(root).as_posix()
    data = path.read_bytes()
    if _binary(data):
        return None
    stat = path.stat()
    text = data.decode("utf-8", errors="replace")
    lang = SUPPORTED_EXTENSIONS.get(path.suffix.lower())
    symbols, edges = parse_source(text, lang) if lang else ([], [])
    parts = Path(rel).parts
    return PreparedFile(
        path=rel, lang=lang, size=stat.st_size, mtime=stat.st_mtime,
        digest=content_hash(data), is_test=_is_test_file(rel),
        is_vendor=int("vendor" in parts or "node_modules" in parts), excerpt=text[:EXCERPT_SIZE],
        symbols=symbols, edges=edges,
    )


def _prepare_files(root: Path, paths: list[Path]) -> list[PreparedFile]:
    args = [(str(root), str(path)) for path in paths]
    requested = int(os.environ.get("CTX_INDEX_WORKERS", "0") or 0)
    workers = requested or min(8, os.cpu_count() or 1)
    if len(paths) < 100 or workers <= 1:
        return [item for item in map(_prepare_file, args) if item is not None]
    try:
        with ProcessPoolExecutor(max_workers=workers) as executor:
            return [item for item in executor.map(_prepare_file, args, chunksize=8) if item is not None]
    except (OSError, RuntimeError):
        return [item for item in map(_prepare_file, args) if item is not None]


def _insert_cold(conn, prepared: list[PreparedFile]) -> None:
    conn.executemany(
        "INSERT INTO files(path,lang,size,mtime,content_hash,is_test,is_vendor) VALUES(?,?,?,?,?,?,?)",
        [(item.path, item.lang, item.size, item.mtime, item.digest, item.is_test, item.is_vendor) for item in prepared],
    )
    file_ids = {row["path"]: int(row["id"]) for row in conn.execute("SELECT id,path FROM files")}
    conn.executemany(
        "INSERT INTO file_excerpts(file_id,excerpt) VALUES(?,?)",
        [(file_ids[item.path], item.excerpt) for item in prepared],
    )
    conn.executemany(
        "INSERT INTO symbols(file_id,name,qualname,kind,start,end,sig,snippet) VALUES(?,?,?,?,?,?,?,?)",
        [
            (file_ids[item.path], symbol.name, symbol.qualname, symbol.kind, symbol.start,
             symbol.end, symbol.sig, symbol.snippet[:EXCERPT_SIZE])
            for item in prepared for symbol in item.symbols
        ],
    )
    symbol_ids: dict[tuple[int, str], int] = {}
    for row in conn.execute("SELECT id,file_id,name FROM symbols ORDER BY id"):
        symbol_ids[(int(row["file_id"]), row["name"])] = int(row["id"])
    conn.executemany(
        "INSERT INTO edges(src_symbol_id,dst_name,kind,file_id,line) VALUES(?,?,?,?,?)",
        [
            (symbol_ids.get((file_ids[item.path], edge.src_name or "")), edge.dst_name,
             edge.kind, file_ids[item.path], edge.line)
            for item in prepared for edge in item.edges
        ],
    )


def _resolve_edges(conn) -> None:
    conn.execute("UPDATE edges SET dst_symbol_id=NULL WHERE source='parser'")
    conn.execute("""
        UPDATE edges SET dst_symbol_id=(
          SELECT s.id FROM symbols s JOIN files f ON f.id=s.file_id
          WHERE s.name=edges.dst_name ORDER BY f.is_test, f.is_vendor, f.path, s.start LIMIT 1
        ) WHERE source='parser' AND EXISTS(SELECT 1 FROM symbols s WHERE s.name=edges.dst_name)
    """)
