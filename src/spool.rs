use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use fs2::FileExt;

pub(crate) fn append_line(directory: &Path, queue_name: &str, line: &[u8]) -> Result<()> {
    append_line_inner(directory, queue_name, line, || {})
}

fn append_line_inner<F>(
    directory: &Path,
    queue_name: &str,
    line: &[u8],
    after_lock: F,
) -> Result<()>
where
    F: FnOnce(),
{
    validate_queue_name(queue_name)?;
    let lock = open_lock(directory, queue_name)?;
    lock.lock_exclusive()?;

    let result = (|| -> Result<()> {
        after_lock();
        let mut queue = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join(queue_name))?;
        queue.write_all(line)?;
        queue.write_all(b"\n")?;
        queue.flush()?;
        Ok(())
    })();
    let unlock_result = FileExt::unlock(&lock);
    result?;
    unlock_result?;
    Ok(())
}

pub(crate) fn claim(directory: &Path, queue_name: &str) -> Result<Vec<PathBuf>> {
    claim_inner(directory, queue_name, || {})
}

fn claim_inner<F>(directory: &Path, queue_name: &str, after_lock: F) -> Result<Vec<PathBuf>>
where
    F: FnOnce(),
{
    validate_queue_name(queue_name)?;
    let lock = open_lock(directory, queue_name)?;
    lock.lock_exclusive()?;

    let result = (|| -> Result<Vec<PathBuf>> {
        after_lock();
        let queue = directory.join(queue_name);
        if queue.is_file() {
            let snapshot = unique_snapshot_path(directory, queue_name)?;
            fs::rename(queue, snapshot)?;
        }
        snapshots(directory, queue_name)
    })();
    let unlock_result = FileExt::unlock(&lock);
    let snapshots = result?;
    unlock_result?;
    Ok(snapshots)
}

fn open_lock(directory: &Path, queue_name: &str) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(format!("{queue_name}.lock")))?)
}

fn validate_queue_name(queue_name: &str) -> Result<()> {
    let mut components = Path::new(queue_name).components();
    ensure!(
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none(),
        "queue name must be a single path component"
    );
    Ok(())
}

fn unique_snapshot_path(directory: &Path, queue_name: &str) -> Result<PathBuf> {
    let mut nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_nanos();
    loop {
        let candidate = directory.join(format!("{queue_name}.{}.{}", std::process::id(), nanos));
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                nanos = nanos
                    .checked_add(1)
                    .context("snapshot name space exhausted")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(error.into()),
        }
    }
}

fn snapshots(directory: &Path, queue_name: &str) -> Result<Vec<PathBuf>> {
    let prefix = format!("{queue_name}.");
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some((pid, timestamp)) = suffix.split_once('.') else {
            continue;
        };
        if !is_decimal(pid) || !is_decimal(timestamp) || !entry.file_type()?.is_file() {
            continue;
        }
        snapshots.push((entry.metadata()?.modified()?, entry.path()));
    }
    snapshots.sort_by(|(left_time, left_path), (right_time, right_path)| {
        left_time
            .cmp(right_time)
            .then_with(|| left_path.cmp(right_path))
    });
    Ok(snapshots.into_iter().map(|(_, path)| path).collect())
}

fn is_decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::sync::mpsc;
    use std::thread;

    use fs2::FileExt;

    use super::*;

    #[test]
    fn append_during_claim_is_available_to_the_next_claim() {
        let directory = tempfile::tempdir().unwrap();
        let directory_path = directory.path().to_path_buf();
        let queue_name = "spool-test.queue";
        append_line(directory.path(), queue_name, b"before").unwrap();

        let (claim_locked_tx, claim_locked_rx) = mpsc::channel();
        let (continue_claim_tx, continue_claim_rx) = mpsc::channel();
        let claim_directory = directory_path.clone();
        let claim_thread = thread::spawn(move || {
            claim_inner(&claim_directory, queue_name, || {
                claim_locked_tx.send(()).unwrap();
                continue_claim_rx.recv().unwrap();
            })
            .unwrap()
        });
        claim_locked_rx.recv().unwrap();

        let (append_attempted_tx, append_attempted_rx) = mpsc::channel();
        let append_directory = directory_path.clone();
        let append_thread = thread::spawn(move || {
            let probe = OpenOptions::new()
                .read(true)
                .write(true)
                .open(append_directory.join(format!("{queue_name}.lock")))
                .unwrap();
            assert!(probe.try_lock_exclusive().is_err());
            append_attempted_tx.send(()).unwrap();
            append_line(&append_directory, queue_name, b"during").unwrap();
        });
        append_attempted_rx.recv().unwrap();
        continue_claim_tx.send(()).unwrap();

        let first_claim = claim_thread.join().unwrap();
        append_thread.join().unwrap();
        assert_eq!(first_claim.len(), 1);
        assert_eq!(fs::read(&first_claim[0]).unwrap(), b"before\n");
        fs::remove_file(&first_claim[0]).unwrap();

        let second_claim = claim(directory.path(), queue_name).unwrap();
        assert_eq!(second_claim.len(), 1);
        assert_eq!(fs::read(&second_claim[0]).unwrap(), b"during\n");
    }

    #[test]
    fn orphan_snapshot_is_returned_and_processed() {
        let directory = tempfile::tempdir().unwrap();
        let orphan = directory.path().join("spool-test.queue.123.456");
        fs::write(&orphan, b"orphan\n").unwrap();

        let claimed = claim(directory.path(), "spool-test.queue").unwrap();
        assert_eq!(claimed, vec![orphan.clone()]);
        let contents = fs::read_to_string(&claimed[0]).unwrap();
        assert_eq!(contents.lines().collect::<Vec<_>>(), vec!["orphan"]);
        fs::remove_file(&claimed[0]).unwrap();
        assert!(!orphan.exists());
    }
}
