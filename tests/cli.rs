use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use serde_json::Value;
use walkdir::WalkDir;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("mini_repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini_repo");
    for entry in WalkDir::new(source).into_iter().map(Result::unwrap) {
        let relative = entry
            .path()
            .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini_repo"))
            .unwrap();
        let destination = root.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(destination).unwrap();
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
    (temporary, root)
}

#[test]
fn cli_indexes_and_searches_with_json_envelope() {
    let (_temporary, root) = fixture();
    Command::cargo_bin("ctx")
        .unwrap()
        .arg("index")
        .arg(&root)
        .assert()
        .success();
    let output = Command::cargo_bin("ctx")
        .unwrap()
        .current_dir(&root)
        .args(["search", "login", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["hits"][0]["path"], "auth.py");
    assert_eq!(value["hits"][0]["symbol"], "login");
}

#[test]
fn missing_index_is_a_readable_error_without_backtrace() {
    let project = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("ctx")
        .unwrap()
        .current_dir(project.path())
        .args(["search", "login", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("index absent"));
    assert!(!stderr.contains("stack backtrace"));
}

#[test]
fn background_lsp_process_cleans_its_pid_file() {
    let (_temporary, root) = fixture();
    Command::cargo_bin("ctx")
        .unwrap()
        .arg("index")
        .arg(&root)
        .assert()
        .success();
    let output = Command::cargo_bin("ctx")
        .unwrap()
        .current_dir(&root)
        .args([
            "lsp",
            "enrich",
            ".",
            "--language",
            "go",
            "--background",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["started"], true);
    let pid_file = root.join(".ctx/lsp/enrich.pid");
    let deadline = Instant::now() + Duration::from_secs(3);
    while pid_file.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!pid_file.exists(), "background PID file was not cleaned");
}
