use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::cache::{AgentResult, CacheStore, TokenUsage};
use crate::config::{ctx_dir, load_config, runner_settings};
use crate::db::{connect, get_meta};
use crate::gitinfo::git_info;
use crate::indexer::index_repository;
use crate::model::Envelope;
use crate::pack::pack_query;

const PROMPT_VERSION: &str = "ctx-run-v1";
const HARNESSES: &[&str] = &["codex", "claude", "opencode", "cursor"];

struct CacheIdentity<'a> {
    harness: &'a str,
    harness_version: &'a str,
    model: &'a str,
    effort: &'a str,
}

struct CacheRequest {
    key: String,
    value: Value,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub question: String,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub cache_mode: String,
    pub timeout: Duration,
}

pub async fn run_question(root: &Path, options: RunOptions) -> Result<AgentResult> {
    let started = Instant::now();
    let config = load_config(root)?;
    let harness = resolve_harness(&options.harness, config.default_harness.as_deref())?;
    let configured = runner_settings(root, &harness)?;
    let model = options.model.or(configured.model);
    let effort = options.effort.unwrap_or(configured.effort);
    ensure_index(root)?;
    let state = git_info(root);
    let state_id = repository_state(root, &state.sha)?;
    let pack = pack_query(&options.question, 800, "explore", root)?;
    let executable = detected_executable(&harness)?;
    require_project_integration(root, &harness)?;
    let version = executable_fingerprint(&executable)?;
    let cache_allowed = config.cache.enabled
        && options.cache_mode != "off"
        && !state.dirty
        && model.is_some()
        && !version.is_empty();
    let model_name = model.clone().unwrap_or_else(|| "default".to_owned());
    let cache_request = cache_allowed.then(|| {
        build_cache_request(
            root,
            &state_id,
            &options.question,
            CacheIdentity {
                harness: &harness,
                harness_version: &version,
                model: &model_name,
                effort: &effort,
            },
            &pack,
        )
    });
    let store = CacheStore::open(root)?;

    let cache_lookup_started = Instant::now();
    let key = cache_request.as_ref().map(|request| request.key.as_str());
    if options.cache_mode != "refresh"
        && let Some(key) = key
        && let Some(mut cached) = store.lookup(key)?
    {
        if valid_cached_result(root, &cached) {
            cached.cached = true;
            cached.duration_ms = started.elapsed().as_millis();
            cached.cache_lookup_ms = cache_lookup_started.elapsed().as_millis();
            cached.harness_ms = 0;
            cached.usage = TokenUsage::default();
            return Ok(cached);
        }
        store.delete(key)?;
    }

    let owner = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    if let Some(key) = key {
        let wait_started = Instant::now();
        loop {
            if store.acquire_lease(key, &owner, options.timeout.as_secs() + 30)? {
                break;
            }
            if options.cache_mode != "refresh"
                && let Some(mut cached) = store.lookup(key)?
                && valid_cached_result(root, &cached)
            {
                cached.cached = true;
                cached.duration_ms = started.elapsed().as_millis();
                cached.cache_lookup_ms = cache_lookup_started.elapsed().as_millis();
                cached.harness_ms = 0;
                cached.usage = TokenUsage::default();
                return Ok(cached);
            }
            if wait_started.elapsed() >= options.timeout {
                bail!("timeout en attente d'une exécution identique");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    let cache_lookup_ms = cache_lookup_started.elapsed().as_millis();
    let harness_started = Instant::now();
    let execution = execute_harness(
        root,
        &executable,
        &harness,
        model.as_deref(),
        &effort,
        &options.question,
        options.timeout,
    )
    .await;
    let harness_ms = harness_started.elapsed().as_millis();
    let (answer, usage) = match execution {
        Ok(value) => value,
        Err(error) => {
            if let Some(key) = key {
                store.release(key, &owner)?;
            }
            return Err(error);
        }
    };
    let mut result = AgentResult {
        answer,
        cached: false,
        cache_key: key.map(str::to_owned),
        harness,
        model: model_name,
        effort,
        sha: state.sha,
        duration_ms: started.elapsed().as_millis(),
        cache_lookup_ms,
        harness_ms,
        validation_ms: 0,
        usage,
        hits: pack.hits,
        coverage: pack.coverage,
        hint: pack.hint,
    };
    let validation_started = Instant::now();
    if let Some(key) = key {
        if valid_cached_result(root, &result) {
            result.validation_ms = validation_started.elapsed().as_millis();
            let stored = store.store_and_release(
                key,
                &owner,
                &cache_request.as_ref().expect("cache request missing").value,
                &result,
            );
            if let Err(error) = stored {
                let _ = store.release(key, &owner);
                result.hint = Some(format!(
                    "réponse non cachée: écriture cache impossible: {error}"
                ));
            } else if let Err(error) =
                store.prune(config.cache.max_age_days, config.cache.max_size_mb)
            {
                result.hint = Some(format!("réponse cachée; prune différé: {error}"));
            }
        } else {
            result.validation_ms = validation_started.elapsed().as_millis();
            store.release(key, &owner)?;
            result.hint = Some("réponse non cachée: aucune citation ctx vérifiable".to_owned());
        }
    } else if state.dirty {
        result.hint = Some("cache bypassé: working tree dirty".to_owned());
    } else if !config.cache.enabled {
        result.hint = Some("cache désactivé dans .ctx/config.toml".to_owned());
    } else if options.cache_mode == "off" {
        result.hint = Some("cache désactivé pour cette exécution".to_owned());
    } else if model.is_none() {
        result.hint = Some("cache bypassé: modèle non résolu".to_owned());
    }
    Ok(result)
}

fn ensure_index(root: &Path) -> Result<()> {
    let database = ctx_dir(root).join("index.sqlite");
    let state = git_info(root);
    let stale = if database.is_file() {
        let connection = connect(&database, false)?;
        get_meta(&connection, "indexed_sha", "")? != state.sha
    } else {
        true
    };
    if stale || state.dirty || state.sha == "nogit" {
        index_repository(root)?;
    }
    Ok(())
}

fn repository_state(root: &Path, git_sha: &str) -> Result<String> {
    if git_sha != "nogit" {
        return Ok(git_sha.to_owned());
    }
    let connection = connect(&ctx_dir(root).join("index.sqlite"), false)?;
    let mut statement = connection.prepare("SELECT path,content_hash FROM files ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut digest = Sha256::new();
    for row in rows {
        let (path, content_hash) = row?;
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(content_hash.as_bytes());
        digest.update([0]);
    }
    Ok(format!("nogit:{:x}", digest.finalize()))
}

fn resolve_harness(requested: &str, configured: Option<&str>) -> Result<String> {
    let value = if requested == "auto" {
        configured.context(
            "aucun default_harness dans .ctx/config.toml; utilisez --harness explicitement",
        )?
    } else {
        requested
    };
    if !HARNESSES.contains(&value) {
        bail!("harness inconnu: {value}");
    }
    Ok(value.to_owned())
}

fn require_project_integration(root: &Path, harness: &str) -> Result<()> {
    let configured = match harness {
        "opencode" => {
            file_contains_ctx(&root.join("opencode.json"))
                || file_contains_ctx(&root.join("opencode.jsonc"))
        }
        "cursor" => file_contains_ctx(&root.join(".cursor/mcp.json")),
        _ => true,
    };
    if !configured {
        bail!(
            "intégration {harness} absente; lancez `ctx install --target {harness}` avant `ctx run`"
        );
    }
    Ok(())
}

fn file_contains_ctx(path: &Path) -> bool {
    fs::read_to_string(path)
        .is_ok_and(|contents| contents.contains("\"mcp\"") && contents.contains("\"ctx\""))
}

fn detected_executable(harness: &str) -> Result<PathBuf> {
    let candidates: &[&str] = match harness {
        "cursor" => &["cursor-agent", "cursor"],
        "codex" => &["codex"],
        "claude" => &["claude"],
        "opencode" => &["opencode"],
        _ => &[],
    };
    let search_path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&search_path) {
        for candidate in candidates {
            for suffix in ["", ".exe", ".cmd", ".bat"] {
                let path = directory.join(format!("{candidate}{suffix}"));
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
    }
    bail!("harness {harness} absent du PATH")
}

fn executable_fingerprint(executable: &Path) -> Result<String> {
    let canonical = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_owned());
    let metadata = fs::metadata(&canonical)?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    Ok(format!(
        "{}:{}:{modified}",
        canonical.display(),
        metadata.len()
    ))
}

async fn execute_harness(
    root: &Path,
    executable: &Path,
    harness: &str,
    model: Option<&str>,
    effort: &str,
    question: &str,
    timeout: Duration,
) -> Result<(String, TokenUsage)> {
    let prompt = format!(
        "Use ctx_pack exactly once. Do not use shell search or edit files. Answer this codebase question with exact ctx path:line citations: {question}"
    );
    let mut command = Command::new(executable);
    command.current_dir(root).kill_on_drop(true);
    let output_file = temporary_output(root, harness)?;
    match harness {
        "codex" => {
            let current = std::env::current_exe()?;
            command.args([
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--sandbox",
                "read-only",
                "--json",
                "--output-last-message",
            ]);
            command.arg(&output_file);
            command.arg("-c").arg(format!(
                "mcp_servers.ctx.command={}",
                serde_json::to_string(&current.to_string_lossy().as_ref())?
            ));
            command
                .arg("-c")
                .arg("mcp_servers.ctx.args=[\"mcp\",\"--compact\"]");
            command
                .arg("-c")
                .arg("mcp_servers.ctx.enabled_tools=[\"ctx_pack\"]");
            command.arg("-c").arg(format!(
                "model_reasoning_effort={}",
                serde_json::to_string(effort)?
            ));
            if let Some(model) = model {
                command.args(["--model", model]);
            }
            command.arg(&prompt);
        }
        "claude" => {
            let current = std::env::current_exe()?;
            let mcp = json!({"mcpServers":{"ctx":{"type":"stdio","command":current,"args":["mcp","--compact"]}}});
            command.args([
                "-p",
                "--output-format",
                "json",
                "--permission-mode",
                "plan",
                "--permission-prompts",
                "none",
                "--no-session-persistence",
                "--strict-mcp-config",
                "--mcp-config",
            ]);
            command.arg(mcp.to_string());
            command.args(["--allowedTools", "mcp__ctx__ctx_pack", "--effort", effort]);
            if let Some(model) = model {
                command.args(["--model", model]);
            }
            command.arg(&prompt);
        }
        "opencode" => {
            command.args(["run", "--format", "json", "--dir"]);
            command.arg(root);
            command.args(["--variant", effort]);
            if let Some(model) = model {
                command.args(["--model", model]);
            }
            command.arg(&prompt);
        }
        "cursor" => {
            command.args(["-p", "--output-format", "json"]);
            if let Some(model) = model {
                command.args(["--model", model]);
            }
            command.arg(&prompt);
        }
        _ => bail!("harness inconnu: {harness}"),
    }
    let output = match tokio::time::timeout(timeout, command.output()).await {
        Ok(output) => output?,
        Err(_) => {
            let _ = fs::remove_file(&output_file);
            bail!("timeout harness après {}s", timeout.as_secs());
        }
    };
    const MAX_OUTPUT_BYTES: usize = 4 * 1_048_576;
    if output.stdout.len().saturating_add(output.stderr.len()) > MAX_OUTPUT_BYTES {
        let _ = fs::remove_file(&output_file);
        bail!("sortie {harness} supérieure à 4 MiB");
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_file(&output_file);
        bail!("{harness} a échoué: {}", stderr.trim());
    }
    let stdout = String::from_utf8(output.stdout).context("sortie harness non UTF-8")?;
    let answer = if harness == "codex" {
        fs::read_to_string(&output_file)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| extract_answer(&stdout))
    } else {
        extract_answer(&stdout)
    };
    let _ = fs::remove_file(output_file);
    if answer.trim().is_empty() {
        bail!("{harness} n'a retourné aucune réponse");
    }
    Ok((answer.trim().to_owned(), extract_usage(&stdout)))
}

fn temporary_output(root: &Path, harness: &str) -> Result<PathBuf> {
    let directory = ctx_dir(root).join("run");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(directory.join(format!("{harness}-{}-{nonce}.txt", std::process::id())))
}

fn extract_answer(stdout: &str) -> String {
    let mut answer = String::new();
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(result) = find_string(&value, &["result", "answer"]) {
            answer = result;
        } else if let Some(text) = assistant_text(&value) {
            answer.push_str(&text);
        }
    }
    if answer.is_empty()
        && let Ok(value) = serde_json::from_str::<Value>(stdout)
        && let Some(result) = find_string(&value, &["result", "answer"])
    {
        return result;
    }
    if answer.is_empty() {
        stdout.trim().to_owned()
    } else {
        answer
    }
}

fn assistant_text(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) == Some("assistant") {
        let content = value.pointer("/message/content")?.as_array()?;
        let text = content
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<String>();
        return (!text.is_empty()).then_some(text);
    }
    value
        .pointer("/part/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(value) = object.get(*key).and_then(Value::as_str) {
                return Some(value.to_owned());
            }
        }
        for child in object.values() {
            if let Some(value) = find_string(child, keys) {
                return Some(value);
            }
        }
    }
    value
        .as_array()?
        .iter()
        .find_map(|child| find_string(child, keys))
}

