//! PreToolUse hook that makes a few shell commands cheaper to read without changing what they do.
//!
//! A search (`rg`, `grep`, `git grep`, optionally piped to head/tail/sort/uniq) runs unchanged and
//! its output is piped to `ctx compact`, which groups matches by file and bounds the output. A
//! `cat` of a long file becomes `ctx read`. Anything else runs untouched; the hook never blocks.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const CAT_MAX_UNREWRITTEN_LINES: usize = 200;
/// Token budget for the definition bodies appended to a compacted search.
const EXPAND_TOKENS: usize = 1_500;
/// Stages a search may be piped through before compaction: they only filter or reorder lines.
const LINE_FILTERS: &[&str] = &["head", "tail", "sort", "uniq"];

/// Rewrites `command` into a cheaper equivalent, or returns `None` to leave it unchanged.
pub fn rewrite_command(command: &str, cwd: &Path) -> Option<String> {
    let (prefix, directory, rest) = split_cd_prefix(command)?;
    let effective = match &directory {
        Some(directory) => cwd.join(directory),
        None => cwd.to_path_buf(),
    };
    // A `cd` into a directory that does not exist fails in the shell: leave it alone.
    if directory.is_some() && !effective.is_dir() {
        return None;
    }
    // A list `a; b && c`: each search in it is compacted, everything else runs as is.
    let segments = split_list(rest)?;
    if segments.len() > 1 {
        let mut changed = false;
        let mut out = String::new();
        for (segment, separator) in &segments {
            match rewrite_search_pipeline(segment) {
                Some(rewritten) => {
                    changed = true;
                    out.push_str(&rewritten);
                }
                None => {
                    // Every segment must still be a form this hook understands.
                    split_pipeline(segment)?
                        .into_iter()
                        .map(tokenize)
                        .collect::<Option<Vec<_>>>()?;
                    out.push_str(segment.trim_end());
                }
            }
            out.push_str(separator);
        }
        return changed.then(|| format!("{prefix}{out}"));
    }
    let stages = split_pipeline(rest)?
        .into_iter()
        .map(tokenize)
        .collect::<Option<Vec<_>>>()?;
    let (first, _) = stages.split_first()?;
    let (program, arguments) = first.split_first()?;
    if stages.len() == 1 && program == "cat" {
        return Some(format!("{prefix}{}", rewrite_cat(arguments, &effective)?));
    }
    Some(format!("{prefix}{}", rewrite_search_pipeline(rest)?))
}

/// `search [| filter]...` followed by ` | ctx compact`, or `None`.
fn rewrite_search_pipeline(command: &str) -> Option<String> {
    let stages = split_pipeline(command)?
        .into_iter()
        .map(tokenize)
        .collect::<Option<Vec<_>>>()?;
    let (first, filters) = stages.split_first()?;
    let (program, arguments) = first.split_first()?;
    if !is_search(program, arguments) || !filters.iter().all(|stage| is_line_filter(stage)) {
        return None;
    }
    Some(format!("{} | ctx compact", command.trim()))
}

/// Splits a command list on unquoted `;` and `&&`, keeping each separator (with
/// its spacing) after its segment; `None` on `||`, a lone `&` or an open quote.
fn split_list(command: &str) -> Option<Vec<(String, String)>> {
    let bytes = command.as_bytes();
    let (mut segments, mut start, mut index) = (Vec::new(), 0, 0);
    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match (quote, byte) {
            (Some(open), _) if byte == open => quote = None,
            (Some(b'"'), b'\\') => index += 1,
            (Some(_), _) => {}
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'\\') => index += 1,
            (None, b';') | (None, b'&') => {
                let width = if byte == b'&' {
                    if bytes.get(index + 1) != Some(&b'&') {
                        return None;
                    }
                    2
                } else {
                    1
                };
                let separator_end = index + width;
                let next = command[separator_end..]
                    .find(|c: char| c != ' ')
                    .map_or(command.len(), |offset| separator_end + offset);
                let segment = command[start..index].trim();
                if segment.is_empty() {
                    return None;
                }
                let spaced = format!(
                    "{}{}",
                    if command[..index].ends_with(' ') {
                        " "
                    } else {
                        ""
                    },
                    &command[index..next]
                );
                segments.push((segment.to_owned(), spaced));
                start = next;
                index = next;
                continue;
            }
            (None, b'|') if bytes.get(index + 1) == Some(&b'|') => return None,
            _ => {}
        }
        index += 1;
    }
    if quote.is_some() {
        return None;
    }
    let last = command[start..].trim();
    if !last.is_empty() {
        segments.push((last.to_owned(), String::new()));
    }
    Some(segments)
}

