use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const MAX_FILE_SIZE: u64 = 1_048_576;
pub const EXCERPT_SIZE: usize = 800;

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
