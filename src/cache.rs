use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::ctx_dir;
use crate::model::Hit;

const CACHE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cache_meta(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_cache(
  cache_key TEXT PRIMARY KEY,
  request_json TEXT NOT NULL DEFAULT '{}',
  result_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_hit_at INTEGER NOT NULL,
  hit_count INTEGER NOT NULL DEFAULT 0,
  size_bytes INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS cache_leases(
  cache_key TEXT PRIMARY KEY,
  owner TEXT NOT NULL,
  started_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_cache_lru ON agent_cache(last_hit_at);
CREATE INDEX IF NOT EXISTS idx_agent_cache_created ON agent_cache(created_at);
"#;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub answer: String,
    pub cached: bool,
    pub cache_key: Option<String>,
    pub harness: String,
    pub model: String,
    pub effort: String,
    pub sha: String,
    pub duration_ms: u128,
    #[serde(default)]
    pub cache_lookup_ms: u128,
    #[serde(default)]
    pub harness_ms: u128,
    #[serde(default)]
    pub validation_ms: u128,
    pub usage: TokenUsage,
    pub hits: Vec<Hit>,
    pub coverage: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CacheStore {
    path: PathBuf,
}

impl CacheStore {
    pub fn open(root: &Path) -> Result<Self> {
        let path = ctx_dir(root).join("cache.sqlite");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let store = Self { path };
        store.connection()?;
        Ok(store)
    }

    pub fn lookup(&self, key: &str) -> Result<Option<AgentResult>> {
        let connection = self.connection()?;
        let result = connection
            .query_row(
                "SELECT result_json FROM agent_cache WHERE cache_key=?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(result) = result else {
            return Ok(None);
        };
        connection.execute(
            "UPDATE agent_cache SET last_hit_at=?2,hit_count=hit_count+1 WHERE cache_key=?1",
            params![key, now_seconds()],
        )?;
        match serde_json::from_str(&result) {
            Ok(result) => Ok(Some(result)),
            Err(_) => {
                connection.execute("DELETE FROM agent_cache WHERE cache_key=?1", [key])?;
                Ok(None)
            }
        }
    }

    pub fn acquire_lease(&self, key: &str, owner: &str, stale_after_seconds: u64) -> Result<bool> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let cutoff =
            now_seconds().saturating_sub(i64::try_from(stale_after_seconds).unwrap_or(i64::MAX));
        transaction.execute(
            "DELETE FROM cache_leases WHERE cache_key=?1 AND started_at<?2",
            params![key, cutoff],
        )?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO cache_leases(cache_key,owner,started_at) VALUES(?1,?2,?3)",
            params![key, owner, now_seconds()],
        )?;
        transaction.commit()?;
        Ok(inserted == 1)
    }

    pub fn store_and_release(
        &self,
        key: &str,
        owner: &str,
        request: &Value,
        result: &AgentResult,
    ) -> Result<()> {
        let body = serde_json::to_string(result)?;
        let request = serde_json::to_string(request)?;
        let size = body.len().saturating_add(request.len());
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO agent_cache(cache_key,request_json,result_json,created_at,last_hit_at,hit_count,size_bytes)
             VALUES(?1,?2,?3,?4,?4,0,?5)
             ON CONFLICT(cache_key) DO UPDATE SET
               request_json=excluded.request_json,result_json=excluded.result_json,created_at=excluded.created_at,
               last_hit_at=excluded.last_hit_at,hit_count=0,size_bytes=excluded.size_bytes",
            params![key, request, body, now_seconds(), size as i64],
        )?;
        transaction.execute(
            "DELETE FROM cache_leases WHERE cache_key=?1 AND owner=?2",
            params![key, owner],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn release(&self, key: &str, owner: &str) -> Result<()> {
        self.connection()?.execute(
            "DELETE FROM cache_leases WHERE cache_key=?1 AND owner=?2",
            params![key, owner],
        )?;
        Ok(())
    }

    pub fn clear(&self, kind: &str) -> Result<usize> {
        if !matches!(kind, "agent" | "all") {
            anyhow::bail!("cache kind inconnu: {kind}");
        }
        let connection = self.connection()?;
        let count = connection.execute("DELETE FROM agent_cache", [])?;
        connection.execute("DELETE FROM cache_leases", [])?;
        Ok(count)
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM agent_cache WHERE cache_key=?1", [key])?;
        Ok(())
    }

    pub fn prune(&self, max_age_days: u64, max_size_mb: u64) -> Result<usize> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let cutoff = now_seconds()
            .saturating_sub(i64::try_from(max_age_days.saturating_mul(86_400)).unwrap_or(i64::MAX));
        let mut removed =
            transaction.execute("DELETE FROM agent_cache WHERE created_at<?1", [cutoff])?;
        let limit = max_size_mb.saturating_mul(1_048_576);
        let mut total: u64 = transaction.query_row(
            "SELECT COALESCE(SUM(size_bytes),0) FROM agent_cache",
            [],
            |row| row.get(0),
        )?;
        while total > limit {
            let deleted = transaction.execute(
                "DELETE FROM agent_cache WHERE cache_key=(SELECT cache_key FROM agent_cache ORDER BY last_hit_at,created_at LIMIT 1)",
                [],
            )?;
            if deleted == 0 {
                break;
            }
            removed += deleted;
            total = transaction.query_row(
                "SELECT COALESCE(SUM(size_bytes),0) FROM agent_cache",
                [],
                |row| row.get(0),
            )?;
        }
        transaction.commit()?;
        Ok(removed)
    }

    pub fn status(&self) -> Result<Value> {
        let connection = self.connection()?;
        let (entries, payload_bytes, hits): (u64, u64, u64) = connection.query_row(
            "SELECT count(*),COALESCE(SUM(size_bytes),0),COALESCE(SUM(hit_count),0) FROM agent_cache",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let leases: u64 =
            connection.query_row("SELECT count(*) FROM cache_leases", [], |row| row.get(0))?;
        Ok(json!({
            "database": self.path,
            "entries": entries,
            "payload_bytes": payload_bytes,
            "database_bytes": fs::metadata(&self.path).map(|value| value.len()).unwrap_or(0),
            "hits": hits,
            "leases": leases,
        }))
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;
        connection.execute_batch(CACHE_SCHEMA)?;
        let mut statement = connection.prepare("PRAGMA table_info(agent_cache)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        if !columns.iter().any(|column| column == "request_json") {
            connection.execute(
                "ALTER TABLE agent_cache ADD COLUMN request_json TEXT NOT NULL DEFAULT '{}'",
                [],
            )?;
        }
        connection.execute(
            "INSERT INTO cache_meta(key,value) VALUES('schema_version','1')
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [],
        )?;
        Ok(connection)
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> AgentResult {
        AgentResult {
            answer: "defined at auth.py:5-7".to_owned(),
            cached: false,
            cache_key: Some("key".to_owned()),
            harness: "codex".to_owned(),
            model: "test".to_owned(),
            effort: "high".to_owned(),
            sha: "abc".to_owned(),
            duration_ms: 1,
            cache_lookup_ms: 0,
            harness_ms: 1,
            validation_ms: 0,
            usage: TokenUsage::default(),
            hits: Vec::new(),
            coverage: "complete".to_owned(),
            hint: None,
        }
    }

    #[test]
    fn stores_leases_hits_and_prunes() {
        let temporary = tempfile::tempdir().unwrap();
        let store = CacheStore::open(temporary.path()).unwrap();
        assert!(store.acquire_lease("key", "one", 30).unwrap());
        assert!(!store.acquire_lease("key", "two", 30).unwrap());
        store
            .store_and_release("key", "one", &json!({"question":"test"}), &result())
            .unwrap();
        assert!(store.lookup("key").unwrap().is_some());
        assert_eq!(store.status().unwrap()["hits"], 1);
        assert_eq!(store.prune(30, 0).unwrap(), 1);
        assert!(store.lookup("key").unwrap().is_none());
    }
}
