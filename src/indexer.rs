use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha1::{Digest, Sha1};

use crate::config::{
    EXCERPT_SIZE, MAX_FILE_SIZE, ctx_dir, index_settings, language, relative_path, repo_root,
};
use crate::db::{connect, get_meta, set_meta};
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
    content: String,
    version: String,
    symbols: Vec<Symbol>,
    edges: Vec<Edge>,
}

#[derive(Debug)]
struct ExistingFile {
    version: String,
    id: i64,
    size: u64,
    mtime: f64,
    digest: String,
}

#[derive(Debug, Default)]
pub struct WalkReport {
    pub paths: Vec<PathBuf>,
    pub directories: Vec<PathBuf>,
    pub excluded_too_large: usize,
    pub errors: usize,
}

pub fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(walk_report(root)?.paths)
}

pub fn walk_report(root: &Path) -> Result<WalkReport> {
    walk_report_inner(root, true)
}
pub(crate) fn walk_metadata_report(root: &Path) -> Result<WalkReport> {
    walk_report_inner(root, false)
}

fn walk_report_inner(root: &Path, inspect_binary: bool) -> Result<WalkReport> {
    let settings = index_settings(root)?;
    let excluded = ctx_dir(root)
        .canonicalize()
        .unwrap_or_else(|_| ctx_dir(root));
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".ctxignore")
        .filter_entry(move |entry| {
            if entry.path().starts_with(&excluded) {
                return false;
            }
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
    let mut report = WalkReport::default();
    for entry in builder.build() {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => {
                report.errors += 1;
                continue;
            }
        };
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            report.directories.push(entry.into_path());
            continue;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(value) => value,
            Err(_) => {
                report.errors += 1;
                continue;
            }
        };
        if metadata.len() > settings.max_file_bytes {
            report.excluded_too_large += 1;
            continue;
        }
        if !inspect_binary {
            report.paths.push(entry.into_path());
            continue;
        }
        let header = (|| -> std::io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            fs::File::open(entry.path())?
                .take(8192)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match header {
            Ok(bytes) if !ctx_tgrep::trigram::is_binary(&bytes) => {
                report.paths.push(entry.into_path())
            }
            Ok(_) => {}
            Err(_) => report.errors += 1,
        }
    }
    report
        .paths
        .sort_by_key(|path| relative_path(path, root).unwrap_or_default());
    Ok(report)
}

pub fn index_repository(path: impl AsRef<Path>) -> Result<IndexResult> {
    index_repository_with_options(path, false)
}

pub fn index_repository_with_options(path: impl AsRef<Path>, force: bool) -> Result<IndexResult> {
    let root = repo_root(path)?;
    let _lock = crate::text_index::WriterLock::try_acquire(&ctx_dir(&root))?
        .context("index writer already active; stop its watcher before manual indexing")?;
    let result = index_repository_locked(&root, force);
    if result.is_ok() {
        crate::watcher::clear_state_after_manual(&root);
    }
    result
}

pub(crate) fn index_repository_locked(root: &Path, force: bool) -> Result<IndexResult> {
    index_repository_locked_with_paths(root, force, &HashSet::new())
}

