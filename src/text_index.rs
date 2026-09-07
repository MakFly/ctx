//! Full-content retrieval backed by the vendored tgrep index.
// Portions Copyright (c) Microsoft Corporation. MIT: vendor/tgrep-core/LICENSE.
//!
//! SQLite is the publication manifest: a transaction references an immutable
//! generation only after all its files have been written and synchronized.
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use ctx_tgrep::{builder::DocumentBuilder, query, reader::IndexReader};
use fs2::FileExt;
use regex::{Regex, RegexBuilder};
use rusqlite::{Connection, params};

use crate::config::{ctx_dir, find_ctx, index_settings, relative_path};
use crate::db::{connect, get_meta, set_meta};
use crate::model::{Envelope, Hit};
use crate::search::EvidenceBudget;

pub const FORMAT: &str = "4";

/// OS-held lock, never inferred from a PID file. Drop also releases after panic.
pub struct WriterLock(File);
impl WriterLock {
    pub fn try_acquire(directory: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(directory)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("writer.lock"))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self(file))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}
impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub struct GenerationReader {
    reader: std::sync::Arc<IndexReader>,
    hybrid: ctx_tgrep::hybrid::HybridIndex,
    _lease: File,
}
impl std::ops::Deref for GenerationReader {
    type Target = IndexReader;
    fn deref(&self) -> &Self::Target {
        &self.reader
    }
}

pub fn open_generation(connection: &Connection, directory: &Path) -> Result<GenerationReader> {
    let generation = get_meta(connection, "text_generation", "")?;
    if generation.is_empty() || !generation.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        bail!("text index generation absent or invalid");
    }
    let path = directory.join("text").join(generation);
    let lease = File::open(path.join("lease"))?;
    FileExt::lock_shared(&lease)?;
    let root = PathBuf::from(get_meta(connection, "repo_root", "")?);
    let hybrid = ctx_tgrep::hybrid::HybridIndex::open(&path, &root)?;
    let reader = hybrid.reader_arc();
    let meta = ctx_tgrep::meta::IndexMeta::load(&path)?;
    if !meta.complete
        || meta.num_files != reader.num_files() as u64
        || meta.num_trigrams != reader.num_trigrams() as u64
    {
        bail!("text generation metadata does not match its files");
    }
    Ok(GenerationReader {
        reader,
        hybrid,
        _lease: lease,
    })
}

type CachedGeneration = Mutex<Option<(String, Arc<GenerationReader>)>>;
type MatcherKey = (String, bool, bool);
#[derive(Default)]
struct SessionCache {
    generation: CachedGeneration,
    matchers: Mutex<VecDeque<(MatcherKey, Arc<Regex>)>>,
}
const MATCHER_CACHE_ENTRIES: usize = 16;
const MATCHER_CACHE_PATTERN_BYTES: usize = 4096;
const MATCHER_CACHE_COMPILED_BYTES: usize = 256 * 1024;
const MATCHER_CACHE_DFA_BYTES: usize = 64 * 1024;
static READER_SESSIONS: OnceLock<Mutex<HashMap<PathBuf, Weak<SessionCache>>>> = OnceLock::new();

/// A session owns at most one cached generation. In-flight searches keep their
/// own Arc/lease when publication replaces it; the last session releases cache.
pub struct ReaderSession {
    _cache: Arc<SessionCache>,
}
impl ReaderSession {
    pub fn start(root: &Path) -> Result<Self> {
        let directory = find_ctx(root)?;
        let directory = directory.canonicalize().unwrap_or(directory);
        let mut sessions = READER_SESSIONS
            .get_or_init(Default::default)
            .lock()
            .unwrap();
        sessions.retain(|_, cache| cache.strong_count() > 0);
        let cache = sessions
            .get(&directory)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let cache = Arc::new(SessionCache::default());
                sessions.insert(directory, Arc::downgrade(&cache));
                cache
            });
        Ok(Self { _cache: cache })
    }
}

fn search_generation(connection: &Connection, directory: &Path) -> Result<Arc<GenerationReader>> {
    let directory = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    let cache = READER_SESSIONS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(&directory)
        .and_then(Weak::upgrade);
    let Some(cache) = cache else {
        return open_generation(connection, &directory).map(Arc::new);
    };
    // Read the key from this request's SQLite snapshot, never from a second
    // connection that might see a different graph/text publication.
    let generation = get_meta(connection, "text_generation", "")?;
    let mut cached = cache.generation.lock().unwrap();
    if let Some((key, reader)) = &*cached {
        if key == &generation {
            return Ok(Arc::clone(reader));
        }
    }
    // Drop the cache's old lease even if the new generation is unusable.
    *cached = None;
    let reader = Arc::new(open_generation(connection, &directory)?);
    *cached = Some((generation, Arc::clone(&reader)));
    Ok(reader)
}

