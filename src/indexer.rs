use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use rayon::prelude::*;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha1::{Digest, Sha1};

use crate::config::{
    EXCERPT_SIZE, MAX_FILE_SIZE, ctx_dir, is_text, language, relative_path, repo_root,
};
use crate::db::{connect, rebuild_fts, set_meta};
use crate::gitinfo::git_info;
use crate::model::{Edge, Symbol};
use crate::parser::parse_source;

#[derive(Debug, Clone, Serialize)]
pub struct IndexResult {
    pub files: usize,
    pub changed: usize,
    pub symbols: usize,
    pub edges: usize,
    pub database: PathBuf,
}

#[derive(Debug)]
struct PreparedFile {
    path: String,
    language: Option<String>,
    size: u64,
    mtime: f64,
    digest: String,
    is_test: bool,
    is_vendor: bool,
    excerpt: String,
    symbols: Vec<Symbol>,
    edges: Vec<Edge>,
}

#[derive(Debug)]
struct ExistingFile {
    id: i64,
    size: u64,
    mtime: f64,
    digest: String,
}

pub fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".ctxignore")
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git"
                    | ".ctx"
                    | ".venv"
                    | "venv"
                    | "node_modules"
                    | "vendor"
                    | "target"
                    | "dist"
                    | "build"
                    | "__pycache__"
                    | ".next"
                    | ".nuxt"
            )
        });
    let mut paths = Vec::new();
    for entry in builder.build() {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) || !is_text(entry.path()) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() <= MAX_FILE_SIZE {
            paths.push(entry.into_path());
        }
    }
    paths.sort_by_key(|path| relative_path(path, root).unwrap_or_default());
    Ok(paths)
}

pub fn index_repository(path: impl AsRef<Path>) -> Result<IndexResult> {
    let root = repo_root(path)?;
    let database = ctx_dir(&root).join("index.sqlite");
    let mut connection = connect(&database, true)?;
    let paths = walk_files(&root)?;
    let existing = existing_files(&connection)?;
    let current = paths
        .iter()
        .filter_map(|path| relative_path(path, &root).ok())
        .collect::<HashSet<_>>();
    let changed_paths = paths
        .into_iter()
        .filter(|path| {
            let Ok(relative) = relative_path(path, &root) else {
                return false;
            };
            let Ok(metadata) = path.metadata() else {
                return false;
            };
            let mtime = modified_seconds(&metadata);
            existing.get(&relative).is_none_or(|item| {
                item.size != metadata.len() || (item.mtime - mtime).abs() > 0.000_001
            })
        })
        .collect::<Vec<_>>();
    let prepared = changed_paths
        .par_iter()
        .filter_map(|path| prepare_file(&root, path).transpose())
        .collect::<Result<Vec<_>>>()?;

    let transaction = connection.transaction()?;
    let mut changed = 0;
    for item in prepared {
        if let Some(previous) = existing.get(&item.path)
            && previous.digest == item.digest
        {
            transaction.execute(
                "UPDATE files SET size=?1,mtime=?2 WHERE id=?3",
                params![item.size, item.mtime, previous.id],
            )?;
            continue;
        }
        changed += 1;
        upsert_file(&transaction, &item)?;
    }
    let removed = existing
        .iter()
        .filter(|(path, _)| !current.contains(*path))
        .map(|(_, file)| file.id)
        .collect::<Vec<_>>();
    for file_id in &removed {
        transaction.execute("DELETE FROM edges WHERE file_id=?1", [file_id])?;
        transaction.execute("DELETE FROM symbols WHERE file_id=?1", [file_id])?;
        transaction.execute("DELETE FROM file_excerpts WHERE file_id=?1", [file_id])?;
        transaction.execute("DELETE FROM files WHERE id=?1", [file_id])?;
    }
    if changed > 0 || !removed.is_empty() {
        transaction.execute("DELETE FROM edges WHERE source='lsp'", [])?;
    }
    resolve_edges(&transaction)?;
    rebuild_fts(&transaction)?;
    let git = git_info(&root);
    set_meta(&transaction, "repo_root", &root.to_string_lossy())?;
    set_meta(
        &transaction,
        "indexed_at",
        &chrono::Utc::now().timestamp_millis().to_string(),
    )?;
    set_meta(&transaction, "indexed_sha", &git.sha)?;
    transaction.commit()?;

    let files = count(&connection, "files")?;
    let symbols = count(&connection, "symbols")?;
    let edges = count(&connection, "edges")?;
    Ok(IndexResult {
        files,
        changed,
        symbols,
        edges,
        database,
    })
}