pub(crate) fn index_repository_locked_with_paths(
    root: &Path,
    force: bool,
    touched: &HashSet<PathBuf>,
) -> Result<IndexResult> {
    let database = ctx_dir(root).join("index.sqlite");
    let mut connection = connect(&database, true)?;
    let report = walk_metadata_report(root)?;
    // Do not turn an incomplete walk into deletions of unreadable subtrees.
    if report.errors > 0 {
        bail!("repository walk incomplete: {} errors", report.errors);
    }
    let excluded_too_large = report.excluded_too_large;
    let paths = report.paths;
    let existing = existing_files(&connection)?;
    let refresh_format = get_meta(&connection, "index_format", "")? != crate::text_index::FORMAT;
    let mut current = paths
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
            force
                || touched.iter().any(|changed| path.starts_with(changed))
                || refresh_format
                || existing.get(&relative).is_none_or(|item| {
                    item.size != metadata.len()
                        || (item.mtime - mtime).abs() > 0.000_001
                        || item.version
                            != format!("{:?}", ctx_tgrep::builder::file_version(&metadata))
                })
        })
        .collect::<Vec<_>>();
    let transaction = connection.transaction()?;
    let mut changed = 0;
    let mut changed_names = HashSet::new();
    // Stream one bounded document at a time. ASTs and content do not accumulate
    // with corpus size, and the trigram builder later reads this same snapshot.
    for path in changed_paths {
        let relative = relative_path(&path, root)?;
        let Some(item) = prepare_file(root, &path)? else {
            current.remove(&relative);
            continue;
        };
        if let Some(previous) = existing.get(&item.path)
            && previous.digest == item.digest
            && !refresh_format
        {
            transaction.execute(
                "UPDATE files SET size=?1,mtime=?2 WHERE id=?3",
                params![item.size, item.mtime, previous.id],
            )?;
            transaction.execute(
                "UPDATE file_contents SET version=?1 WHERE file_id=?2",
                params![item.version, previous.id],
            )?;
            continue;
        }
        changed += 1;
        changed_names.insert(item.path.clone());
        upsert_file(&transaction, &item)?;
    }
    let removed = existing
        .iter()
        .filter(|(path, _)| !current.contains(*path))
        .map(|(_, file)| file.id)
        .collect::<Vec<_>>();
    let removed_names: HashSet<String> = existing
        .keys()
        .filter(|path| !current.contains(*path))
        .cloned()
        .collect();
    for file_id in &removed {
        transaction.execute("DELETE FROM files_fts WHERE rowid=?1", [file_id])?;
        transaction.execute(
            "DELETE FROM symbols_fts WHERE rowid IN (SELECT id FROM symbols WHERE file_id=?1)",
            [file_id],
        )?;
        transaction.execute("DELETE FROM edges WHERE file_id=?1", [file_id])?;
        transaction.execute("DELETE FROM symbols WHERE file_id=?1", [file_id])?;
        transaction.execute("DELETE FROM file_excerpts WHERE file_id=?1", [file_id])?;
        transaction.execute("DELETE FROM files WHERE id=?1", [file_id])?;
    }
    if changed > 0 || !removed.is_empty() {
        transaction.execute("DELETE FROM edges WHERE source='lsp'", [])?;
    }
    if changed > 0 || !removed.is_empty() || refresh_format {
        resolve_edges(&transaction)?;
    }
    let staged =
        crate::text_index::stage_generation(&transaction, root, &changed_names, &removed_names)?;
    set_meta(
        &transaction,
        "excluded_too_large",
        &excluded_too_large.to_string(),
    )?;
    set_meta(
        &transaction,
        "text_max_file_bytes",
        &index_settings(root)?.max_file_bytes.to_string(),
    )?;
    let git = git_info(&root);
    set_meta(&transaction, "repo_root", &root.to_string_lossy())?;
    set_meta(
        &transaction,
        "indexed_at",
        &chrono::Utc::now().timestamp_millis().to_string(),
    )?;
    set_meta(&transaction, "indexed_sha", &git.sha)?;
    set_meta(&transaction, "index_format", crate::text_index::FORMAT)?;
    transaction.commit()?;
    if let Some(staged) = staged {
        staged.commit();
    }
    crate::text_index::cleanup_generations(&connection, root);

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
    let mut statement = connection.prepare(
        "SELECT f.id,f.path,f.size,f.mtime,f.content_hash,COALESCE(c.version,'')
            FROM files f LEFT JOIN file_contents c ON c.file_id=f.id ORDER BY f.path",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            ExistingFile {
                version: row.get(5)?,
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
    let Some((text, metadata, digest)) = read_document(path, index_settings(root)?.max_file_bytes)?
    else {
        return Ok(None);
    };
    let language = if metadata.len() <= MAX_FILE_SIZE {
        language(path).map(str::to_owned)
    } else {
        None
    };
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
        digest,
        is_test,
        is_vendor,
        excerpt: truncate(&text, EXCERPT_SIZE),
        content: text,
        version: format!("{:?}", ctx_tgrep::builder::file_version(&metadata)),
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
    connection.execute(
        "INSERT INTO file_contents(file_id,content,version) VALUES(?1,?2,?3)
        ON CONFLICT(file_id) DO UPDATE SET content=excluded.content,version=excluded.version",
        params![file_id, item.content, item.version],
    )?;
    connection.execute("DELETE FROM files_fts WHERE rowid=?1", [file_id])?;
    connection.execute(
        "INSERT INTO files_fts(rowid,path,excerpt) VALUES(?1,?2,?3)",
        params![file_id, item.path, item.excerpt],
    )?;
    connection.execute(
        "DELETE FROM symbols_fts WHERE rowid IN (SELECT id FROM symbols WHERE file_id=?1)",
        [file_id],
    )?;
    connection.execute("DELETE FROM edges WHERE file_id=?1", [file_id])?;
    connection.execute("DELETE FROM symbols WHERE file_id=?1", [file_id])?;
    for symbol in &item.symbols {
        connection.execute(
            "INSERT INTO symbols(file_id,name,qualname,kind,start,end,sig,snippet,snippet_truncated)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                file_id,
                symbol.name,
                symbol.qualname,
                symbol.kind,
                symbol.start,
                symbol.end,
                symbol.sig,
                truncate(&symbol.snippet, 2_000),
                symbol.snippet_truncated || symbol.snippet.chars().count() > 2_000
            ],
        )?;
    }
    connection.execute(
        "INSERT INTO symbols_fts(rowid,name,qualname,snippet)
        SELECT id,name,COALESCE(qualname,''),COALESCE(snippet,'') FROM symbols WHERE file_id=?1",
        [file_id],
    )?;
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
         UPDATE edges SET dst_symbol_id=COALESCE(
           (SELECT s.id FROM symbols s
            WHERE s.name=edges.dst_name AND s.file_id=edges.file_id
            ORDER BY s.start LIMIT 1),
           (SELECT s.id FROM symbols s JOIN files f ON f.id=s.file_id
            WHERE s.name=edges.dst_name
              AND f.lang=(SELECT src.lang FROM files src WHERE src.id=edges.file_id)
            ORDER BY f.is_test,f.is_vendor,f.path,s.start LIMIT 1),
           (SELECT s.id FROM symbols s JOIN files f ON f.id=s.file_id
            WHERE s.name=edges.dst_name
            ORDER BY f.is_test,f.is_vendor,f.path,s.start LIMIT 1)
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

pub(crate) fn modified_seconds(metadata: &fs::Metadata) -> f64 {
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

/// Read once for all derived indexes, rejecting growth and concurrent edits.
pub(crate) fn read_document(
    path: &Path,
    max_bytes: u64,
) -> Result<Option<(String, fs::Metadata, String)>> {
    let Some((bytes, metadata)) = read_document_bytes(path, max_bytes)? else {
        return Ok(None);
    };
    let digest = format!("{:x}", Sha1::digest(&bytes));
    Ok(Some((
        String::from_utf8_lossy(&bytes).into_owned(),
        metadata,
        digest,
    )))
}

/// The exact same validated read as indexing, without computing an unused hash.
pub(crate) fn read_search_document(
    path: &Path,
    max_bytes: u64,
) -> Result<Option<(String, fs::Metadata)>> {
    Ok(read_document_bytes(path, max_bytes)?
        .map(|(bytes, metadata)| (String::from_utf8_lossy(&bytes).into_owned(), metadata)))
}

fn read_document_bytes(path: &Path, max_bytes: u64) -> Result<Option<(Vec<u8>, fs::Metadata)>> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("symlink changed during traversal: {}", path.display());
    }
    let mut file =
        fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let before = file.metadata()?;
    if before.len() > max_bytes {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(8192.min(max_bytes.saturating_add(1)))
        .read_to_end(&mut bytes)?;
    if ctx_tgrep::trigram::is_binary(&bytes) {
        return Ok(None);
    }
    let remaining = max_bytes
        .saturating_add(1)
        .saturating_sub(bytes.len() as u64);
    (&mut file).take(remaining).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if current.file_type().is_symlink() {
        bail!("symlink changed during read: {}", path.display());
    }
    if bytes.len() as u64 > max_bytes
        || bytes.len() as u64 != after.len()
        || ctx_tgrep::builder::file_version(&before) != ctx_tgrep::builder::file_version(&after)
        || ctx_tgrep::builder::file_version(&current) != ctx_tgrep::builder::file_version(&after)
    {
        bail!("file changed while reading: {}", path.display());
    }
    if ctx_tgrep::trigram::is_binary(&bytes) {
        return Ok(None);
    }
    Ok(Some((bytes, after)))
}

#[cfg(test)]
mod document_read_tests {
    use super::*;

    #[test]
    fn search_and_index_readers_share_decoding_admission_and_hash_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.py");
        for bytes in [
            b"normal\r\ntext".as_slice(),
            b"invalid \xff UTF8",
            b"",
            b"binary\0content",
        ] {
            fs::write(&path, bytes).unwrap();
            for cap in [0, 4, 100] {
                let indexed = read_document(&path, cap).unwrap();
                let searched = read_search_document(&path, cap).unwrap();
                assert_eq!(indexed.is_some(), searched.is_some());
                if let (Some((a, meta_a, hash)), Some((b, meta_b))) = (indexed, searched) {
                    assert_eq!(a, b);
                    assert_eq!(meta_a.len(), meta_b.len());
                    assert_eq!(hash, format!("{:x}", Sha1::digest(bytes)));
                }
            }
        }
    }
}