fn extract_usage(stdout: &str) -> TokenUsage {
    let mut usage = TokenUsage::default();
    for line in stdout.lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            usage.input_tokens = find_number(&value, &["input_tokens"]).or(usage.input_tokens);
            usage.output_tokens = find_number(&value, &["output_tokens"]).or(usage.output_tokens);
            usage.reasoning_tokens =
                find_number(&value, &["reasoning_output_tokens", "reasoning_tokens"])
                    .or(usage.reasoning_tokens);
        }
    }
    usage
}

fn find_number(value: &Value, keys: &[&str]) -> Option<u64> {
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(value) = object.get(*key).and_then(Value::as_u64) {
                return Some(value);
            }
        }
        for child in object.values() {
            if let Some(value) = find_number(child, keys) {
                return Some(value);
            }
        }
    }
    value
        .as_array()?
        .iter()
        .find_map(|child| find_number(child, keys))
}

fn build_cache_request(
    root: &Path,
    sha: &str,
    question: &str,
    identity: CacheIdentity<'_>,
    pack: &Envelope,
) -> CacheRequest {
    let request = json!({
        "repo": root.to_string_lossy(),
        "sha": sha,
        "question": normalize_question(question),
        "harness": identity.harness,
        "harness_version": identity.harness_version,
        "model": identity.model,
        "effort": identity.effort,
        "prompt_version": PROMPT_VERSION,
        "ctx_version": env!("CARGO_PKG_VERSION"),
        "integration_digest": integration_digest(root, identity.harness),
        "pack_digest": pack_digest(pack),
    });
    CacheRequest {
        key: format!("{:x}", Sha256::digest(request.to_string().as_bytes())),
        value: request,
    }
}