fn existing_files(connection: &rusqlite::Connection) -> Result<HashMap<String, ExistingFile>> {
    let mut statement =
        connection.prepare("SELECT id,path,size,mtime,content_hash FROM files ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            ExistingFile {
                id: row.get(0)?,
                size: row.get::<_, i64>(2)?.max(0) as u64,
                mtime: row.get(3)?,
                digest: row.get(4)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

fn prepare_file(root: &Path, path: &Path) -> Result<Option<PreparedFile>> {
    let bytes =
        fs::read(path).with_context(|| format!("lecture impossible: {}", path.display()))?;
    if bytes.iter().take(8_192).any(|byte| *byte == 0) {
        return Ok(None);
    }
    let metadata = path.metadata()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let language = language(path).map(str::to_owned);
    let (symbols, edges) = match language.as_deref() {
        Some(name) => parse_source(&text, name)?,
        None => (Vec::new(), Vec::new()),
    };
    let relative = relative_path(path, root)?;
    let parts = Path::new(&relative)
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_test = parts.iter().any(|part| {
        matches!(
            part.as_str(),
            "test" | "tests" | "spec" | "specs" | "__tests__"
        )
    }) || filename.starts_with("test_")
        || filename.ends_with("_test.go")
        || filename.contains(".test.")
        || filename.contains(".spec.");
    let is_vendor = parts
        .iter()
        .any(|part| matches!(part.as_str(), "vendor" | "node_modules"));
    Ok(Some(PreparedFile {
        path: relative,
        language,
        size: metadata.len(),
        mtime: modified_seconds(&metadata),
        digest: format!("{:x}", Sha1::digest(&bytes)),
        is_test,
        is_vendor,
        excerpt: truncate(&text, EXCERPT_SIZE),
        symbols,
        edges,
    }))
}

fn upsert_file(connection: &rusqlite::Connection, item: &PreparedFile) -> Result<()> {
    connection.execute(
        "INSERT INTO files(path,lang,size,mtime,content_hash,is_test,is_vendor)
         VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(path) DO UPDATE SET
           lang=excluded.lang,size=excluded.size,mtime=excluded.mtime,
           content_hash=excluded.content_hash,is_test=excluded.is_test,is_vendor=excluded.is_vendor",
        params![
            item.path,
            item.language,
            item.size,
            item.mtime,
            item.digest,
            item.is_test as i64,
            item.is_vendor as i64
        ],
    )?;
    let file_id: i64 =
        connection.query_row("SELECT id FROM files WHERE path=?1", [&item.path], |row| {
            row.get(0)
        })?;
    connection.execute(
        "INSERT INTO file_excerpts(file_id,excerpt) VALUES(?1,?2)
         ON CONFLICT(file_id) DO UPDATE SET excerpt=excluded.excerpt",
        params![file_id, item.excerpt],
    )?;
    connection.execute("DELETE FROM edges WHERE file_id=?1", [file_id])?;
    connection.execute("DELETE FROM symbols WHERE file_id=?1", [file_id])?;
    for symbol in &item.symbols {
        connection.execute(
            "INSERT INTO symbols(file_id,name,qualname,kind,start,end,sig,snippet)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                file_id,
                symbol.name,
                symbol.qualname,
                symbol.kind,
                symbol.start,
                symbol.end,
                symbol.sig,
                truncate(&symbol.snippet, 2_000)
            ],
        )?;
    }
    let mut symbol_ids = HashMap::new();
    let mut statement = connection.prepare("SELECT id,name FROM symbols WHERE file_id=?1")?;
    for row in statement.query_map([file_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, name) = row?;
        symbol_ids.insert(name, id);
    }
    for edge in &item.edges {
        connection.execute(
            "INSERT INTO edges(src_symbol_id,dst_name,kind,file_id,line)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                edge.src_name.as_ref().and_then(|name| symbol_ids.get(name)),
                edge.dst_name,
                edge.kind,
                file_id,
                edge.line
            ],
        )?;
    }
    Ok(())
}

fn resolve_edges(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute_batch(
        "UPDATE edges SET dst_symbol_id=NULL WHERE source='parser';
         UPDATE edges SET dst_symbol_id=(
           SELECT s.id FROM symbols s JOIN files f ON f.id=s.file_id
           WHERE s.name=edges.dst_name
           ORDER BY f.is_test,f.is_vendor,f.path,s.start LIMIT 1
         )
         WHERE source='parser'
           AND EXISTS(SELECT 1 FROM symbols s WHERE s.name=edges.dst_name);",
    )?;
    Ok(())
}

fn count(connection: &rusqlite::Connection, table: &str) -> Result<usize> {
    let sql = format!("SELECT count(*) FROM {table}");
    Ok(connection.query_row(&sql, [], |row| row.get::<_, i64>(0))? as usize)
}

fn modified_seconds(metadata: &fs::Metadata) -> f64 {
    metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut boundary = limit;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

pub fn index_root_from_database(start: impl AsRef<Path>) -> Result<PathBuf> {
    let database = crate::config::find_ctx(start)?.join("index.sqlite");
    let connection = connect(&database, false)?;
    let value: Option<String> = connection
        .query_row("SELECT value FROM meta WHERE key='repo_root'", [], |row| {
            row.get(0)
        })
        .optional()?;
    value
        .map(PathBuf::from)
        .context("repo_root absent de l'index")
}
