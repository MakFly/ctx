use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use ctx_code::db::{connect, get_meta};
use ctx_code::indexer::index_repository;
use ctx_code::text_index::WriterLock;
use ctx_code::watcher::WatchSession;

fn eventually(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !predicate() {
        assert!(Instant::now() < deadline, "watcher did not converge");
        thread::sleep(Duration::from_millis(50));
    }
}

fn indexed_content(root: &Path, path: &str) -> Option<String> {
    let connection = connect(&root.join(".ctx/index.sqlite"), false).ok()?;
    connection
        .query_row(
            "SELECT c.content FROM file_contents c JOIN files f ON f.id=c.file_id WHERE f.path=?1",
            [path],
            |row| row.get(0),
        )
        .ok()
}

#[test]
fn watcher_updates_creates_renames_deletes_and_reloads_exclusions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.txt"), "first").unwrap();
    let session = WatchSession::start(root).unwrap();
    eventually(|| indexed_content(root, "a.txt").as_deref() == Some("first"));
    fs::write(root.join("a.txt"), "updated").unwrap();
    eventually(|| indexed_content(root, "a.txt").as_deref() == Some("updated"));
    fs::create_dir(root.join("newdir")).unwrap();
    fs::write(root.join("newdir/b.txt"), "newfile").unwrap();
    eventually(|| indexed_content(root, "newdir/b.txt").is_some());
    fs::rename(root.join("newdir/b.txt"), root.join("newdir/c.txt")).unwrap();
    eventually(|| {
        indexed_content(root, "newdir/b.txt").is_none()
            && indexed_content(root, "newdir/c.txt").is_some()
    });
    fs::write(root.join(".ctxignore"), "newdir/\n").unwrap();
    eventually(|| indexed_content(root, "newdir/c.txt").is_none());
    fs::write(root.join(".ctxignore"), "").unwrap();
    eventually(|| indexed_content(root, "newdir/c.txt").is_some());
    fs::remove_file(root.join("a.txt")).unwrap();
    eventually(|| indexed_content(root, "a.txt").is_none());
    drop(session);
    assert!(
        WriterLock::try_acquire(&root.join(".ctx"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn observer_takes_over_and_all_session_threads_release_the_writer() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.txt"), "initial").unwrap();
    index_repository(root).unwrap();
    let owner = WriterLock::try_acquire(&root.join(".ctx"))
        .unwrap()
        .unwrap();
    assert!(
        WriterLock::try_acquire(&root.join(".ctx"))
            .unwrap()
            .is_none()
    );
    let observer = WatchSession::start(root).unwrap();
    fs::write(root.join("a.txt"), "after takeover").unwrap();
    drop(owner);
    eventually(|| indexed_content(root, "a.txt").as_deref() == Some("after takeover"));
    drop(observer);
    assert!(
        WriterLock::try_acquire(&root.join(".ctx"))
            .unwrap()
            .is_some()
    );
    let db = connect(&root.join(".ctx/index.sqlite"), false).unwrap();
    assert!(!get_meta(&db, "text_generation", "").unwrap().is_empty());
}

#[test]
fn killed_watch_process_releases_its_lock_for_a_new_session() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.txt"), "initial").unwrap();
    let mut child = ChildGuard(
        std::process::Command::new(assert_cmd::cargo::cargo_bin!("ctx"))
            .args(["index", ".", "--watch"])
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    eventually(|| indexed_content(root, "a.txt").is_some());
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "--watch exited after one indexing pass"
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    fs::write(root.join("a.txt"), "after crash").unwrap();
    let recovered = WatchSession::start(root).unwrap();
    eventually(|| indexed_content(root, "a.txt").as_deref() == Some("after crash"));
    drop(recovered);
    assert!(
        WriterLock::try_acquire(&root.join(".ctx"))
            .unwrap()
            .is_some()
    );
}