fn integration_digest(root: &Path, harness: &str) -> String {
    let paths: &[&str] = match harness {
        "codex" => &[
            ".agents/skills/ctx-explore/SKILL.md",
            ".codex/agents/ctx-explorer.toml",
            "AGENTS.md",
        ],
        "claude" => &[
            ".claude/skills/ctx-explore/SKILL.md",
            ".claude/agents/ctx-explorer.md",
            "CLAUDE.md",
        ],
        "opencode" => &[
            ".opencode/skills/ctx-explore/SKILL.md",
            ".opencode/agents/ctx-explorer.md",
            "opencode.json",
            "opencode.jsonc",
        ],
        "cursor" => &[
            ".cursor/skills/ctx-explore/SKILL.md",
            ".cursor/agents/ctx-explorer.md",
            ".cursor/mcp.json",
        ],
        _ => &[],
    };
    let mut digest = Sha256::new();
    for path in paths {
        digest.update(path.as_bytes());
        if let Ok(contents) = fs::read(root.join(path)) {
            digest.update(contents);
        }
    }
    format!("{:x}", digest.finalize())
}

fn pack_digest(pack: &Envelope) -> String {
    let stable = json!({
        "hits": pack.hits,
        "tokens": pack.tokens,
        "coverage": pack.coverage,
        "hint": pack.hint,
    });
    format!("{:x}", Sha256::digest(stable.to_string().as_bytes()))
}

