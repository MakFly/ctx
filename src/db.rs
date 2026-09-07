use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

pub const SCHEMA_VERSION: &str = "4";

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS files(
  id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, lang TEXT, size INTEGER,
  mtime REAL, content_hash TEXT, is_test INTEGER DEFAULT 0, is_vendor INTEGER DEFAULT 0
);
CREATE TABLE IF NOT EXISTS symbols(
  id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  name TEXT NOT NULL, qualname TEXT, kind TEXT, start INTEGER, end INTEGER,
  sig TEXT, snippet TEXT, snippet_truncated INTEGER NOT NULL DEFAULT 0
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
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source);
CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(path, excerpt);
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(name, qualname, snippet);
"#;

const TEXT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS file_contents(
  file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
  content TEXT NOT NULL,
  version TEXT NOT NULL DEFAULT ''
);
CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(
  content, content='file_contents', content_rowid='file_id'
);
CREATE TRIGGER IF NOT EXISTS contents_insert AFTER INSERT ON file_contents BEGIN
  INSERT INTO content_fts(rowid,content) VALUES(new.file_id,new.content);
END;
CREATE TRIGGER IF NOT EXISTS contents_delete AFTER DELETE ON file_contents BEGIN
  INSERT INTO content_fts(content_fts,rowid,content) VALUES('delete',old.file_id,old.content);
END;
CREATE TRIGGER IF NOT EXISTS contents_update AFTER UPDATE OF content ON file_contents BEGIN
  INSERT INTO content_fts(content_fts,rowid,content) VALUES('delete',old.file_id,old.content);
  INSERT INTO content_fts(rowid,content) VALUES(new.file_id,new.content);
END;
"#;

pub fn connect(path: &Path, create: bool) -> Result<Connection> {
    if !create && !path.is_file() {
        bail!(
            "index absent: {}; lancez `ctx index [path]`",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = (if create {
        Connection::open(path)
    } else {
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).or_else(
            |_| Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY),
        )
    })
    .with_context(|| format!("impossible d'ouvrir {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let current: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .ok();
    if !create && current.as_deref() == Some(SCHEMA_VERSION) {
        connection.execute_batch("PRAGMA foreign_keys=ON")?;
        return Ok(connection);
    }
    connection.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=MEMORY;",
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let transaction = connection.unchecked_transaction()?;
    migrate(&transaction)?;
    if create {
        transaction.execute_batch(SCHEMA).map_err(|error| {
            if error
                .to_string()
                .to_ascii_lowercase()
                .contains("no such module: fts5")
            {
                anyhow::anyhow!("SQLite compilé sans FTS5")
            } else {
                error.into()
            }
        })?;
        set_meta(&transaction, "schema_version", SCHEMA_VERSION)?;
    }
    transaction.execute_batch(TEXT_SCHEMA)?;
    set_meta(&transaction, "schema_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(connection)
}

fn migrate(connection: &Connection) -> Result<()> {
    let mut symbols = connection.prepare("PRAGMA table_info(symbols)")?;
    let columns = symbols
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.is_empty() && !columns.iter().any(|name| name == "snippet_truncated") {
        // Old indexes did not record truncation: be conservative until reindexed.
        connection.execute(
            "ALTER TABLE symbols ADD COLUMN snippet_truncated INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    let exists: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='edges'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(());
    }
    let mut statement = connection.prepare("PRAGMA table_info(edges)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "source") {
        connection.execute(
            "ALTER TABLE edges ADD COLUMN source TEXT NOT NULL DEFAULT 'parser'",
            [],
        )?;
    }
    if !columns.iter().any(|column| column == "confidence") {
        connection.execute(
            "ALTER TABLE edges ADD COLUMN confidence REAL NOT NULL DEFAULT 0.65",
            [],
        )?;
    }
    Ok(())
}

pub fn set_meta(connection: &Connection, key: &str, value: &str) -> Result<()> {
    connection.execute(
        "INSERT INTO meta(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn get_meta(connection: &Connection, key: &str, default: &str) -> Result<String> {
    Ok(connection
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()?
        .unwrap_or_else(|| default.to_owned()))
}

pub fn rebuild_fts(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "DELETE FROM files_fts;
         DELETE FROM symbols_fts;
         INSERT INTO files_fts(rowid,path,excerpt)
           SELECT f.id,f.path,COALESCE(x.excerpt,'')
           FROM files f LEFT JOIN file_excerpts x ON x.file_id=f.id;
         INSERT INTO symbols_fts(rowid,name,qualname,snippet)
           SELECT id,name,COALESCE(qualname,''),COALESCE(snippet,'') FROM symbols;",
    )?;
    Ok(())
}
