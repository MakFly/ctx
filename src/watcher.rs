//! Session-scoped filesystem maintenance. No detached processes or PID locks.
// Portions Copyright (c) Microsoft Corporation. MIT: vendor/tgrep-core/LICENSE.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};

use crate::config::ctx_dir;
use crate::text_index::WriterLock;

const QUEUE_CAP: usize = 16_384;
const QUIET: Duration = Duration::from_millis(200);
const MAX_BURST: Duration = Duration::from_secs(2);
const RECONCILE: Duration = Duration::from_secs(3600);

/// Adapted from tgrep's FsEventBurst: deduplicate paths, preserve a burst's
/// first timestamp, and never extend the maximum deadline on every event.
#[derive(Default)]
struct EventBatch {
    paths: HashSet<PathBuf>,
    first: Option<Instant>,
    last: Option<Instant>,
    full: bool,
}
impl EventBatch {
    fn push(&mut self, event: Event) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.full |= event.need_rescan();
        for path in event.paths {
            self.full |= matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".gitignore" | ".ignore" | ".ctxignore" | "exclude" | "HEAD")
            );
            if self.paths.len() >= QUEUE_CAP {
                self.full = true;
                self.paths.clear();
                break;
            }
            self.paths.insert(path);
        }
    }
    fn ready(&self) -> bool {
        self.full
            || self.first.is_some_and(|first| first.elapsed() >= MAX_BURST)
            || self.last.is_some_and(|last| last.elapsed() >= QUIET)
    }
}

