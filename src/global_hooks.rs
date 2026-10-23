//! Global search hooks: `ctx hook install` and `ctx hook uninstall` add or remove
//! exactly ctx's own search hook entries in each harness's user configuration under
//! HOME, keeping every other key, entry and hook as it was.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::harness::{load_json, write_text};

/// Harnesses that receive the search hooks, in the order `all` handles them.
pub const HOOK_HARNESSES: [&str; 3] = ["claude", "codex", "grok"];

/// One hook entry: the event, the tool matcher and the command it runs.
struct Hook {
    event: &'static str,
    matcher: &'static str,
    command: &'static str,
}

/// What `install` or `uninstall` did to one configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added,
    AlreadyPresent,
    Removed,
    Absent,
    /// `all` skips a harness whose configuration directory does not exist.
    Skipped,
}

impl Change {
    pub fn label(&self) -> &'static str {
        match self {
            Change::Added => "ajouté",
            Change::AlreadyPresent => "déjà présent",
            Change::Removed => "retiré",
            Change::Absent => "absent",
            Change::Skipped => "ignoré (harness non installé)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub harness: &'static str,
    pub path: PathBuf,
    pub change: Change,
}

/// The configuration file (relative to HOME), the directory whose presence marks the
/// harness as installed, whether ctx owns the whole file, and the hooks.
fn plan(harness: &str) -> Option<(&'static str, &'static str, bool, Vec<Hook>)> {
    match harness {
        "claude" => Some((
            ".claude/settings.json",
            ".claude",
            false,
            vec![
                Hook {
                    event: "PreToolUse",
                    matcher: "Bash",
                    command: "ctx hook rewrite --harness claude",
                },
                Hook {
                    event: "PostToolUse",
                    matcher: "Grep",
                    command: "ctx hook compact-output --harness claude",
                },
            ],
        )),
        "codex" => Some((
            ".codex/hooks.json",
            ".codex",
            false,
            vec![Hook {
                event: "PreToolUse",
                matcher: "Bash",
                command: "ctx hook rewrite --harness codex",
            }],
        )),
        // Grok also reads ~/.claude/settings.json, where the Claude hooks decline its
        // payloads (camelCase `toolName`): its own file carries the Grok variant.
        "grok" => Some((
            ".grok/hooks/ctx.json",
            ".grok",
            true,
            vec![Hook {
                event: "PreToolUse",
                matcher: "run_terminal_command",
                command: "ctx hook rewrite --harness grok",
            }],
        )),
        _ => None,
    }
}

fn selected(harness: &str) -> Result<Vec<&'static str>> {
    if harness == "all" {
        return Ok(HOOK_HARNESSES.to_vec());
    }
    match HOOK_HARNESSES.iter().find(|name| **name == harness) {
        Some(name) => Ok(vec![*name]),
        None => bail!("harness inconnu: {harness} (claude, codex, grok ou all)"),
    }
}

/// A hook is ctx's when its command is ours, alone or followed by options
/// (`--expand-tokens N`).
fn is_ours(hook: &Value, command: &str) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|value| {
            value == command
                || value
                    .strip_prefix(command)
                    .is_some_and(|rest| rest.starts_with(' '))
        })
}

fn hooks_object<'a>(
    config: &'a mut Map<String, Value>,
    path: &Path,
) -> Result<&'a mut Map<String, Value>> {
    config
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .with_context(|| format!("hooks doit être un objet: {}", path.display()))
}

/// Adds the hooks missing from `config`; returns whether anything was added.
fn add(config: &mut Map<String, Value>, hooks: &[Hook], path: &Path) -> Result<bool> {
    let object = hooks_object(config, path)?;
    let mut added = false;
    for hook in hooks {
        let entries = object
            .entry(hook.event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .with_context(|| {
                format!(
                    "hooks.{} doit être un tableau: {}",
                    hook.event,
                    path.display()
                )
            })?;
        let present = entries.iter().any(|entry| {
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|nested| nested.iter().any(|value| is_ours(value, hook.command)))
        });
        if !present {
            entries.push(json!({
                "matcher": hook.matcher,
                "hooks": [{"type": "command", "command": hook.command}]
            }));
            added = true;
        }
    }
    Ok(added)
}

/// Removes ctx's hooks from `config`, then any entry, event or `hooks` object left
/// empty by that removal; returns whether anything was removed.
fn remove(config: &mut Map<String, Value>, hooks: &[Hook]) -> bool {
    let Some(object) = config.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut removed = false;
    for hook in hooks {
        let Some(entries) = object.get_mut(hook.event).and_then(Value::as_array_mut) else {
            continue;
        };
        entries.retain_mut(|entry| {
            let Some(nested) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let before = nested.len();
            nested.retain(|value| !is_ours(value, hook.command));
            if nested.len() == before {
                return true;
            }
            removed = true;
            !nested.is_empty()
        });
        if entries.is_empty() {
            object.remove(hook.event);
        }
    }
    if removed && object.is_empty() {
        config.remove("hooks");
    }
    removed
}

