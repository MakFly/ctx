use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;
use fs2::FileExt;
use serde::Deserialize;
use serde_json::json;

use crate::config::{ctx_dir, repo_root};
use crate::indexer::index_repository_with_options;

const REQUEST_FILE: &str = "reindex.request";
const WORKER_LOCK: &str = "reindex.worker.lock";

#[derive(Debug, Deserialize)]
struct ReindexRequest {
    force: bool,
}

pub fn request_async(root: &Path, force: bool) -> Result<()> {
    let root = repo_root(root)?;
    let directory = ctx_dir(&root);
    fs::create_dir_all(&directory)?;
    let mut request = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(REQUEST_FILE))?;
    writeln!(request, "{}", json!({"force": force}))?;
    spawn_worker(&root);
    Ok(())
}

pub fn drain(root: &Path) -> Result<()> {
    let root = repo_root(root)?;
    let directory = ctx_dir(&root);
    fs::create_dir_all(&directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(WORKER_LOCK))?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    loop {
        let request_path = directory.join(REQUEST_FILE);
        if !request_path.is_file() {
            break;
        }
        let snapshot = directory.join(format!(
            "reindex.request.{}.{}",
            std::process::id(),
            std::process::id()
        ));
        if fs::rename(&request_path, &snapshot).is_err() {
            break;
        }
        let force = fs::read_to_string(&snapshot)?
            .lines()
            .filter_map(|line| serde_json::from_str::<ReindexRequest>(line).ok())
            .any(|request| request.force);
        fs::remove_file(snapshot)?;
        if crate::watcher::status(&root)["watcher"] == "active" {
            continue;
        }
        if index_repository_with_options(&root, force).is_err() {
            let mut retry = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&request_path)?;
            writeln!(retry, "{}", json!({"force": force}))?;
            break;
        }
    }
    let _ = lock.unlock();
    Ok(())
}

fn spawn_worker(root: &Path) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let _ = Command::new(executable)
        .args(["reindex-worker", "--root"])
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir(root)
        .spawn();
}