pub struct WatchSession {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl WatchSession {
    pub fn start(root: &Path) -> Result<Self> {
        Self::start_with_force(root, false)
    }
    pub fn start_with_force(root: &Path, force: bool) -> Result<Self> {
        let root = root.canonicalize()?;
        let stop = Arc::new(AtomicBool::new(false));
        let child_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("ctx-index-watch".into())
            .spawn(move || {
                // An observer periodically retries the OS lock, including between
                // MCP requests, and takes over after the owner exits or crashes.
                while !child_stop.load(Ordering::Acquire) {
                    match WriterLock::try_acquire(&ctx_dir(&root)) {
                        Ok(Some(lock)) => {
                            if let Err(error) = run_owner(&root, &child_stop, &lock, force) {
                                eprintln!("ctx watcher: {error:#}");
                                write_state(&root, false, true, None, Some(&format!("{error:#}")));
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            eprintln!("ctx watcher unavailable: {error:#}");
                            break;
                        }
                    }
                    if !child_stop.load(Ordering::Acquire) {
                        thread::sleep(QUIET);
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for WatchSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn write_state(root: &Path, active: bool, pending: bool, last: Option<i64>, error: Option<&str>) {
    let value = json!({"watcher": if active { "active" } else { "inactive" },
        "pid": std::process::id(), "catching_up": pending,
        "last_reconciliation_ms": last, "error": error});
    // State is advisory; an unreadable/torn record is treated as pending by readers.
    let _ = std::fs::write(ctx_dir(root).join("watch-state.json"), value.to_string());
}

fn ignored_artifact(root: &Path, path: &Path) -> bool {
    if path.starts_with(ctx_dir(root)) {
        return true;
    }
    path.strip_prefix(root).is_ok_and(|relative| {
        relative.components().any(|part| {
            matches!(
                part.as_os_str().to_str(),
                Some(
                    ".ctx"
                        | "target"
                        | "node_modules"
                        | "vendor"
                        | "dist"
                        | "build"
                        | ".venv"
                        | "venv"
                        | "__pycache__"
                        | ".next"
                        | ".nuxt"
                )
            )
        })
    })
}

fn register_directories(
    watcher: &mut RecommendedWatcher,
    root: &Path,
    registered: &mut HashSet<PathBuf>,
) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut desired: HashSet<PathBuf> = crate::indexer::walk_metadata_report(root)?
            .directories
            .into_iter()
            .collect();
        // Only Git metadata which controls traversal/branch state; never object trees.
        for path in [root.join(".git"), root.join(".git/info")] {
            if path.is_dir() {
                desired.insert(path);
            }
        }
        for path in registered.difference(&desired) {
            let _ = watcher.unwatch(path);
        }
        for path in desired.difference(registered) {
            watcher.watch(path, RecursiveMode::NonRecursive)?;
        }
        *registered = desired;
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        if registered.is_empty() {
            watcher.watch(root, RecursiveMode::Recursive)?;
            registered.insert(root.to_path_buf());
        }
    }
    Ok(())
}

fn run_owner(root: &Path, stop: &AtomicBool, _lock: &WriterLock, force: bool) -> Result<()> {
    write_state(root, true, true, None, None);
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAP);
    let overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = overflow.clone();
    let callback_root = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if let Ok(event) = &event {
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            if !event.paths.is_empty()
                && event
                    .paths
                    .iter()
                    .all(|path| ignored_artifact(&callback_root, path))
            {
                return;
            }
        }
        if sender.try_send(event).is_err() {
            callback_overflow.store(true, Ordering::Release);
        }
    })?;
    let mut registered = HashSet::new();
    register_directories(&mut watcher, root, &mut registered)?;
    crate::indexer::index_repository_locked(root, force)?;
    let mut last_reconciliation = chrono::Utc::now().timestamp_millis();
    let mut reconciled_at = Instant::now();
    let mut batch = EventBatch::default();
    write_state(root, true, false, Some(last_reconciliation), None);
    while !stop.load(Ordering::Acquire) {
        let timeout = if batch.full {
            Duration::ZERO
        } else {
            let max_remaining = batch
                .first
                .map(|first| MAX_BURST.saturating_sub(first.elapsed()))
                .unwrap_or(QUIET);
            let quiet_remaining = batch
                .last
                .map(|last| QUIET.saturating_sub(last.elapsed()))
                .unwrap_or(QUIET);
            max_remaining.min(quiet_remaining)
        };
        match receiver.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                batch.push(event);
                write_state(root, true, true, Some(last_reconciliation), None);
            }
            Ok(Err(_)) => batch.full = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        batch.full |=
            overflow.swap(false, Ordering::AcqRel) || reconciled_at.elapsed() >= RECONCILE;
        if !batch.ready() {
            continue;
        }
        write_state(root, true, true, Some(last_reconciliation), None);
        // Re-register before and after updates so newly admitted directories
        // cannot remain invisible after an ignore-rule change or rename.
        register_directories(&mut watcher, root, &mut registered)?;
        match crate::indexer::index_repository_locked_with_paths(root, batch.full, &batch.paths) {
            Ok(_) => {
                if batch.full {
                    reconciled_at = Instant::now();
                    last_reconciliation = chrono::Utc::now().timestamp_millis();
                }
                batch = EventBatch::default();
                register_directories(&mut watcher, root, &mut registered)?;
                write_state(root, true, false, Some(last_reconciliation), None);
            }
            Err(error) => {
                write_state(
                    root,
                    true,
                    true,
                    Some(last_reconciliation),
                    Some(&format!("{error:#}")),
                );
                batch.full = true; // Retry transient edits; never mark failed work fresh.
                thread::sleep(QUIET);
            }
        }
    }
    drop(watcher);
    write_state(
        root,
        false,
        batch.ready() || batch.first.is_some(),
        Some(last_reconciliation),
        None,
    );
    Ok(())
}

pub fn status(root: &Path) -> Value {
    let directory = ctx_dir(root);
    let mut state = std::fs::read(directory.join("watch-state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or_else(
            || json!({"watcher":"inactive","catching_up":false,"last_reconciliation_ms":null}),
        );
    if directory.is_dir() {
        match WriterLock::try_acquire(&directory) {
            Ok(Some(_)) => state["watcher"] = json!("inactive"),
            Ok(None) => {
                if state["watcher"] != "active" {
                    state["watcher"] = json!("writer_active");
                    state["catching_up"] = json!(true);
                }
            }
            Err(_) => state["catching_up"] = json!(true),
        }
    }
    state
}

pub fn generation(start: &Path) -> Option<String> {
    let directory = crate::config::find_ctx(start).ok()?;
    let connection = rusqlite::Connection::open_with_flags(
        directory.join("index.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    crate::db::get_meta(&connection, "text_generation", "").ok()
}

pub fn check_coverage(
    start: &Path,
    before: Option<String>,
    mut envelope: crate::model::Envelope,
) -> crate::model::Envelope {
    let root =
        crate::indexer::index_root_from_database(start).unwrap_or_else(|_| start.to_path_buf());
    if before.as_ref().is_none_or(|value| value.is_empty())
        || before != generation(start)
        || status(&root)["catching_up"].as_bool().unwrap_or(true)
        || evidence_stale(&root, &envelope)
    {
        envelope.coverage = "partial".into();
        let hint = envelope.hint.get_or_insert_with(String::new);
        if !hint.is_empty() {
            hint.push_str("; ");
        }
        hint.push_str("index update pending or generation changed during retrieval; retry for consistent context");
    }
    envelope
}

fn evidence_stale(root: &Path, envelope: &crate::model::Envelope) -> bool {
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        ctx_dir(root).join("index.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return true;
    };
    let mut seen = HashSet::new();
    for hit in &envelope.hits {
        if !seen.insert(&hit.path) {
            continue;
        }
        let Ok(metadata) = root.join(&hit.path).metadata() else {
            return true;
        };
        let indexed: Option<String> = connection.query_row(
            "SELECT c.version FROM file_contents c JOIN files f ON f.id=c.file_id WHERE f.path=?1",
            [&hit.path], |row| row.get(0),
        ).ok();
        if indexed.as_deref()
            != Some(format!("{:?}", ctx_tgrep::builder::file_version(&metadata)).as_str())
        {
            return true;
        }
    }
    false
}

pub(crate) fn clear_state_after_manual(root: &Path) {
    write_state(
        root,
        false,
        false,
        Some(chrono::Utc::now().timestamp_millis()),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_events_coalesce_but_do_not_extend_the_maximum_deadline() {
        let mut batch = EventBatch::default();
        for _ in 0..20 {
            batch.push(Event::new(EventKind::Any).add_path(PathBuf::from("same.txt")));
        }
        assert_eq!(batch.paths.len(), 1);
        batch.first = Some(Instant::now() - MAX_BURST);
        batch.last = Some(Instant::now());
        assert!(batch.ready());
    }

    #[test]
    fn lost_events_and_ignore_updates_request_full_reconciliation() {
        let mut batch = EventBatch::default();
        batch.push(Event::new(EventKind::Any).set_flag(notify::event::Flag::Rescan));
        assert!(batch.full && batch.ready());
        let mut batch = EventBatch::default();
        batch.push(Event::new(EventKind::Any).add_path(PathBuf::from("nested/.ctxignore")));
        assert!(batch.full && batch.ready());
    }
}

/// Retry ownership at the request boundary as well as in the observer thread.
/// Existing owners are never interrupted. A recovered writer reconciles before
/// this request can use a stale graph left by a crashed session.
pub fn before_request(root: &Path) -> Result<()> {
    let lock = match WriterLock::try_acquire(&ctx_dir(root)) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("ctx watcher unavailable: {error:#}");
            return Ok(());
        }
    };
    if let Some(_lock) = lock {
        write_state(root, true, true, None, None);
        let result = crate::indexer::index_repository_locked(root, false);
        match result {
            Ok(_) => clear_state_after_manual(root),
            Err(error) => {
                write_state(root, false, true, None, Some(&format!("{error:#}")));
                // Keep the last generation readable; exact queries can scan.
                eprintln!("ctx index reconciliation deferred: {error:#}");
            }
        }
    }
    Ok(())
}