/// A search whose output is `path:line:text` lines, with no option that runs another program.
fn is_search(program: &str, arguments: &[String]) -> bool {
    let (arguments, runs_programs): (&[String], &[&str]) = match program {
        "rg" => (arguments, &["--pre", "--pre-glob", "-z", "--search-zip"]),
        "grep" => (arguments, &[]),
        "git" if arguments.first().is_some_and(|sub| sub == "grep") => {
            (&arguments[1..], &["-O", "--open-files-in-pager"])
        }
        _ => return false,
    };
    let short = match program {
        "rg" => Some('z'),
        "git" => Some('O'),
        _ => None,
    };
    !arguments.iter().any(|argument| {
        runs_programs
            .iter()
            .any(|flag| argument == flag || argument.starts_with(&format!("{flag}=")))
            || short.is_some_and(|flag| {
                argument.starts_with('-') && !argument.starts_with("--") && argument.contains(flag)
            })
    })
}

/// `head`, `tail`, `sort` or `uniq` with options only: no file argument, so they read the pipe.
fn is_line_filter(stage: &[String]) -> bool {
    stage.split_first().is_some_and(|(program, arguments)| {
        LINE_FILTERS.contains(&program.as_str())
            && arguments
                .iter()
                .all(|argument| argument.starts_with('-') || argument.parse::<u64>().is_ok())
    })
}

