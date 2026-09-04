from __future__ import annotations

import sqlite3
from pathlib import Path

SCHEMA_VERSION = 2

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS files(
  id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, lang TEXT, size INTEGER,
  mtime REAL, content_hash TEXT, is_test INTEGER DEFAULT 0, is_vendor INTEGER DEFAULT 0
);
CREATE TABLE IF NOT EXISTS symbols(
  id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  name TEXT NOT NULL, qualname TEXT, kind TEXT, start INTEGER, end INTEGER,
  sig TEXT, snippet TEXT
);
CREATE TABLE IF NOT EXISTS edges(
  id INTEGER PRIMARY KEY, src_symbol_id INTEGER, dst_name TEXT NOT NULL,
  dst_symbol_id INTEGER, kind TEXT, file_id INTEGER, line INTEGER,
  source TEXT NOT NULL DEFAULT 'parser', confidence REAL NOT NULL DEFAULT 0.65
);
CREATE TABLE IF NOT EXISTS file_excerpts(
  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE, excerpt TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_edges_dst_name ON edges(dst_name);
CREATE INDEX IF NOT EXISTS idx_edges_dst_symbol_id ON edges(dst_symbol_id);
CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(path, excerpt);
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(name, qualname, snippet);
"""


def connect(path: Path, *, create: bool = False) -> sqlite3.Connection:
    if not create and not path.exists():
        raise FileNotFoundError(f"index absent: {path}; lancez `ctx index [path]`")
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys=ON")
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")
    conn.execute("PRAGMA temp_store=MEMORY")
    if create:
        try:
            conn.executescript(SCHEMA)
        except sqlite3.OperationalError as exc:
            if "fts5" in str(exc).lower():
                raise RuntimeError("SQLite système compilé sans FTS5") from exc
            raise
        set_meta(conn, "schema_version", str(SCHEMA_VERSION))
    if conn.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='edges'").fetchone():
        _migrate(conn)
    return conn


def _migrate(conn: sqlite3.Connection) -> None:
    columns = {row[1] for row in conn.execute("PRAGMA table_info(edges)")}
    if "source" not in columns:
        conn.execute("ALTER TABLE edges ADD COLUMN source TEXT NOT NULL DEFAULT 'parser'")
    if "confidence" not in columns:
        conn.execute("ALTER TABLE edges ADD COLUMN confidence REAL NOT NULL DEFAULT 0.65")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source)")


def set_meta(conn: sqlite3.Connection, key: str, value: str) -> None:
    conn.execute(
        "INSERT INTO meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        (key, value),
    )


def get_meta(conn: sqlite3.Connection, key: str, default: str = "") -> str:
    row = conn.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
    return str(row[0]) if row else default


def rebuild_fts(conn: sqlite3.Connection) -> None:
    conn.execute("DELETE FROM files_fts")
    conn.execute("DELETE FROM symbols_fts")
    conn.execute(
        "INSERT INTO files_fts(rowid,path,excerpt) SELECT f.id,f.path,COALESCE(x.excerpt,'') FROM files f LEFT JOIN file_excerpts x ON x.file_id=f.id"
    )
    conn.execute(
        "INSERT INTO symbols_fts(rowid,name,qualname,snippet) SELECT id,name,COALESCE(qualname,''),COALESCE(snippet,'') FROM symbols"
    )