fn normalize_question(question: &str) -> String {
    question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn valid_cached_result(root: &Path, result: &AgentResult) -> bool {
    if result.hits.is_empty() || !result.hits.iter().all(|hit| root.join(&hit.path).is_file()) {
        return false;
    }
    let Ok(pattern) = Regex::new(r"([A-Za-z0-9_./-]+):(\d+)(?:-(\d+))?") else {
        return false;
    };
    let citations = pattern
        .captures_iter(&result.answer)
        .filter_map(|capture| {
            Some((
                capture.get(1)?.as_str(),
                capture.get(2)?.as_str().parse::<usize>().ok()?,
                capture
                    .get(3)
                    .and_then(|value| value.as_str().parse::<usize>().ok()),
            ))
        })
        .collect::<Vec<_>>();
    !citations.is_empty()
        && citations.iter().all(|(path, start, end)| {
            result.hits.iter().any(|hit| {
                hit.path == *path
                    && *start >= hit.start
                    && *start <= hit.end
                    && end.is_none_or(|end| end >= *start && end <= hit.end)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_common_harness_answers_and_usage() {
        let output = r#"{"type":"assistant","message":{"content":[{"text":"first"}]}}
{"type":"result","result":"auth.py:5-7","usage":{"input_tokens":12,"output_tokens":3}}"#;
        assert_eq!(extract_answer(output), "auth.py:5-7");
        let usage = extract_usage(output);
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(3));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn all_headless_adapters_parse_a_machine_response() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let fake = temporary.path().join("agent");
        fs::write(
            &fake,
            r#"#!/bin/sh
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then shift; out="$1"; fi
  shift
done
if [ -n "$out" ]; then echo 'auth.py:5-7' > "$out"; fi
echo '{"type":"result","result":"auth.py:5-7","usage":{"input_tokens":12,"output_tokens":3}}'
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake, permissions).unwrap();
        for harness in HARNESSES {
            let (answer, usage) = execute_harness(
                temporary.path(),
                &fake,
                harness,
                Some("test"),
                "high",
                "where is login?",
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            assert_eq!(answer, "auth.py:5-7", "adapter {harness}");
            assert_eq!(usage.input_tokens, Some(12));
        }
    }
}
