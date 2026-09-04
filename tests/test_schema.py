from __future__ import annotations

from ctx.db import connect


def test_schema_has_required_tables(tmp_path):
    conn = connect(tmp_path / "index.sqlite", create=True)
    names = {row[0] for row in conn.execute("SELECT name FROM sqlite_master")}
    assert {"files", "symbols", "edges", "files_fts", "symbols_fts"} <= names
    edge_columns = {row[1] for row in conn.execute("PRAGMA table_info(edges)")}
    assert {"source", "confidence"} <= edge_columns


def test_schema_migrates_existing_edges_table(tmp_path):
    import sqlite3

    database = tmp_path / "old.sqlite"
    legacy = sqlite3.connect(database)
    legacy.execute("CREATE TABLE edges(id INTEGER PRIMARY KEY, dst_name TEXT)")
    legacy.commit()
    legacy.close()

    conn = connect(database)
    columns = {row[1] for row in conn.execute("PRAGMA table_info(edges)")}
    conn.close()
    assert {"source", "confidence"} <= columns
