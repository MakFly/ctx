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

fn metrics_cli(root: &Path) -> Command {
    let mut command = Command::cargo_bin("ctx").unwrap();
    command
        .current_dir(root)
        .env("CTX_DIR", root.join("index"))
        .env("CTX_GLOBAL_METRICS_DIR", root.join("global"))
        .env("NO_COLOR", "1");
    command
}

#[test]
fn dashboard_entrypoints_explain_missing_metrics_without_creating_an_index() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let help = metrics_cli(root).output().unwrap();
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("Usage:"));
    assert!(help.contains("Commands:"));
    assert!(help.contains("gain"));
    assert!(!help.contains("CTX Token Savings"));
    assert!(!root.join("index").exists());
    assert!(!root.join("global").exists());
    let mut previous = None;
    for arguments in [vec!["gain"], vec!["metrics", "report"]] {
        let output = metrics_cli(root).args(arguments).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("CTX Token Savings (Global Scope)"));
        assert!(text.contains("No usage recorded in the last 30 days."));
        assert!(text.contains("No metrics file exists at the path above."));
        assert!(text.contains(&root.join("global/metrics.sqlite").display().to_string()));
        assert!(text.contains("ctx --help"));
        assert!(!text.contains('\x1b'));
        if let Some(previous) = &previous {
            assert_eq!(&text, previous);
        }
        previous = Some(text);
    }
    assert!(!root.join("index").exists());
    assert!(!root.join("global").exists());
    metrics_cli(root)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("Usage:"));
}

#[test]
fn gain_reports_recorded_events_and_preserves_metrics_json() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir(root.join("index")).unwrap();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let event = serde_json::json!({
        "event_id": "dashboard-fixture",
        "created_at": created_at,
        "project_id": "dashboard-project",
        "project_name": "dashboard-fixture",
        "kind": "mcp_pack",
        "harness": "mcp",
        "model": "",
        "confidence": "estimated",
        "ctx_tokens": 64,
        "baseline_input_tokens": 256,
        "saved_input_tokens": 192,
        "duration_ms": 20,
        "cache_hit": false,
        "tool_calls": 1
    });
    fs::write(root.join("index/metrics.queue"), format!("{event}\n")).unwrap();
    metrics_cli(root)
        .args(["metrics", "drain", "--root"])
        .arg(root)
        .assert()
        .success();

    let output = metrics_cli(root).arg("gain").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Last 30 days"));
    assert!(text.contains("Total commands:          1\n"));
    assert!(text.contains("Est. tokens saved:       192 (75.0%, estimated)"));
    assert!(text.contains("ctx_pack"));
    assert!(text.contains("not a measured reduction in model usage or billing"));

    let mut previous = None;
    for arguments in [
        vec!["gain", "--global", "--since-days", "7", "--json"],
        vec!["metrics", "report", "--since-days", "7", "--json"],
    ] {
        let output = metrics_cli(root).args(arguments).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["scope"], "global");
        assert_eq!(value["since_days"], 7);
        assert_eq!(value["mcp_calls"], 1);
        assert_eq!(value["ctx_tokens"], 64);
        assert_eq!(value["saved_input_tokens"], 192);
        if let Some(previous) = &previous {
            assert_eq!(&value, previous);
        }
        previous = Some(value);
    }
}