fn session_matcher(
    directory: &Path,
    query: &str,
    literal: bool,
    ignore_case: bool,
) -> Result<Arc<Regex>> {
    let directory = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    let cache = READER_SESSIONS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(&directory)
        .and_then(Weak::upgrade);
    let Some(cache) = cache.filter(|_| query.len() <= MATCHER_CACHE_PATTERN_BYTES) else {
        return matcher(query, literal, ignore_case).map(Arc::new);
    };
    let key = (query.to_owned(), literal, ignore_case);
    let mut entries = cache.matchers.lock().unwrap();
    if let Some(index) = entries.iter().position(|(existing, _)| existing == &key) {
        let entry = entries.remove(index).unwrap();
        let regex = Arc::clone(&entry.1);
        entries.push_back(entry);
        return Ok(regex);
    }
    let pattern = if literal {
        regex::escape(query)
    } else {
        query.to_owned()
    };
    // Cap cached compiler/DFA resources. A pattern exceeding these smaller
    // limits still uses the original uncached compiler, preserving acceptance.
    let bounded = RegexBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .size_limit(MATCHER_CACHE_COMPILED_BYTES)
        .dfa_size_limit(MATCHER_CACHE_DFA_BYTES)
        .build();
    let Ok(regex) = bounded else {
        drop(entries);
        return matcher(query, literal, ignore_case).map(Arc::new);
    };
    let regex = Arc::new(regex);
    if entries.len() == MATCHER_CACHE_ENTRIES {
        entries.pop_front();
    }
    entries.push_back((key, Arc::clone(&regex)));
    Ok(regex)
}

