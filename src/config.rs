use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use toml_edit::DocumentMut;

pub const MAX_FILE_SIZE: u64 = 1_048_576;
pub const DEFAULT_TEXT_MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;
pub const DEFAULT_INDEX_BUFFER_BYTES: usize = 64 * 1024 * 1024;
pub const EXCERPT_SIZE: usize = 800;

#[derive(Debug, Clone)]
pub struct CacheSettings {
    pub enabled: bool,
    pub max_size_mb: u64,
    pub max_age_days: u64,
}

#[derive(Debug, Clone)]
pub struct EmbeddingSettings {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub api_key_env: String,
    pub allow_remote_code: bool,
}

#[derive(Debug, Clone)]
pub struct RunnerSettings {
    pub model: Option<String>,
    pub effort: String,
}

#[derive(Debug, Clone)]
pub struct CtxConfig {
    pub default_harness: Option<String>,
    pub cache: CacheSettings,
    pub embeddings: EmbeddingSettings,
}

impl Default for CtxConfig {
    fn default() -> Self {
        Self {
            default_harness: None,
            cache: CacheSettings {
                enabled: true,
                max_size_mb: 256,
                max_age_days: 30,
            },
            embeddings: EmbeddingSettings {
                enabled: false,
                provider: "local".to_owned(),
                model: String::new(),
                endpoint: String::new(),
                api_key_env: String::new(),
                allow_remote_code: false,
            },
        }
    }
}

pub fn default_config_text() -> &'static str {
    "# Set this before using `ctx run --harness auto`.\n# default_harness = \"codex\"\n\n[cache]\nenabled = true\nmax_size_mb = 256\nmax_age_days = 30\n\n[embeddings]\nenabled = false\nprovider = \"local\"\nmodel = \"\"\nendpoint = \"\"\napi_key_env = \"\"\nallow_remote_code = false\n"
}

pub fn load_config(root: &Path) -> Result<CtxConfig> {
    let path = ctx_dir(root).join("config.toml");
    if !path.is_file() {
        return Ok(CtxConfig::default());
    }
    let text = fs::read_to_string(&path)?;
    let document = DocumentMut::from_str(&text)
        .with_context(|| format!("configuration TOML invalide: {}", path.display()))?;
    let mut config = CtxConfig {
        default_harness: document
            .get("default_harness")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        ..CtxConfig::default()
    };
    if let Some(cache) = document.get("cache").and_then(|value| value.as_table()) {
        config.cache.enabled = cache
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(config.cache.enabled);
        config.cache.max_size_mb = cache
            .get("max_size_mb")
            .and_then(|value| value.as_integer())
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(config.cache.max_size_mb);
        config.cache.max_age_days = cache
            .get("max_age_days")
            .and_then(|value| value.as_integer())
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(config.cache.max_age_days);
    }
    if let Some(embeddings) = document
        .get("embeddings")
        .and_then(|value| value.as_table())
    {
        config.embeddings.enabled = embeddings
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        config.embeddings.provider = string_value(embeddings, "provider", "local");
        config.embeddings.model = string_value(embeddings, "model", "");
        config.embeddings.endpoint = string_value(embeddings, "endpoint", "");
        config.embeddings.api_key_env = string_value(embeddings, "api_key_env", "");
        config.embeddings.allow_remote_code = embeddings
            .get("allow_remote_code")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
    }
    Ok(config)
}

pub fn runner_settings(root: &Path, harness: &str) -> Result<RunnerSettings> {
    let path = ctx_dir(root).join("config.toml");
    let mut settings = RunnerSettings {
        model: None,
        effort: "high".to_owned(),
    };
    if !path.is_file() {
        return Ok(settings);
    }
    let document = DocumentMut::from_str(&fs::read_to_string(&path)?)?;
    let Some(table) = document
        .get("runners")
        .and_then(|value| value.as_table())
        .and_then(|runners| runners.get(harness))
        .and_then(|value| value.as_table())
    else {
        return Ok(settings);
    };
    settings.model = table
        .get("model")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    settings.effort = string_value(table, "effort", "high");
    Ok(settings)
}

