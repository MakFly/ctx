use std::path::Path;
use std::process::Command;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct GitInfo {
    pub sha: String,
    pub dirty: bool,
}

pub fn git_info(root: &Path) -> GitInfo {
    let sha = git_output(root, &["rev-parse", "--short", "HEAD"])
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "nogit".to_owned());
    let dirty = git_output(root, &["status", "--porcelain"])
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    GitInfo { sha, dirty }
}

fn git_output(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