pub struct StagedGeneration {
    directory: PathBuf,
    committed: bool,
}
impl StagedGeneration {
    pub fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for StagedGeneration {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

/// Build only changed postings; merge the old mmap without decoding it in full.
/// Documents come from the transaction, not another potentially different read.
pub fn stage_generation(
    connection: &Connection,
    root: &Path,
    changed: &HashSet<String>,
    removed: &HashSet<String>,
) -> Result<Option<StagedGeneration>> {
    let directory = ctx_dir(root);
    let old = open_generation(connection, &directory).ok();
    if old.is_some() && changed.is_empty() && removed.is_empty() {
        return Ok(None);
    }
    let generation = format!(
        "{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    );
    let staged = StagedGeneration {
        directory: directory.join("text").join(&generation),
        committed: false,
    };
    fs::create_dir_all(&staged.directory)?;
    File::create(staged.directory.join("lease"))?;
    let delta_dir = staged.directory.join("delta");
    let mut builder = DocumentBuilder::new(root, &delta_dir, index_settings(root)?.buffer_bytes)?;
    if old.is_none() {
        let mut statement = connection.prepare(
            "SELECT f.path,c.content FROM files f JOIN file_contents c ON c.file_id=f.id ORDER BY f.path"
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            builder.push(row.get(0)?, &row.get::<_, String>(1)?)?;
        }
    } else {
        let mut paths = changed.iter().collect::<Vec<_>>();
        paths.sort();
        let mut statement = connection.prepare(
            "SELECT c.content FROM files f JOIN file_contents c ON c.file_id=f.id WHERE f.path=?1",
        )?;
        for path in paths {
            let content: String = statement.query_row([path], |row| row.get(0))?;
            builder.push(path.clone(), &content)?;
        }
    }
    builder.finish()?;
    if let Some(reader) = old {
        let delta = IndexReader::open(&delta_dir)?;
        let superseded = changed.union(removed).cloned().collect();
        ctx_tgrep::builder::merge_index_with_delta(
            root,
            &staged.directory,
            &reader,
            &delta,
            &superseded,
            true,
        )?;
        drop(delta);
        fs::remove_dir_all(&delta_dir)?;
    } else {
        for entry in fs::read_dir(&delta_dir)? {
            let entry = entry?;
            fs::rename(entry.path(), staged.directory.join(entry.file_name()))?;
        }
        fs::remove_dir(&delta_dir)?;
    }
    for entry in fs::read_dir(&staged.directory)? {
        let path = entry?.path();
        if path.is_file() {
            OpenOptions::new().write(true).open(path)?.sync_all()?;
        }
    }
    #[cfg(unix)]
    {
        File::open(&staged.directory)?.sync_all()?;
        File::open(directory.join("text"))?.sync_all()?;
    }
    let reader = IndexReader::open(&staged.directory)?;
    reader.validate_lookup().map_err(anyhow::Error::msg)?;
    let previous = get_meta(connection, "text_generation", "")?;
    set_meta(connection, "previous_text_generation", &previous)?;
    set_meta(connection, "text_generation", &generation)?;
    Ok(Some(staged))
}

pub fn matcher(query: &str, literal: bool, ignore_case: bool) -> Result<Regex> {
    let pattern = if literal {
        regex::escape(query)
    } else {
        query.to_owned()
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .build()
        .context("invalid or unsupported Rust regex")
}

/// Determine line spans lazily, after finding at least one match. Adapted from
/// tgrep-cli matching::LineIndex (see vendor/tgrep-core/PROVENANCE.md).
pub fn matching_spans(content: &str, matcher: &Regex) -> Vec<(usize, usize)> {
    let mut matches = matcher.find_iter(content).peekable();
    if matches.peek().is_none() {
        return Vec::new();
    }
    let mut starts = vec![0];
    starts.extend(content.match_indices('\n').map(|(offset, _)| offset + 1));
    if starts.len() > 1 && starts.last() == Some(&content.len()) {
        starts.pop();
    }
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for matched in matches {
        let first = starts
            .partition_point(|offset| *offset <= matched.start())
            .saturating_sub(1);
        let last_offset = matched
            .end()
            .saturating_sub(usize::from(!matched.is_empty()));
        let last = starts
            .partition_point(|offset| *offset <= last_offset)
            .saturating_sub(1);
        let begin = first.saturating_sub(2);
        let end = (last + 3).min(starts.len());
        if let Some(previous) = spans.last_mut()
            && begin <= previous.1
        {
            previous.1 = previous.1.max(end);
        } else {
            spans.push((begin, end));
        }
    }
    spans
}

pub fn content_hits(path: &str, content: &str, matcher: &Regex, kind: &str) -> Vec<Hit> {
    let spans = matching_spans(content, matcher);
    if spans.is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    spans
        .into_iter()
        .map(|(start, end)| span_hit(path, &lines, start, end, kind))
        .collect()
}

fn span_hit(path: &str, lines: &[&str], start: usize, end: usize, kind: &str) -> Hit {
    Hit {
        path: path.to_owned(),
        start: start + 1,
        end: end.max(start + 1),
        symbol: None,
        kind: kind.to_owned(),
        sig: String::new(),
        snippet: lines.get(start..end).unwrap_or_default().join("\n"),
        snippet_truncated: false,
        score: 0.55,
        why: "full-content match".to_owned(),
    }
}

/// Fresh paths and modified files always bypass the index. Even without a
/// watcher, a stale or missing generation cannot hide newly introduced matches.
pub fn exact_search(
    query_text: &str,
    mode: &str,
    ignore_case: bool,
    path_filter: Option<&str>,
    limit: usize,
    budget: usize,
    start: &Path,
) -> Result<Envelope> {
    search_full_content(
        query_text,
        mode,
        ignore_case,
        path_filter,
        limit,
        budget,
        start,
        true,
    )
}

/// Direct-scan oracle for paired benchmarks and pruning parity tests.
pub fn scan_search(
    query_text: &str,
    mode: &str,
    ignore_case: bool,
    path_filter: Option<&str>,
    limit: usize,
    budget: usize,
    start: &Path,
) -> Result<Envelope> {
    search_full_content(
        query_text,
        mode,
        ignore_case,
        path_filter,
        limit,
        budget,
        start,
        false,
    )
}

fn search_full_content(
    query_text: &str,
    mode: &str,
    ignore_case: bool,
    path_filter: Option<&str>,
    limit: usize,
    budget: usize,
    start: &Path,
    use_index: bool,
) -> Result<Envelope> {
    let started = Instant::now();
    let directory = find_ctx(start)?;
    let matcher = session_matcher(&directory, query_text, mode == "literal", ignore_case)?;
    let connection = if directory.join("index.sqlite").is_file() {
        connect(&directory.join("index.sqlite"), false).ok()
    } else {
        None
    };
    if let Some(connection) = &connection {
        connection.execute_batch("BEGIN DEFERRED")?;
    }
    let root = match &connection {
        Some(connection) => {
            PathBuf::from(get_meta(connection, "repo_root", &start.to_string_lossy())?)
        }
        None => {
            if std::env::var_os("CTX_DIR").is_none() {
                directory.parent().unwrap_or(start).canonicalize()?
            } else {
                start.canonicalize()?
            }
        }
    };
    // Conservatively bypass folding/inline flags until Unicode pruning is proven.
    let plan = if ignore_case || (mode == "regex" && query_text.contains("(?")) {
        query::QueryPlan::MatchAll
    } else if mode == "literal" {
        query::build_literal_plan(query_text, false)
    } else {
        query::build_query_plan(query_text, false).map_err(anyhow::Error::msg)?
    };
    let reader = if use_index && !plan.is_match_all() {
        connection
            .as_ref()
            .and_then(|db| search_generation(db, &directory).ok())
    } else {
        None
    };
    let candidates: Option<HashSet<String>> = reader.as_ref().map(|reader| {
        let (ids, snapshot) = reader.hybrid.execute_query_with_masks(&plan);
        ids.into_iter()
            .filter_map(|id| reader.hybrid.resolve_path(id, &snapshot))
            .collect()
    });
    let max_file_bytes = index_settings(&root)?.max_file_bytes;
    let walk = crate::indexer::walk_metadata_report(&root)?;
    let mut partial = walk.errors > 0;
    let mut evidence = EvidenceBudget::new(budget);
    let mut seen = 0;
    let mut omitted = false;
    for path in walk.paths {
        let relative = relative_path(&path, &root)?;
        if path_filter.is_some_and(|filter| !relative.contains(filter)) {
            continue;
        }
        let metadata = match path.metadata() {
            Ok(value) => value,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        // A candidate must be read regardless of its version. Only check
        // freshness when pruning would otherwise skip this file.
        let excluded = candidates
            .as_ref()
            .is_some_and(|paths| !paths.contains(&relative));
        let unchanged = if excluded && let Some(connection) = &connection {
            connection.prepare_cached(
                "SELECT f.size=?2 AND c.version=?3 FROM files f JOIN file_contents c ON c.file_id=f.id WHERE f.path=?1",
            ).and_then(|mut statement| statement.query_row(
                params![relative, metadata.len(), format!("{:?}", ctx_tgrep::builder::file_version(&metadata))],
                |row| row.get::<_, bool>(0),
            )).unwrap_or(false)
        } else {
            false
        };
        if unchanged {
            continue;
        }
        let content = match crate::indexer::read_search_document(&path, max_file_bytes) {
            Ok(Some((content, _))) => content,
            Ok(None) => continue,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        let mut lines = None;
        for (start, end) in matching_spans(&content, &matcher) {
            if seen >= limit {
                omitted = true;
                break;
            }
            seen += 1;
            if evidence.closed() {
                continue;
            }
            let lines = lines.get_or_insert_with(|| content.lines().collect::<Vec<_>>());
            let mut hit = span_hit(&relative, lines, start, end, "text");
            if metadata.len() > crate::config::MAX_FILE_SIZE
                || crate::config::language(&path).is_none()
            {
                hit.why = "full-content match; no structural analysis".into();
            }
            evidence.push(hit);
        }
        if omitted {
            break;
        }
    }
    let mut envelope = evidence.finish(
        started,
        if partial || omitted {
            "partial"
        } else {
            "text_only"
        },
        None,
    );
    if partial || omitted {
        envelope.hint = Some(
            "full-content search: results omitted or files changed/unreadable during the scan"
                .into(),
        );
    }
    Ok(envelope)
}

/// Keep current and previous generations. Shared leases retain older readers;
/// an open racing cleanup simply falls back to scanning the live files.
pub(crate) fn cleanup_generations(connection: &Connection, root: &Path) {
    let current = get_meta(connection, "text_generation", "").unwrap_or_default();
    let previous = get_meta(connection, "previous_text_generation", "").unwrap_or_default();
    let Ok(entries) = fs::read_dir(ctx_dir(root).join("text")) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == current
            || name == previous
            || name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_digit() || b == b'-')
        {
            continue;
        }
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(lease) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join("lease"))
        else {
            continue;
        };
        if lease.try_lock_exclusive().is_ok() {
            // No process can legitimately choose this retired generation anew.
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod performance_regressions {
    use super::*;
    use crate::indexer::index_repository;

    #[test]
    fn matcher_cache_keys_flags_bounds_entries_and_drops_with_session() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let directory = root.join(".ctx");
        fs::create_dir(&directory).unwrap();
        let session = ReaderSession::start(root).unwrap();
        let original = session_matcher(&directory, "a.b", true, false).unwrap();
        assert!(Arc::ptr_eq(
            &original,
            &session_matcher(&directory, "a.b", true, false).unwrap()
        ));
        assert!(!original.is_match("axb"));
        assert!(
            session_matcher(&directory, "a.b", false, false)
                .unwrap()
                .is_match("axb")
        );
        assert!(
            session_matcher(&directory, "a.b", true, true)
                .unwrap()
                .is_match("A.B")
        );
        for i in 0..MATCHER_CACHE_ENTRIES + 1 {
            session_matcher(&directory, &format!("pattern_{i}"), true, false).unwrap();
        }
        assert_eq!(
            session._cache.matchers.lock().unwrap().len(),
            MATCHER_CACHE_ENTRIES
        );
        let oversized = "z".repeat(MATCHER_CACHE_PATTERN_BYTES + 1);
        assert!(
            session_matcher(&directory, &oversized, true, false)
                .unwrap()
                .is_match(&oversized)
        );
        assert_eq!(
            session._cache.matchers.lock().unwrap().len(),
            MATCHER_CACHE_ENTRIES
        );
        // This valid expression exceeds the cache compiler limit, but must
        // remain accepted by the existing uncached compiler.
        let complex = r"\w{20}";
        assert!(
            session_matcher(&directory, complex, false, false)
                .unwrap()
                .is_match(&"x".repeat(20))
        );
        assert!(session_matcher(&directory, "[", false, false).is_err());
        let retained = session_matcher(&directory, "last", true, false).unwrap();
        let weak = Arc::downgrade(&retained);
        drop(retained);
        drop(session);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn unprunable_searches_never_open_a_generation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("a.py"), "if HTTPException: pass\n").unwrap();
        index_repository(root).unwrap();
        let session = ReaderSession::start(root).unwrap();
        for (pattern, mode, case) in [
            ("if", "literal", false),
            ("HTTPException", "literal", true),
            ("[a-z]+", "regex", false),
        ] {
            let actual = exact_search(pattern, mode, case, None, 100, 10000, root).unwrap();
            let expected = scan_search(pattern, mode, case, None, 100, 10000, root).unwrap();
            assert_eq!(actual.hits, expected.hits);
            assert!(!actual.hits.is_empty());
            assert!(session._cache.generation.lock().unwrap().is_none());
        }
    }

    #[test]
    fn session_reuses_readers_tracks_publications_and_releases_leases() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let directory = root.join(".ctx");
        fs::write(root.join("a.py"), "originalneedle").unwrap();
        index_repository(root).unwrap();
        let session = ReaderSession::start(root).unwrap();
        let second_session = ReaderSession::start(root).unwrap();
        let db = connect(&directory.join("index.sqlite"), false).unwrap();
        let first = search_generation(&db, &directory).unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &search_generation(&db, &directory).unwrap()
        ));
        let first_path = directory
            .join("text")
            .join(get_meta(&db, "text_generation", "").unwrap());
        // Pin the old SQLite snapshot while a new generation is published.
        db.execute_batch("BEGIN DEFERRED").unwrap();
        get_meta(&db, "text_generation", "").unwrap();
        fs::write(root.join("a.py"), "replacementneedle").unwrap();
        index_repository(root).unwrap();
        let newer_db = connect(&directory.join("index.sqlite"), false).unwrap();
        let newer = search_generation(&newer_db, &directory).unwrap();
        assert!(!Arc::ptr_eq(&first, &newer));
        let old_snapshot = search_generation(&db, &directory).unwrap();
        assert_eq!(old_snapshot.all_paths(), first.all_paths());
        assert_eq!(
            old_snapshot.lookup_trigram(ctx_tgrep::trigram::hash(b'o', b'r', b'i')),
            first.lookup_trigram(ctx_tgrep::trigram::hash(b'o', b'r', b'i'))
        );
        db.execute_batch("ROLLBACK").unwrap();
        let current = search_generation(&newer_db, &directory).unwrap();
        let weak = Arc::downgrade(&current);
        drop(current);
        drop(newer);
        drop(session);
        assert!(weak.upgrade().is_some());
        drop(second_session);
        assert!(weak.upgrade().is_none());
        // The in-flight old reader independently protects its generation.
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .open(first_path.join("lease"))
            .unwrap();
        assert!(lease.try_lock_exclusive().is_err());
        drop(first);
        drop(old_snapshot);
        assert!(lease.try_lock_exclusive().is_ok());
    }
}