fn string_value(table: &toml_edit::Table, key: &str, default: &str) -> String {
    table
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or(default)
        .to_owned()
}

pub fn language(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "py" | "pyi" | "pyw" => Some("python"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" | "vue" | "svelte" | "astro" => Some("typescript"),
        "tsx" => Some("tsx"),
        "go" => Some("go"),
        "rs" => Some("rust"),
        "php" | "phtml" => Some("php"),
        _ => None,
    }
}

pub fn is_text(path: &Path) -> bool {
    language(path).is_some()
        || matches!(
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "md" | "txt"
                | "toml"
                | "json"
                | "yaml"
                | "yml"
                | "ini"
                | "cfg"
                | "html"
                | "css"
                | "scss"
                | "sql"
                | "sh"
        )
}

pub fn repo_root(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    let resolved = path
        .canonicalize()
        .with_context(|| format!("chemin introuvable: {}", path.display()))?;
    if !resolved.is_dir() {
        bail!("répertoire introuvable: {}", resolved.display());
    }
    Ok(resolved)
}

pub fn ctx_dir(root: &Path) -> PathBuf {
    match env::var_os("CTX_DIR") {
        Some(value) => {
            let value = PathBuf::from(value);
            if value.is_absolute() {
                value
            } else {
                root.join(value)
            }
        }
        None => root.join(".ctx"),
    }
}

pub fn find_ctx(start: impl AsRef<Path>) -> Result<PathBuf> {
    if let Some(value) = env::var_os("CTX_DIR") {
        let value = PathBuf::from(value);
        return Ok(if value.is_absolute() {
            value
        } else {
            start.as_ref().canonicalize()?.join(value)
        });
    }
    let start = start
        .as_ref()
        .canonicalize()
        .with_context(|| format!("chemin introuvable: {}", start.as_ref().display()))?;
    for candidate in start.ancestors() {
        let directory = candidate.join(".ctx");
        if directory.join("index.sqlite").is_file() {
            return Ok(directory);
        }
    }
    Ok(start.join(".ctx"))
}

pub fn relative_path(path: &Path, root: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| format!("{} est hors du repo", path.display()))?
        .to_string_lossy()
        .replace('\\', "/"))
}

/// Shared admission and memory settings for indexing, exact search and watching.
#[derive(Debug, Clone)]
pub struct IndexSettings {
    pub max_file_bytes: u64,
    pub buffer_bytes: usize,
}

pub fn index_settings(root: &Path) -> Result<IndexSettings> {
    let path = ctx_dir(root).join("config.toml");
    let mut settings = IndexSettings {
        max_file_bytes: DEFAULT_TEXT_MAX_FILE_SIZE,
        buffer_bytes: DEFAULT_INDEX_BUFFER_BYTES,
    };
    if path.is_file() {
        let document = fs::read_to_string(path)?.parse::<DocumentMut>()?;
        if let Some(table) = document.get("index").and_then(|item| item.as_table()) {
            for (key, destination) in [("max_file_mb", &mut settings.max_file_bytes)] {
                if let Some(value) = table.get(key) {
                    let mb = value
                        .as_integer()
                        .filter(|mb| *mb > 0)
                        .with_context(|| format!("index.{key} must be a positive integer"))?;
                    *destination = (mb as u64)
                        .checked_mul(1024 * 1024)
                        .context("index size overflow")?;
                }
            }
            if let Some(value) = table.get("buffer_mb") {
                let mb = value
                    .as_integer()
                    .filter(|mb| *mb > 0)
                    .context("index.buffer_mb must be a positive integer")?;
                settings.buffer_bytes = usize::try_from(mb)?
                    .checked_mul(1024 * 1024)
                    .context("index buffer size overflow")?;
            }
        }
    }
    Ok(settings)
}