#[test]
fn gain_project_view_identifies_its_metrics_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let output = metrics_cli(root)
        .args(["gain", "--project", "--since-days", "7"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Project Scope"));
    assert!(text.contains("No usage recorded in the last 7 days."));
    assert!(text.contains(&root.join("index/metrics.sqlite").display().to_string()));
    assert!(text.contains("Check CTX_DIR"));
    assert!(text.contains("ctx gain --global"));
    metrics_cli(root)
        .args(["gain", "--project", "--global"])
        .assert()
        .failure();
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
fn status_reports_index_drift_for_admitted_file_changes() {
    let (_temporary, root) = fixture();
    let index = || {
        let output = Command::cargo_bin("ctx")
            .unwrap()
            .current_dir(&root)
            .args(["index"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let status = || {
        let output = Command::cargo_bin("ctx")
            .unwrap()
            .current_dir(&root)
            .args(["status", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };

    index();
    let fresh = status();
    assert_eq!(fresh["stale"], false);
    assert_eq!(fresh["drift"]["added"], 0);
    assert_eq!(fresh["drift"]["modified"], 0);
    assert_eq!(fresh["drift"]["removed"], 0);

    fs::write(
        root.join("auth.py"),
        "def login():\n    return 'modified'\n",
    )
    .unwrap();
    let modified = status();
    assert_eq!(modified["stale"], true);
    assert_eq!(modified["drift"]["modified"], 1);

    index();
    fs::write(
        root.join("new_admitted.py"),
        "def new_file():\n    return True\n",
    )
    .unwrap();
    let added = status();
    assert_eq!(added["stale"], true);
    assert_eq!(added["drift"]["added"], 1);

    index();
    fs::remove_file(root.join("payments.py")).unwrap();
    let removed = status();
    assert_eq!(removed["stale"], true);
    assert_eq!(removed["drift"]["removed"], 1);

    index();
    let refreshed = status();
    assert_eq!(refreshed["stale"], false);
    assert_eq!(refreshed["drift"]["added"], 0);
    assert_eq!(refreshed["drift"]["modified"], 0);
    assert_eq!(refreshed["drift"]["removed"], 0);
}

#[test]
fn explore_refreshes_git_and_non_git_sources_before_reusing_briefings() {
    for git in [false, true] {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        let commit = || {
            for arguments in [
                vec!["add", "."],
                vec![
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-qm",
                    "fixture",
                ],
            ] {
                assert!(
                    std::process::Command::new("git")
                        .args(arguments)
                        .current_dir(root)
                        .status()
                        .unwrap()
                        .success()
                );
            }
        };
        if git {
            assert!(
                std::process::Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(root)
                    .status()
                    .unwrap()
                    .success()
            );
            fs::write(root.join(".gitignore"), ".ctx/\n").unwrap();
        }
        let explore = || {
            let result = Command::cargo_bin("ctx")
                .unwrap()
                .current_dir(root)
                .args(["explore", "--focus", "alpha", "--json"])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            serde_json::from_slice::<Value>(&result.stdout).unwrap()
        };
        fs::write(root.join("a.py"), "def alpha(old):\n    return old\n").unwrap();
        if git {
            commit();
        }
        let old = explore();
        fs::write(
            root.join("a.py"),
            "def alpha(new_content):\n    return new_content\n",
        )
        .unwrap();
        if git {
            commit();
        }
        let new = explore();
        assert!(
            // explore strips definitions to their signature, so the refreshed
            // source must show through the signature.
            new["hits"][0]["sig"]
                .as_str()
                .unwrap()
                .contains("new_content")
        );
        assert_ne!(old["cache_key"], new["cache_key"]);
        assert_eq!(explore()["skipped"], true);
    }
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

#[cfg(unix)]
#[test]
fn simultaneous_cached_runs_share_one_harness_execution() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command as Process, Stdio};
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repo");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("auth.py"), "def login():\n    return True\n").unwrap();
    fs::write(root.join(".gitignore"), ".ctx/\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(
            Process::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
    }
    ctx_code::indexer::index_repository(&root).unwrap();
    let fake = temporary.path().join("codex");
    fs::write(
        &fake,
        r#"#!/bin/sh
echo call >> "$FAKE_COUNT"
sleep 0.5
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then shift; out="$1"; fi
  shift
done
echo 'login: auth.py:1-2' > "$out"
echo '{"type":"turn.completed","usage":{"input_tokens":100}}'
"#,
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let count = temporary.path().join("calls");
    let command = || {
        let mut cmd = Process::new(assert_cmd::cargo::cargo_bin("ctx"));
        cmd.current_dir(&root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    temporary.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("FAKE_COUNT", &count)
            .args([
                "run",
                "where is login?",
                "--harness",
                "codex",
                "--model",
                "fake",
                "--json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    };
    let first = command().spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !count.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(count.exists());
    let second = command().spawn().unwrap();
    let results = [
        first.wait_with_output().unwrap(),
        second.wait_with_output().unwrap(),
    ];
    let values = results
        .iter()
        .map(|output| {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<Value>(&output.stdout).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(values[0]["cache_key"], values[1]["cache_key"]);
    assert_eq!(
        values
            .iter()
            .filter(|value| value["cached"] == true)
            .count(),
        1
    );
    assert_eq!(fs::read_to_string(count).unwrap().lines().count(), 1);
}

#[cfg(unix)]
#[test]
fn run_uses_exact_cache_and_bypasses_it_when_dirty() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command as StdCommand;

    let (temporary, root) = fixture();
    for arguments in [
        vec!["init"],
        vec!["config", "user.email", "ctx-test@example.invalid"],
        vec!["config", "user.name", "ctx test"],
        vec!["add", "."],
        vec!["commit", "-m", "fixture"],
    ] {
        let status = StdCommand::new("git")
            .args(arguments)
            .current_dir(&root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }
    let bin = temporary.path().join("fake-bin");
    fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("codex");
    fs::write(
        &fake,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "fake-codex 1.0"
  exit 0
fi
count=0
if [ -f "$FAKE_COUNT" ]; then count=$(cat "$FAKE_COUNT"); fi
count=$((count + 1))
echo "$count" > "$FAKE_COUNT"
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then shift; out="$1"; fi
  shift
done
echo 'login is defined at auth.py:5-7.' > "$out"
echo '{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20,"reasoning_output_tokens":5}}'
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions).unwrap();
    let count = temporary.path().join("count.txt");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let invoke = || {
        let output = Command::cargo_bin("ctx")
            .unwrap()
            .current_dir(&root)
            .env("PATH", &path)
            .env("FAKE_COUNT", &count)
            .args([
                "run",
                "where is login defined?",
                "--harness",
                "codex",
                "--model",
                "fake",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let first = invoke();
    assert_eq!(first["cached"], false);
    assert_eq!(first["usage"]["input_tokens"], 100);
    let second = invoke();
    assert_eq!(second["cached"], true);
    assert_eq!(second["usage"]["input_tokens"], Value::Null);
    assert!(second["duration_ms"].as_u64().unwrap() < 100);
    assert_eq!(fs::read_to_string(&count).unwrap().trim(), "1");

    fs::write(
        root.join(".ctx/config.toml"),
        "default_harness = \"codex\"\n[runners.codex]\nmodel = \"fake\"\neffort = \"high\"\n",
    )
    .unwrap();
    let automatic = Command::cargo_bin("ctx")
        .unwrap()
        .current_dir(&root)
        .env("PATH", &path)
        .env("FAKE_COUNT", &count)
        .args(["run", "where is login defined?", "--json"])
        .output()
        .unwrap();
    assert!(automatic.status.success());
    let automatic: Value = serde_json::from_slice(&automatic.stdout).unwrap();
    assert_eq!(automatic["cached"], true);
    assert_eq!(fs::read_to_string(&count).unwrap().trim(), "1");

    let status = Command::cargo_bin("ctx")
        .unwrap()
        .current_dir(&root)
        .args(["cache", "status", "--json"])
        .output()
        .unwrap();
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["entries"], 1);

    fs::write(root.join("auth.py"), "# dirty\n").unwrap();
    let dirty = invoke();
    assert_eq!(dirty["cached"], false);
    assert!(dirty["hint"].as_str().unwrap().contains("dirty"));
    assert_eq!(fs::read_to_string(count).unwrap().trim(), "2");
}

#[test]
fn init_writes_cache_defaults_and_disabled_embeddings() {
    let temporary = tempfile::tempdir().unwrap();
    Command::cargo_bin("ctx")
        .unwrap()
        .current_dir(temporary.path())
        .arg("init")
        .assert()
        .success();
    let config = fs::read_to_string(temporary.path().join(".ctx/config.toml")).unwrap();
    assert!(config.contains("max_size_mb = 256"));
    assert!(config.contains("[embeddings]"));
    assert!(config.contains("enabled = false"));
}

#[test]
fn cli_exposes_literal_regex_and_unicode_case_options() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("notes.custom"), "Kelvin\nfirst\nsecond\n").unwrap();
    for (pattern, mode) in [("kelvin", "literal"), ("(?s)first.*second", "regex")] {
        let output = Command::cargo_bin("ctx")
            .unwrap()
            .current_dir(root)
            .args(["search", pattern, "--mode", mode, "--ignore-case", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["hits"][0]["path"], "notes.custom");
    }
}