/// Splits on unquoted `|` (not `||`); `None` when a quote is left open.
fn split_pipeline(command: &str) -> Option<Vec<&str>> {
    let bytes = command.as_bytes();
    let (mut stages, mut start, mut index) = (Vec::new(), 0, 0);
    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match (quote, byte) {
            (Some(open), _) if byte == open => quote = None,
            (Some(b'"'), b'\\') => index += 1,
            (Some(_), _) => {}
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'\\') => index += 1,
            (None, b'|') => {
                if bytes.get(index + 1) == Some(&b'|') {
                    return None;
                }
                stages.push(&command[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    if quote.is_some() {
        return None;
    }
    stages.push(&command[start..]);
    Some(stages)
}

/// Builds the hook response for a PreToolUse payload, or `None` when nothing is rewritten.
pub fn hook_response(payload: &Value, harness: &str) -> Option<Value> {
    if !matches!(harness, "claude" | "codex") {
        return None;
    }
    if !matches!(
        payload.get("tool_name")?.as_str()?,
        "Bash" | "shell" | "exec_command"
    ) {
        return None;
    }
    let tool_input = payload.get("tool_input")?.as_object()?;
    let command = tool_input.get("command")?.as_str()?;
    let cwd = match payload.get("cwd").and_then(Value::as_str) {
        Some(cwd) if !cwd.is_empty() => PathBuf::from(cwd),
        _ => std::env::current_dir().ok()?,
    };
    let rewritten = rewrite_command(command, &cwd)?;
    let mut updated = tool_input.clone();
    updated.insert("command".to_owned(), Value::String(rewritten));
    let mut output = json!({
        "hookEventName": "PreToolUse",
        "permissionDecisionReason": "ctx: bounded output",
        "updatedInput": updated,
    });
    if harness == "codex" {
        output["permissionDecision"] = json!("allow");
    }
    Some(json!({ "hookSpecificOutput": output }))
}

fn rewrite_cat(arguments: &[String], cwd: &Path) -> Option<String> {
    let [file] = arguments else {
        return None;
    };
    if file.starts_with('-') {
        return None;
    }
    let path = cwd.join(file);
    if !fs::metadata(&path).ok()?.is_file() || !has_index(&path) {
        return None;
    }
    if count_lines_up_to(&path, CAT_MAX_UNREWRITTEN_LINES + 1)? <= CAT_MAX_UNREWRITTEN_LINES {
        return None;
    }
    Some(format!("ctx read {}", quote(file)))
}

fn count_lines_up_to(path: &Path, limit: usize) -> Option<usize> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let mut buffer = Vec::new();
    let mut lines = 0;
    while lines < limit {
        buffer.clear();
        if reader.read_until(b'\n', &mut buffer).ok()? == 0 {
            break;
        }
        lines += 1;
    }
    Some(lines)
}

/// True when an index serves `target` (a directory, or a file whose parent directory counts).
fn has_index(target: &Path) -> bool {
    let directory = if target.is_dir() {
        Some(target)
    } else {
        target.parent()
    };
    directory.is_some_and(|directory| {
        crate::config::find_ctx(directory)
            .map(|ctx| ctx.join("index.sqlite").is_file())
            .unwrap_or(false)
    })
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

const BLANKS: [char; 2] = [' ', '\t'];

/// Splits an optional `cd <dir> && ` prefix (kept verbatim) from the command proper.
fn split_cd_prefix(command: &str) -> Option<(&str, Option<String>, &str)> {
    let Some(after_cd) = command.trim_start().strip_prefix("cd") else {
        return Some(("", None, command));
    };
    if !after_cd.starts_with(BLANKS) {
        return Some(("", None, command));
    }
    let (directory, end) = scan_word(command, command.len() - after_cd.len())?;
    let after_operator = command[end..]
        .trim_start_matches(BLANKS)
        .strip_prefix("&&")?;
    if !after_operator.starts_with(BLANKS) {
        return None;
    }
    let rest = after_operator.trim_start_matches(BLANKS);
    let split = command.len() - rest.len();
    Some((&command[..split], Some(directory), rest))
}

fn tokenize(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut position = 0;
    loop {
        let rest = &command[position..];
        if rest.trim_start_matches(BLANKS).is_empty() {
            return Some(tokens);
        }
        let (word, end) = scan_word(command, position)?;
        // Every word must consume input; anything else is a form this hook does not model.
        if end <= position {
            return None;
        }
        tokens.push(word);
        position = end;
    }
}

/// Reads one shell word at or after `start`, returning it with the index just past it.
/// Returns `None` when the word contains anything this hook does not model: command
/// operators, expansions, globs, or newlines.
fn scan_word(text: &str, start: usize) -> Option<(String, usize)> {
    let mut chars = text[start..]
        .char_indices()
        .map(|(index, character)| (start + index, character))
        .peekable();
    while chars
        .next_if(|(_, character)| BLANKS.contains(character))
        .is_some()
    {}
    let mut word = String::new();
    let mut at_start = true;
    while let Some(&(index, character)) = chars.peek() {
        if BLANKS.contains(&character) {
            return Some((word, index));
        }
        // Any other whitespace (CR, Unicode spaces) is not modelled: decline.
        if character.is_whitespace() && character != '\n' {
            return None;
        }
        chars.next();
        match character {
            '\'' => loop {
                match chars.next()?.1 {
                    '\'' => break,
                    '\n' => return None,
                    other => word.push(other),
                }
            },
            '"' => loop {
                match chars.next()?.1 {
                    '"' => break,
                    '\n' | '$' | '`' => return None,
                    '\\' => match chars.next()?.1 {
                        escaped @ ('"' | '\\') => word.push(escaped),
                        '\n' => return None,
                        other => {
                            word.push('\\');
                            word.push(other);
                        }
                    },
                    other => word.push(other),
                }
            },
            '\\' => match chars.next()?.1 {
                '\n' => return None,
                escaped => word.push(escaped),
            },
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '\n' | '*' | '?' | '[' | '{'
            | '!' => return None,
            '~' | '#' if at_start => return None,
            other => word.push(other),
        }
        at_start = false;
    }
    Some((word, text.len()))
}

/// PostToolUse response that compacts a Grep `content` result (Claude), keeping the
/// tool's output shape; `None` when there is nothing to gain.
pub fn post_tool_response(payload: &Value, harness: &str) -> Option<Value> {
    if harness != "claude" || payload.get("tool_name")?.as_str()? != "Grep" {
        return None;
    }
    let response = payload.get("tool_response")?.as_object()?;
    if response.get("mode")?.as_str()? != "content" {
        return None;
    }
    let content = response.get("content")?.as_str()?;
    let root = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let compacted = crate::compact::compact(
        content,
        crate::compact::CompactOptions {
            expand_tokens: EXPAND_TOKENS,
            root,
            ..crate::compact::CompactOptions::default()
        },
    );
    let compacted = compacted.trim_end_matches('\n');
    if compacted.len() >= content.len() {
        return None;
    }
    let mut updated = response.clone();
    updated.insert("content".to_owned(), Value::String(compacted.to_owned()));
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": updated,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(count: usize) -> String {
        (0..count).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn searches_run_unchanged_and_pipe_into_compact() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        for command in [
            "rg -n foo",
            "rg -n \"a|b\" src -g '*.ts'",
            "rg -n foo src | head -160",
            "grep -rn foo .",
            "git grep -n foo -- src",
            "rg -n foo | sort | uniq -c",
        ] {
            assert_eq!(
                rewrite_command(command, cwd).as_deref(),
                Some(format!("{command} | ctx compact").as_str()),
                "{command}"
            );
        }
        fs::create_dir_all(cwd.join("src")).unwrap();
        assert_eq!(
            rewrite_command("cd src && rg -n foo", cwd).unwrap(),
            "cd src && rg -n foo | ctx compact"
        );
    }

    #[test]
    fn leaves_everything_else_alone() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        for command in [
            "rg --pre cat foo",
            "rg --pre=cat foo",
            "rg -z foo",
            "rg -nz foo",
            "git grep -O foo",
            "git log",
            "rg foo > out.txt",
            "rg foo | head -5 out.txt",
            "rg foo | xargs rm",
            "rg foo || true",
            "rg foo && rm -rf x | tee y > z",
            "rg $(echo foo)",
            "rg 'unterminated",
            "rg foo | ctx compact",
            "sed -n '1,40p' src/a.rs",
            "ls | head",
            "",
            "rg foo\r",
        ] {
            assert!(rewrite_command(command, cwd).is_none(), "{command:?}");
        }
        assert!(rewrite_command("cd missing && rg foo", cwd).is_none());
    }

    #[test]
    fn each_search_in_a_command_list_is_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        assert_eq!(
            rewrite_command("sed -n '1,20p' a.rs; rg -n foo src", cwd).unwrap(),
            "sed -n '1,20p' a.rs; rg -n foo src | ctx compact"
        );
        assert_eq!(
            rewrite_command("rg -n a && rg -n b | head -5", cwd).unwrap(),
            "rg -n a | ctx compact && rg -n b | head -5 | ctx compact"
        );
        assert!(rewrite_command("sed -n '1,2p' a; ls", cwd).is_none());
        assert!(rewrite_command("rg a; rm x > y", cwd).is_none());
        assert!(rewrite_command("rg a & rg b", cwd).is_none());
        assert!(rewrite_command("rg a || rg b", cwd).is_none());
    }

    #[test]
    fn cat_of_a_long_indexed_file_becomes_ctx_read() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        fs::create_dir_all(cwd.join(".ctx")).unwrap();
        fs::write(cwd.join(".ctx/index.sqlite"), b"").unwrap();
        fs::write(cwd.join("long.rs"), lines(201)).unwrap();
        fs::write(cwd.join("short.rs"), lines(200)).unwrap();
        assert_eq!(
            rewrite_command("cat long.rs", cwd).unwrap(),
            "ctx read 'long.rs'"
        );
        assert!(rewrite_command("cat short.rs", cwd).is_none());
        assert!(rewrite_command("cat long.rs | head", cwd).is_none());
    }

    #[test]
    fn grep_content_is_compacted_in_the_same_shape() {
        let payload = json!({
            "tool_name": "Grep",
            "tool_response": {
                "mode": "content", "numFiles": 0, "filenames": [],
                "content": "src/a.ts:1:alpha\nsrc/a.ts:9:beta\nsrc/b.ts:3:gamma", "numLines": 3
            }
        });
        let out = post_tool_response(&payload, "claude").unwrap();
        let updated = &out["hookSpecificOutput"]["updatedToolOutput"];
        assert_eq!(
            updated["content"],
            "src/a.ts\n1:alpha\n9:beta\nsrc/b.ts\n3:gamma"
        );
        assert_eq!(updated["mode"], "content");
        assert_eq!(updated["numLines"], 3);
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert!(post_tool_response(&payload, "codex").is_none());
        let files = json!({"tool_name": "Grep", "tool_response": {"mode": "files_with_matches", "filenames": ["a"]}});
        assert!(post_tool_response(&files, "claude").is_none());
        let single =
            json!({"tool_name": "Grep", "tool_response": {"mode": "content", "content": "x"}});
        assert!(post_tool_response(&single, "claude").is_none());
    }

    #[test]
    fn response_shape_differs_between_harnesses_and_keeps_tool_input() {
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": { "command": "rg -n foo", "description": "search" },
            "cwd": std::env::temp_dir(),
        });
        let claude = hook_response(&payload, "claude").unwrap();
        assert_eq!(
            claude["hookSpecificOutput"]["updatedInput"]["command"],
            "rg -n foo | ctx compact"
        );
        assert_eq!(
            claude["hookSpecificOutput"]["updatedInput"]["description"],
            "search"
        );
        assert!(
            claude["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );
        let codex = hook_response(&payload, "codex").unwrap();
        assert_eq!(codex["hookSpecificOutput"]["permissionDecision"], "allow");
        assert!(hook_response(&payload, "grok").is_none());
        assert!(
            hook_response(
                &json!({"tool_name": "Read", "tool_input": {"command": "rg foo"}}),
                "claude"
            )
            .is_none()
        );
    }
}
