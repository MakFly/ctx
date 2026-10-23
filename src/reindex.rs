use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;
use fs2::FileExt;
use serde::Deserialize;
use serde_json::json;

use crate::config::{ctx_dir, repo_root};
use crate::indexer::index_repository_with_options;
use crate::spool;

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
    spool::append_line(
        &directory,
        REQUEST_FILE,
        json!({"force": force}).to_string().as_bytes(),
    )?;
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
        let snapshots = spool::claim(&directory, REQUEST_FILE)?;
        if snapshots.is_empty() {
            break;
        }
        let force = snapshots
            .iter()
            .map(fs::read_to_string)
            .collect::<std::io::Result<Vec<_>>>()?
            .iter()
            .flat_map(|contents| contents.lines())
            .filter_map(|line| serde_json::from_str::<ReindexRequest>(line).ok())
            .any(|request| request.force);
        if crate::watcher::status(&root)["watcher"] == "active" {
            for snapshot in snapshots {
                fs::remove_file(snapshot)?;
            }
            continue;
        }
        if index_repository_with_options(&root, force).is_err() {
            break;
        }
        for snapshot in snapshots {
            fs::remove_file(snapshot)?;
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
