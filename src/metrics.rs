use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fs2::FileExt;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cache::{AgentResult, TokenUsage};
use crate::config::{ctx_dir, global_metrics_dir, load_config, repo_root};
use crate::model::Envelope;

const QUEUE_FILE: &str = "metrics.queue";
const WORKER_LOCK: &str = "metrics.worker.lock";
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events(
  event_id TEXT PRIMARY KEY,
  created_at INTEGER NOT NULL,
  project_id TEXT NOT NULL,
  project_name TEXT NOT NULL,
  kind TEXT NOT NULL,
  harness TEXT NOT NULL,
  model TEXT NOT NULL,
  confidence TEXT NOT NULL,
  query_hash TEXT,
  input_tokens INTEGER,
  cached_input_tokens INTEGER,
  output_tokens INTEGER,
  reasoning_tokens INTEGER,
  ctx_tokens INTEGER,
  baseline_input_tokens INTEGER,
  baseline_output_tokens INTEGER,
  saved_input_tokens INTEGER,
  saved_output_tokens INTEGER,
  duration_ms INTEGER,
  cache_hit INTEGER NOT NULL DEFAULT 0,
  tool_calls INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_metrics_events_created ON events(created_at);
CREATE INDEX IF NOT EXISTS idx_metrics_events_project ON events(project_id,created_at);
CREATE TABLE IF NOT EXISTS pricing(
  model TEXT PRIMARY KEY,
  input_per_million REAL NOT NULL,
  cached_input_per_million REAL NOT NULL,
  output_per_million REAL NOT NULL,
  source TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricEvent {
    pub event_id: String,
    pub created_at: i64,
    pub project_id: String,
    pub project_name: String,
    pub kind: String,
    pub harness: String,
    pub model: String,
    pub confidence: String,
    pub query_hash: Option<String>,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub ctx_tokens: Option<u64>,
    pub baseline_input_tokens: Option<u64>,
    pub baseline_output_tokens: Option<u64>,
    pub saved_input_tokens: Option<u64>,
    pub saved_output_tokens: Option<u64>,
    pub duration_ms: Option<u64>,
    pub cache_hit: bool,
    pub tool_calls: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pricing {
    pub model: String,
    pub input_per_million: f64,
    pub cached_input_per_million: f64,
    pub output_per_million: f64,
    pub source: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsReport {
    pub scope: String,
    pub since_days: u64,
    pub events: u64,
    pub mcp_calls: u64,
    pub run_calls: u64,
    pub hook_events: u64,
    pub cache_hits: u64,
    pub tool_calls: u64,
    pub ctx_tokens: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub uncached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_duration_ms: u64,
    pub estimated_cost_usd: Option<f64>,
    pub baseline_input_tokens: Option<u64>,
    pub saved_input_tokens: Option<u64>,
    pub saved_output_tokens: Option<u64>,
    pub estimated_saved_cost_usd: Option<f64>,
    pub savings_confidence: String,
    pub by_operation: Vec<AggregateGroup>,
    pub by_project: Vec<AggregateGroup>,
    pub by_harness: Vec<AggregateGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AggregateGroup {
    pub id: String,
    pub name: String,
    pub events: u64,
    pub cache_hits: u64,
    pub ctx_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    pub baseline_input_tokens: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub saved_input_tokens: Option<u64>,
    pub estimated_saved_cost_usd: Option<f64>,
}

#[derive(Debug, Default)]
struct AggregateState {
    events: u64,
    mcp_calls: u64,
    run_calls: u64,
    hook_events: u64,
    cache_hits: u64,
    tool_calls: u64,
    ctx_tokens: u64,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    cost_usd: Option<f64>,
    baseline_input_tokens: u64,
    saved_input_tokens: u64,
    saved_output_tokens: u64,
    savings_events: u64,
    exact_savings_events: u64,
    estimated_savings_events: u64,
    saved_cost_usd: Option<f64>,
    duration_ms: u64,
}

pub fn project_id(root: &Path) -> Result<(String, String)> {
    let root = repo_root(root)?;
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project")
        .to_owned();
    let digest = Sha256::digest(root.to_string_lossy().as_bytes());
    Ok((format!("{digest:x}"), name))
}

pub fn hash_query(query: &str) -> String {
    format!("{:x}", Sha256::digest(query.as_bytes()))
}

pub fn record_mcp_async(
    root: &Path,
    kind: &str,
    harness: &str,
    query: &str,
    envelope: &Envelope,
) -> Result<()> {
    let (project_id, project_name) = project_id(root)?;
    enqueue(
        root,
        MetricEvent {
            event_id: new_event_id(),
            created_at: now_seconds(),
            project_id,
            project_name,
            kind: kind.to_owned(),
            harness: harness.to_owned(),
            model: String::new(),
            confidence: "unknown".to_owned(),
            query_hash: Some(hash_query(query)),
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            reasoning_tokens: None,
            ctx_tokens: Some(envelope.tokens as u64),
            baseline_input_tokens: None,
            baseline_output_tokens: None,
            saved_input_tokens: None,
            saved_output_tokens: None,
            duration_ms: Some(envelope.freshness_ms as u64),
            cache_hit: false,
            tool_calls: 1,
            baseline_paths: if matches!(kind, "mcp_pack" | "mcp_search" | "mcp_graph") {
                Some(envelope.hits.iter().map(|hit| hit.path.clone()).collect())
            } else {
                None
            },
        },
    )
}

pub fn record_agent_async(root: &Path, result: &AgentResult) -> Result<()> {
    let (project_id, project_name) = project_id(root)?;
    let confidence = if result.usage.input_tokens.is_some() || result.usage.output_tokens.is_some()
    {
        "exact"
    } else {
        "unknown"
    };
    enqueue(
        root,
        MetricEvent {
            event_id: new_event_id(),
            created_at: now_seconds(),
            project_id,
            project_name,
            kind: "ctx_run".to_owned(),
            harness: result.harness.clone(),
            model: result.model.clone(),
            confidence: confidence.to_owned(),
            query_hash: result.question_hash.clone(),
            input_tokens: result.usage.input_tokens,
            cached_input_tokens: result.usage.cached_input_tokens,
            output_tokens: result.usage.output_tokens,
            reasoning_tokens: result.usage.reasoning_tokens,
            ctx_tokens: Some(result.ctx_tokens as u64),
            baseline_input_tokens: None,
            baseline_output_tokens: None,
            saved_input_tokens: None,
            saved_output_tokens: None,
            duration_ms: Some(result.duration_ms as u64),
            cache_hit: result.cached,
            tool_calls: 1,
            baseline_paths: None,
        },
    )
}

pub fn record_hook_async(
    root: &Path,
    event_kind: &str,
    harness: &str,
    model: Option<&str>,
    payload: &Value,
) -> Result<()> {
    let (project_id, project_name) = project_id(root)?;
    let usage = extract_usage(payload);
    let model = model
        .map(str::to_owned)
        .or_else(|| find_string(payload, &["model", "model_id"]))
        .unwrap_or_default();
    let confidence = if usage.input_tokens.is_some() || usage.output_tokens.is_some() {
        "exact"
    } else {
        "unknown"
    };
    enqueue(
        root,
        MetricEvent {
            event_id: new_event_id(),
            created_at: now_seconds(),
            project_id,
            project_name,
            kind: format!("hook:{event_kind}"),
            harness: harness.to_owned(),
            model,
            confidence: confidence.to_owned(),
            query_hash: None,
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            ctx_tokens: None,
            baseline_input_tokens: None,
            baseline_output_tokens: None,
            saved_input_tokens: None,
            saved_output_tokens: None,
            duration_ms: None,
            cache_hit: false,
            tool_calls: 0,
            baseline_paths: None,
        },
    )
}

pub fn record_baseline_async(
    root: &Path,
    harness: &str,
    model: &str,
    query_hash: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> Result<()> {
    let (project_id, project_name) = project_id(root)?;
    enqueue(
        root,
        MetricEvent {
            event_id: new_event_id(),
            created_at: now_seconds(),
            project_id,
            project_name,
            kind: "baseline".to_owned(),
            harness: harness.to_owned(),
            model: model.to_owned(),
            confidence: "exact".to_owned(),
            query_hash: Some(query_hash.to_owned()),
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            reasoning_tokens: None,
            ctx_tokens: None,
            baseline_input_tokens: Some(input_tokens),
            baseline_output_tokens: Some(output_tokens),
            saved_input_tokens: None,
            saved_output_tokens: None,
            duration_ms: None,
            cache_hit: false,
            tool_calls: 0,
            baseline_paths: None,
        },
    )
}

pub fn enqueue(root: &Path, event: MetricEvent) -> Result<()> {
    if !load_config(root)?.metrics.enabled {
        return Ok(());
    }
    let root = repo_root(root)?;
    let directory = ctx_dir(&root);
    fs::create_dir_all(&directory)?;
    let queue = directory.join(QUEUE_FILE);
    let mut file = OpenOptions::new().create(true).append(true).open(&queue)?;
    serde_json::to_writer(&mut file, &event)?;
    file.write_all(b"\n")?;
    file.flush()?;
    spawn_worker(&root);
    Ok(())
}

pub fn drain(root: &Path) -> Result<usize> {
    let root = repo_root(root)?;
    let directory = ctx_dir(&root);
    fs::create_dir_all(&directory)?;
    let lock_path = directory.join(WORKER_LOCK);
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(0);
    }
    let mut processed = 0;
    loop {
        let queue = directory.join(QUEUE_FILE);
        if !queue.is_file() {
            break;
        }
        let snapshot = directory.join(format!(
            "metrics.queue.{}.{}",
            std::process::id(),
            now_nanos()
        ));
        if fs::rename(&queue, &snapshot).is_err() {
            break;
        }
        let file = fs::File::open(&snapshot)?;
        let mut events = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            events.push(
                serde_json::from_str::<MetricEvent>(&line)
                    .with_context(|| format!("invalid metrics event in {}", snapshot.display()))?,
            );
        }
        for event in &events {
            store_event(&root, event)?;
            processed += 1;
        }
        fs::remove_file(snapshot)?;
    }
    let _ = lock.unlock();
    Ok(processed)
}

pub fn status(root: &Path, global: bool) -> Result<Value> {
    let path = database_path(root, global);
    if !path.is_file() {
        return Ok(json!({
            "scope": if global { "global" } else { "project" },
            "database": path,
            "exists": false,
            "queued": if global { Value::Null } else { json!(queue_bytes(root)?) }
        }));
    }
    let connection = open_database(&path)?;
    let (events, cache_hits, ctx_tokens, input_tokens, output_tokens): (u64, u64, u64, u64, u64) =
        connection.query_row(
            "SELECT count(*), COALESCE(sum(cache_hit),0), COALESCE(sum(ctx_tokens),0),
                    COALESCE(sum(input_tokens),0), COALESCE(sum(output_tokens),0) FROM events",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    Ok(json!({
        "scope": if global { "global" } else { "project" },
        "database": path,
        "exists": true,
        "events": events,
        "cache_hits": cache_hits,
        "ctx_tokens": ctx_tokens,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "queued": if global { Value::Null } else { json!(queue_bytes(root)?) }
    }))
}

pub fn report(root: &Path, global: bool, since_days: u64) -> Result<MetricsReport> {
    let path = database_path(root, global);
    if !path.is_file() {
        return Ok(empty_report(
            if global { "global" } else { "project" },
            since_days,
        ));
    }
    let connection = open_database(&path)?;
    let cutoff = now_seconds().saturating_sub((since_days as i64).saturating_mul(86_400));
    let events = read_events(&connection, cutoff)?;
    let mut pricing = read_pricing(&connection)?;
    if !global {
        let global_path = global_metrics_dir().join("metrics.sqlite");
        if global_path.is_file() {
            let global = open_database(&global_path)?;
            pricing.extend(read_pricing(&global)?);
        }
    }
    build_report(
        if global { "global" } else { "project" },
        since_days,
        &events,
        &pricing,
    )
}

pub fn export(root: &Path, global: bool, since_days: u64) -> Result<Vec<MetricEvent>> {
    let path = database_path(root, global);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let connection = open_database(&path)?;
    let cutoff = now_seconds().saturating_sub((since_days as i64).saturating_mul(86_400));
    read_events(&connection, cutoff)
}

pub fn set_pricing(
    model: &str,
    input_per_million: f64,
    cached_input_per_million: f64,
    output_per_million: f64,
) -> Result<Pricing> {
    let path = global_metrics_dir().join("metrics.sqlite");
    let connection = open_database(&path)?;
    let pricing = Pricing {
        model: model.to_owned(),
        input_per_million,
        cached_input_per_million,
        output_per_million,
        source: "user".to_owned(),
        updated_at: now_seconds(),
    };
    connection.execute(
        "INSERT INTO pricing(model,input_per_million,cached_input_per_million,output_per_million,source,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(model) DO UPDATE SET input_per_million=excluded.input_per_million,
           cached_input_per_million=excluded.cached_input_per_million,
           output_per_million=excluded.output_per_million,source=excluded.source,
           updated_at=excluded.updated_at",
        params![
            pricing.model,
            pricing.input_per_million,
            pricing.cached_input_per_million,
            pricing.output_per_million,
            pricing.source,
            pricing.updated_at
        ],
    )?;
    Ok(pricing)
}

pub fn list_pricing() -> Result<Vec<Pricing>> {
    let path = global_metrics_dir().join("metrics.sqlite");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let connection = open_database(&path)?;
    let mut statement = connection.prepare(
        "SELECT model,input_per_million,cached_input_per_million,output_per_million,source,updated_at
         FROM pricing ORDER BY model",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Pricing {
            model: row.get(0)?,
            input_per_million: row.get(1)?,
            cached_input_per_million: row.get(2)?,
            output_per_million: row.get(3)?,
            source: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn store_event(root: &Path, event: &MetricEvent) -> Result<()> {
    let mut event = event.clone();
    if event.baseline_input_tokens.is_none()
        && let Some(paths) = event.baseline_paths.take()
    {
        let baseline = estimate_source_tokens(root, &paths);
        if baseline > 0 {
            event.baseline_input_tokens = Some(baseline);
            event.saved_input_tokens = event
                .ctx_tokens
                .map(|tokens| baseline.saturating_sub(tokens));
            event.confidence = "estimated".to_owned();
        }
    }
    let project_path = ctx_dir(root).join("metrics.sqlite");
    let project = open_database(&project_path)?;
    insert_event(&project, &event)?;
    if load_config(root)?.metrics.global {
        let global = open_database(&global_metrics_dir().join("metrics.sqlite"))?;
        insert_event(&global, &event)?;
    }
    Ok(())
}

fn insert_event(connection: &Connection, event: &MetricEvent) -> Result<()> {
    connection.execute(
        "INSERT OR IGNORE INTO events(
           event_id,created_at,project_id,project_name,kind,harness,model,confidence,query_hash,
           input_tokens,cached_input_tokens,output_tokens,reasoning_tokens,ctx_tokens,
           baseline_input_tokens,baseline_output_tokens,saved_input_tokens,saved_output_tokens,
           duration_ms,cache_hit,tool_calls
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
        params![
            event.event_id,
            event.created_at,
            event.project_id,
            event.project_name,
            event.kind,
            event.harness,
            event.model,
            event.confidence,
            event.query_hash,
            event.input_tokens,
            event.cached_input_tokens,
            event.output_tokens,
            event.reasoning_tokens,
            event.ctx_tokens,
            event.baseline_input_tokens,
            event.baseline_output_tokens,
            event.saved_input_tokens,
            event.saved_output_tokens,
            event.duration_ms,
            event.cache_hit as i64,
            event.tool_calls,
        ],
    )?;
    Ok(())
}

fn open_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;",
    )?;
    connection.execute_batch(SCHEMA)?;
    Ok(connection)
}

fn database_path(root: &Path, global: bool) -> PathBuf {
    if global {
        global_metrics_dir().join("metrics.sqlite")
    } else {
        ctx_dir(root).join("metrics.sqlite")
    }
}

fn queue_bytes(root: &Path) -> Result<u64> {
    Ok(fs::metadata(ctx_dir(root).join(QUEUE_FILE))
        .map(|metadata| metadata.len())
        .unwrap_or(0))
}

fn spawn_worker(root: &Path) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let _ = Command::new(executable)
        .args(["metrics", "drain", "--root"])
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir(root)
        .spawn();
}

fn read_events(connection: &Connection, cutoff: i64) -> Result<Vec<MetricEvent>> {
    let mut statement = connection.prepare(
        "SELECT event_id,created_at,project_id,project_name,kind,harness,model,confidence,query_hash,
                input_tokens,cached_input_tokens,output_tokens,reasoning_tokens,ctx_tokens,
                baseline_input_tokens,baseline_output_tokens,saved_input_tokens,saved_output_tokens,
                duration_ms,cache_hit,tool_calls
         FROM events WHERE created_at>=?1 ORDER BY created_at,event_id",
    )?;
    let rows = statement.query_map([cutoff], |row| {
        Ok(MetricEvent {
            event_id: row.get(0)?,
            created_at: row.get(1)?,
            project_id: row.get(2)?,
            project_name: row.get(3)?,
            kind: row.get(4)?,
            harness: row.get(5)?,
            model: row.get(6)?,
            confidence: row.get(7)?,
            query_hash: row.get(8)?,
            input_tokens: row.get(9)?,
            cached_input_tokens: row.get(10)?,
            output_tokens: row.get(11)?,
            reasoning_tokens: row.get(12)?,
            ctx_tokens: row.get(13)?,
            baseline_input_tokens: row.get(14)?,
            baseline_output_tokens: row.get(15)?,
            saved_input_tokens: row.get(16)?,
            saved_output_tokens: row.get(17)?,
            duration_ms: row.get(18)?,
            cache_hit: row.get::<_, i64>(19)? != 0,
            tool_calls: row.get(20)?,
            baseline_paths: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_pricing(connection: &Connection) -> Result<Vec<Pricing>> {
    let mut statement = connection.prepare(
        "SELECT model,input_per_million,cached_input_per_million,output_per_million,source,updated_at
         FROM pricing",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Pricing {
            model: row.get(0)?,
            input_per_million: row.get(1)?,
            cached_input_per_million: row.get(2)?,
            output_per_million: row.get(3)?,
            source: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn build_report(
    scope: &str,
    since_days: u64,
    events: &[MetricEvent],
    pricing: &[Pricing],
) -> Result<MetricsReport> {
    let mut state = AggregateState::default();
    let mut projects = std::collections::BTreeMap::new();
    let mut harnesses = std::collections::BTreeMap::new();
    let mut operations = std::collections::BTreeMap::new();
    let baselines = events
        .iter()
        .filter_map(|event| {
            if event.kind != "baseline" {
                return None;
            }
            Some((
                (
                    event.project_id.clone(),
                    event.query_hash.clone()?,
                    event.harness.clone(),
                    event.model.clone(),
                ),
                (event.baseline_input_tokens?, event.baseline_output_tokens?),
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();
    for original in events {
        let mut event = original.clone();
        if event.kind == "ctx_run"
            && event.saved_input_tokens.is_none()
            && let Some(query_hash) = event.query_hash.clone()
            && let Some((baseline_input, baseline_output)) = baselines.get(&(
                event.project_id.clone(),
                query_hash,
                event.harness.clone(),
                event.model.clone(),
            ))
        {
            event.baseline_input_tokens = Some(*baseline_input);
            event.baseline_output_tokens = Some(*baseline_output);
            event.saved_input_tokens = event
                .input_tokens
                .map(|value| baseline_input.saturating_sub(value));
            event.saved_output_tokens = event
                .output_tokens
                .map(|value| baseline_output.saturating_sub(value));
        }
        accumulate(&mut state, &event, pricing);
        accumulate_group(
            &mut projects,
            &event.project_id,
            &event.project_name,
            &event,
            pricing,
        );
        accumulate_group(
            &mut harnesses,
            &event.harness,
            &event.harness,
            &event,
            pricing,
        );
        if event.kind != "baseline" && !event.kind.starts_with("hook:") {
            accumulate_group(&mut operations, &event.kind, &event.kind, &event, pricing);
        }
    }
    Ok(MetricsReport {
        scope: scope.to_owned(),
        since_days,
        events: state.events,
        mcp_calls: state.mcp_calls,
        run_calls: state.run_calls,
        hook_events: state.hook_events,
        cache_hits: state.cache_hits,
        tool_calls: state.tool_calls,
        ctx_tokens: state.ctx_tokens,
        input_tokens: state.input_tokens,
        cached_input_tokens: state.cached_input_tokens,
        uncached_input_tokens: state.input_tokens.saturating_sub(state.cached_input_tokens),
        output_tokens: state.output_tokens,
        reasoning_tokens: state.reasoning_tokens,
        total_duration_ms: state.duration_ms,
        estimated_cost_usd: state.cost_usd,
        baseline_input_tokens: (state.baseline_input_tokens > 0)
            .then_some(state.baseline_input_tokens),
        saved_input_tokens: (state.savings_events > 0).then_some(state.saved_input_tokens),
        saved_output_tokens: (state.saved_output_tokens > 0).then_some(state.saved_output_tokens),
        estimated_saved_cost_usd: state.saved_cost_usd,
        savings_confidence: if state.exact_savings_events > 0 {
            "exact".to_owned()
        } else if state.estimated_savings_events > 0 {
            "estimated".to_owned()
        } else {
            "unknown".to_owned()
        },
        by_operation: operations.into_values().collect(),
        by_project: projects.into_values().collect(),
        by_harness: harnesses.into_values().collect(),
    })
}

fn accumulate(state: &mut AggregateState, event: &MetricEvent, pricing: &[Pricing]) {
    state.events += 1;
    state.tool_calls += event.tool_calls;
    state.ctx_tokens += event.ctx_tokens.unwrap_or(0);
    state.input_tokens += event.input_tokens.unwrap_or(0);
    state.cached_input_tokens += event.cached_input_tokens.unwrap_or(0);
    state.output_tokens += event.output_tokens.unwrap_or(0);
    state.reasoning_tokens += event.reasoning_tokens.unwrap_or(0);
    state.duration_ms += event.duration_ms.unwrap_or(0);
    state.cache_hits += event.cache_hit as u64;
    if matches!(
        event.kind.as_str(),
        "mcp_pack" | "mcp_search" | "mcp_graph" | "mcp_file"
    ) {
        state.mcp_calls += 1;
    }
    if event.kind == "ctx_run" {
        state.run_calls += 1;
    }
    if event.kind.starts_with("hook:") {
        state.hook_events += 1;
    }
    if event.kind != "baseline" {
        if let Some(value) = event.baseline_input_tokens {
            state.baseline_input_tokens += value;
        }
        if let Some(value) = event.saved_input_tokens {
            state.saved_input_tokens += value;
            state.savings_events += 1;
            if event.confidence == "estimated" {
                state.estimated_savings_events += 1;
            } else {
                state.exact_savings_events += 1;
            }
            if let Some(cost) = event_savings_cost(event, pricing) {
                *state.saved_cost_usd.get_or_insert(0.0) += cost;
            }
        }
    }
    state.saved_output_tokens += event.saved_output_tokens.unwrap_or(0);
    if let Some(cost) = event_cost(event, pricing) {
        *state.cost_usd.get_or_insert(0.0) += cost;
    }
}

fn accumulate_group(
    groups: &mut std::collections::BTreeMap<String, AggregateGroup>,
    id: &str,
    name: &str,
    event: &MetricEvent,
    pricing: &[Pricing],
) {
    let group = groups
        .entry(id.to_owned())
        .or_insert_with(|| AggregateGroup {
            id: id.to_owned(),
            name: name.to_owned(),
            events: 0,
            cache_hits: 0,
            ctx_tokens: 0,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: 0,
            baseline_input_tokens: None,
            estimated_cost_usd: None,
            saved_input_tokens: None,
            estimated_saved_cost_usd: None,
        });
    group.events += 1;
    group.cache_hits += event.cache_hit as u64;
    group.ctx_tokens += event.ctx_tokens.unwrap_or(0);
    group.input_tokens += event.input_tokens.unwrap_or(0);
    group.output_tokens += event.output_tokens.unwrap_or(0);
    group.duration_ms += event.duration_ms.unwrap_or(0);
    if event.kind != "baseline" {
        if let Some(baseline) = event.baseline_input_tokens {
            *group.baseline_input_tokens.get_or_insert(0) += baseline;
        }
    }
    if let Some(cost) = event_cost(event, pricing) {
        *group.estimated_cost_usd.get_or_insert(0.0) += cost;
    }
    if let Some(saved) = event.saved_input_tokens {
        *group.saved_input_tokens.get_or_insert(0) += saved;
    }
    if let Some(cost) = event_savings_cost(event, pricing) {
        *group.estimated_saved_cost_usd.get_or_insert(0.0) += cost;
    }
}

fn event_cost(event: &MetricEvent, pricing: &[Pricing]) -> Option<f64> {
    let rate = pricing_for_event(event, pricing)?;
    let input = event.input_tokens?;
    let cached = event.cached_input_tokens.unwrap_or(0).min(input);
    let output = event.output_tokens.unwrap_or(0);
    let uncached = input.saturating_sub(cached);
    Some(
        (uncached as f64 * rate.input_per_million
            + cached as f64 * rate.cached_input_per_million
            + output as f64 * rate.output_per_million)
            / 1_000_000.0,
    )
}

fn event_savings_cost(event: &MetricEvent, pricing: &[Pricing]) -> Option<f64> {
    let rate = pricing_for_event(event, pricing)?;
    let baseline_input = event.baseline_input_tokens?;
    if let (Some(baseline_output), Some(actual_cost)) =
        (event.baseline_output_tokens, event_cost(event, pricing))
    {
        let baseline_cost = (baseline_input as f64 * rate.input_per_million
            + baseline_output as f64 * rate.output_per_million)
            / 1_000_000.0;
        return Some((baseline_cost - actual_cost).max(0.0));
    }
    let actual_input = event.ctx_tokens?;
    Some(baseline_input.saturating_sub(actual_input) as f64 * rate.input_per_million / 1_000_000.0)
}

fn pricing_for_event(event: &MetricEvent, pricing: &[Pricing]) -> Option<Pricing> {
    if !event.model.is_empty() {
        return pricing
            .iter()
            .find(|value| value.model == event.model)
            .cloned()
            .or_else(|| builtin_pricing(&event.model));
    }
    let mut rates = pricing.iter();
    let first = rates.next()?.clone();
    rates.all(|rate| rate.model == first.model).then_some(first)
}

fn builtin_pricing(model: &str) -> Option<Pricing> {
    (model == "gpt-5.6-luna").then(|| Pricing {
        model: model.to_owned(),
        input_per_million: 0.20,
        cached_input_per_million: 0.02,
        output_per_million: 1.20,
        source: "ctx benchmark baseline".to_owned(),
        updated_at: 0,
    })
}

fn empty_report(scope: &str, since_days: u64) -> MetricsReport {
    MetricsReport {
        scope: scope.to_owned(),
        since_days,
        events: 0,
        mcp_calls: 0,
        run_calls: 0,
        hook_events: 0,
        cache_hits: 0,
        tool_calls: 0,
        ctx_tokens: 0,
        input_tokens: 0,
        cached_input_tokens: 0,
        uncached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: 0,
        total_duration_ms: 0,
        estimated_cost_usd: None,
        baseline_input_tokens: None,
        saved_input_tokens: None,
        saved_output_tokens: None,
        estimated_saved_cost_usd: None,
        savings_confidence: "unknown".to_owned(),
        by_project: Vec::new(),
        by_operation: Vec::new(),
        by_harness: Vec::new(),
    }
}

fn extract_usage(value: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: find_number(value, &["input_tokens"]),
        cached_input_tokens: find_number(value, &["cached_input_tokens", "cached_tokens"]),
        output_tokens: find_number(value, &["output_tokens"]),
        reasoning_tokens: find_number(value, &["reasoning_output_tokens", "reasoning_tokens"]),
    }
}

fn find_number(value: &Value, keys: &[&str]) -> Option<u64> {
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(number) = object.get(*key).and_then(Value::as_u64) {
                return Some(number);
            }
        }
        for child in object.values() {
            if let Some(number) = find_number(child, keys) {
                return Some(number);
            }
        }
    }
    value
        .as_array()?
        .iter()
        .find_map(|child| find_number(child, keys))
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(string) = object.get(*key).and_then(Value::as_str) {
                return Some(string.to_owned());
            }
        }
        for child in object.values() {
            if let Some(string) = find_string(child, keys) {
                return Some(string);
            }
        }
    }
    value
        .as_array()?
        .iter()
        .find_map(|child| find_string(child, keys))
}

fn new_event_id() -> String {
    format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}:{}:{}",
                std::process::id(),
                now_nanos(),
                std::thread::current().name().unwrap_or("ctx")
            )
            .as_bytes(),
        )
    )
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn estimate_source_tokens(root: &Path, paths: &[String]) -> u64 {
    let mut seen = std::collections::HashSet::new();
    paths
        .iter()
        .filter(|path| seen.insert(path.as_str()))
        .filter_map(|path| fs::read(root.join(path)).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).chars().count().div_ceil(4) as u64)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> MetricEvent {
        MetricEvent {
            event_id: "event".to_owned(),
            created_at: 1,
            project_id: "project".to_owned(),
            project_name: "demo".to_owned(),
            kind: "ctx_run".to_owned(),
            harness: "codex".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            confidence: "exact".to_owned(),
            query_hash: Some("query".to_owned()),
            input_tokens: Some(1_000),
            cached_input_tokens: Some(400),
            output_tokens: Some(100),
            reasoning_tokens: Some(20),
            ctx_tokens: Some(80),
            baseline_input_tokens: None,
            baseline_output_tokens: None,
            saved_input_tokens: None,
            saved_output_tokens: None,
            duration_ms: Some(10),
            cache_hit: false,
            tool_calls: 1,
            baseline_paths: None,
        }
    }

    #[test]
    fn report_aggregates_usage_and_builtin_pricing() {
        let report = build_report("project", 30, &[event()], &[]).unwrap();
        assert_eq!(report.events, 1);
        assert_eq!(report.input_tokens, 1_000);
        assert_eq!(report.cached_input_tokens, 400);
        assert_eq!(report.uncached_input_tokens, 600);
        assert_eq!(report.ctx_tokens, 80);
        assert_eq!(report.run_calls, 1);
        assert!((report.estimated_cost_usd.unwrap() - 0.000248).abs() < 1e-12);
        assert_eq!(report.savings_confidence, "unknown");
    }

    #[test]
    fn hook_usage_is_extracted_from_nested_payloads() {
        let usage = extract_usage(&json!({
            "turn": {
                "usage": {
                    "input_tokens": 12,
                    "input_tokens_details": {"cached_tokens": 4},
                    "output_tokens": 3,
                    "reasoning_output_tokens": 2
                }
            }
        }));
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.cached_input_tokens, Some(4));
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.reasoning_tokens, Some(2));
    }

    #[test]
    fn report_pairs_a_baseline_with_a_ctx_run() {
        let current = event();
        let mut baseline = event();
        baseline.event_id = "baseline".to_owned();
        baseline.kind = "baseline".to_owned();
        baseline.input_tokens = None;
        baseline.output_tokens = None;
        baseline.ctx_tokens = None;
        baseline.baseline_input_tokens = Some(2_000);
        baseline.baseline_output_tokens = Some(200);
        baseline.tool_calls = 0;
        let report = build_report("project", 30, &[current, baseline], &[]).unwrap();
        assert_eq!(report.baseline_input_tokens, Some(2_000));
        assert_eq!(report.saved_input_tokens, Some(1_000));
        assert_eq!(report.saved_output_tokens, Some(100));
        assert_eq!(report.savings_confidence, "exact");
        assert!(report.estimated_saved_cost_usd.unwrap() > 0.0);
    }

    #[test]
    fn report_estimates_mcp_saved_cost_from_a_single_configured_price() {
        let mut current = event();
        current.kind = "mcp_pack".to_owned();
        current.model.clear();
        current.confidence = "estimated".to_owned();
        current.input_tokens = None;
        current.output_tokens = None;
        current.ctx_tokens = Some(25);
        current.baseline_input_tokens = Some(100);
        current.baseline_output_tokens = None;
        current.saved_input_tokens = Some(75);
        let pricing = vec![Pricing {
            model: "gpt-5.6-luna".to_owned(),
            input_per_million: 0.20,
            cached_input_per_million: 0.02,
            output_per_million: 1.20,
            source: "test".to_owned(),
            updated_at: 0,
        }];
        let report = build_report("project", 30, &[current], &pricing).unwrap();
        assert_eq!(report.savings_confidence, "estimated");
        assert!((report.estimated_saved_cost_usd.unwrap() - 0.000015).abs() < 1e-12);
    }
}