/// Installs (or, with `uninstall`, removes) the search hooks for `harness` (`claude`,
/// `codex`, `grok` or `all`) under `home`. `all` skips a harness that is not installed.
pub fn apply(home: &Path, harness: &str, uninstall: bool) -> Result<Vec<Outcome>> {
    let mut outcomes = Vec::new();
    let all = harness == "all";
    for name in selected(harness)? {
        let (relative, marker, owned, hooks) = plan(name).expect("known harness");
        let path = home.join(relative);
        if all && !home.join(marker).is_dir() {
            outcomes.push(Outcome {
                harness: name,
                path,
                change: Change::Skipped,
            });
            continue;
        }
        let mut config = load_json(&path)?;
        let change = if uninstall {
            if remove(&mut config, &hooks) {
                if owned && config.is_empty() {
                    fs::remove_file(&path)
                        .with_context(|| format!("impossible de supprimer: {}", path.display()))?;
                } else {
                    write_config(home, &path, &config)?;
                }
                Change::Removed
            } else {
                Change::Absent
            }
        } else if add(&mut config, &hooks, &path)? {
            write_config(home, &path, &config)?;
            Change::Added
        } else {
            Change::AlreadyPresent
        };
        outcomes.push(Outcome {
            harness: name,
            path,
            change,
        });
    }
    Ok(outcomes)
}

fn write_config(home: &Path, path: &Path, config: &Map<String, Value>) -> Result<()> {
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(config.clone()))?
    );
    write_text(home, path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn install_merges_and_uninstall_restores_existing_hooks() {
        let home = tempfile::tempdir().unwrap();
        let settings = home.path().join(".claude/settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let original = json!({
            "model": "x",
            "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "other"}]}]}
        });
        fs::write(&settings, serde_json::to_string_pretty(&original).unwrap()).unwrap();

        let first = apply(home.path(), "claude", false).unwrap();
        assert_eq!(first[0].change, Change::Added);
        let installed = read(&settings);
        assert_eq!(
            installed["hooks"]["PreToolUse"].as_array().unwrap().len(),
            2
        );
        assert_eq!(installed["hooks"]["PostToolUse"][0]["matcher"], "Grep");
        assert_eq!(
            apply(home.path(), "claude", false).unwrap()[0].change,
            Change::AlreadyPresent
        );
        assert_eq!(read(&settings), installed);

        assert_eq!(
            apply(home.path(), "claude", true).unwrap()[0].change,
            Change::Removed
        );
        assert_eq!(read(&settings), original);
        assert_eq!(
            apply(home.path(), "claude", true).unwrap()[0].change,
            Change::Absent
        );
    }

    #[test]
    fn install_keeps_key_order() {
        let home = tempfile::tempdir().unwrap();
        let settings = home.path().join(".claude/settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, "{\"zeta\": 1, \"alpha\": 2}").unwrap();
        apply(home.path(), "claude", false).unwrap();
        let text = fs::read_to_string(&settings).unwrap();
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap());
    }

    #[test]
    fn uninstall_drops_option_variants_and_keeps_a_shared_entry() {
        let home = tempfile::tempdir().unwrap();
        let hooks = home.path().join(".codex/hooks.json");
        fs::create_dir_all(hooks.parent().unwrap()).unwrap();
        fs::write(
            &hooks,
            json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "ctx hook rewrite --harness codex --expand-tokens 0"},
                {"type": "command", "command": "keep"}
            ]}]}})
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            apply(home.path(), "codex", true).unwrap()[0].change,
            Change::Removed
        );
        let left = read(&hooks);
        let nested = left["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap();
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0]["command"], "keep");
    }

    #[test]
    fn all_skips_missing_harnesses_and_grok_file_goes_with_its_hooks() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".grok")).unwrap();
        let outcomes = apply(home.path(), "all", false).unwrap();
        let changes: Vec<_> = outcomes
            .iter()
            .map(|o| (o.harness, o.change.clone()))
            .collect();
        assert_eq!(
            changes,
            [
                ("claude", Change::Skipped),
                ("codex", Change::Skipped),
                ("grok", Change::Added)
            ]
        );
        let grok = home.path().join(".grok/hooks/ctx.json");
        assert_eq!(
            read(&grok)["hooks"]["PreToolUse"][0]["matcher"],
            "run_terminal_command"
        );
        apply(home.path(), "all", true).unwrap();
        assert!(!grok.exists());
        assert!(!home.path().join(".claude").exists());
    }

    #[test]
    fn unknown_harness_and_invalid_json_fail_without_writing() {
        let home = tempfile::tempdir().unwrap();
        assert!(apply(home.path(), "cursor", false).is_err());
        let settings = home.path().join(".claude/settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, "{oops").unwrap();
        assert!(apply(home.path(), "claude", false).is_err());
        assert_eq!(fs::read_to_string(&settings).unwrap(), "{oops");
    }
}
