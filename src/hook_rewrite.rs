//! PreToolUse hook that rewrites a few shell commands into bounded ctx equivalents.
//!
//! The rewrite is deliberately conservative: it only fires when the ctx output is exact and
//! bounded, and otherwise leaves the command untouched. It never blocks a command.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const CAT_MAX_UNREWRITTEN_LINES: usize = 200;
const SEARCH_LIMIT: usize = 200;
const SEARCH_BUDGET_TOKENS: usize = 4_000;
const GREP_REGEX_METACHARACTERS: &[char] = &[
    '.', '[', ']', '(', ')', '{', '}', '*', '+', '?', '^', '$', '|', '\\',
];

/// Rewrites `command` into a bounded ctx command, or returns `None` to leave it unchanged.
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
    let tokens = tokenize(rest)?;
    let (program, arguments) = tokens.split_first()?;
    let rewritten = match program.as_str() {
        "rg" => rewrite_search(arguments, &effective, SearchTool::Rg)?,
        "grep" => rewrite_search(arguments, &effective, SearchTool::Grep)?,
        "cat" => rewrite_cat(arguments, &effective)?,
        _ => return None,
    };
    Some(format!("{prefix}{rewritten}"))
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchTool {
    Rg,
    Grep,
}

fn rewrite_search(arguments: &[String], cwd: &Path, tool: SearchTool) -> Option<String> {
    let (mut recursive, mut ignore_case, mut literal) = (false, false, false);
    let mut positional = Vec::new();
    for argument in arguments {
        if argument.len() > 1 && argument.starts_with('-') {
            // A flag after the pattern is still a flag to the tool, not a path.
            if !positional.is_empty() {
                return None;
            }
            for flag in argument[1..].chars() {
                match (tool, flag) {
                    (_, 'n') => {}
                    (_, 'i') => ignore_case = true,
                    (_, 'F') => literal = true,
                    (SearchTool::Grep, 'r' | 'R') => recursive = true,
                    _ => return None,
                }
            }
        } else {
            positional.push(argument.as_str());
        }
    }
    if tool == SearchTool::Grep && !recursive {
        return None;
    }
    let (pattern, path) = match positional.as_slice() {
        [pattern] => (*pattern, None),
        [pattern, path] => (*pattern, Some(*path)),
        _ => return None,
    };
    if pattern.is_empty() || pattern.contains('\n') {
        return None;
    }
    // grep basic regex syntax differs from ctx regex: only plain patterns are safe.
    if tool == SearchTool::Grep && !literal && pattern.contains(GREP_REGEX_METACHARACTERS) {
        return None;
    }
    let literal = literal || tool == SearchTool::Grep;
    let target = match path {
        Some(path) => cwd.join(path),
        None => cwd.to_path_buf(),
    };
    if !has_index(&target) {
        return None;
    }
    let mut command = format!(
        "ctx search {} --mode {}",
        quote(pattern),
        if literal { "literal" } else { "regex" }
    );
    if ignore_case {
        command.push_str(" --ignore-case");
    }
    if let Some(path) = path.filter(|path| !matches!(*path, "." | "./")) {
        command.push_str(&format!(" --path {}", quote(path)));
    }
    command.push_str(&format!(
        " --limit {SEARCH_LIMIT} --budget-tokens {SEARCH_BUDGET_TOKENS}"
    ));
    Some(command)
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
        if character != '\n' && character.is_whitespace() {
            return Some((word, index));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed_dir() -> tempfile::TempDir {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir_all(temporary.path().join(".ctx")).unwrap();
        fs::write(temporary.path().join(".ctx/index.sqlite"), b"").unwrap();
        fs::create_dir_all(temporary.path().join("src")).unwrap();
        temporary
    }

    fn lines(count: usize) -> String {
        (0..count).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn rewrites_rg_with_flags_pattern_and_path() {
        let dir = indexed_dir();
        let cwd = dir.path();
        assert_eq!(
            rewrite_command("rg -n foo", cwd).unwrap(),
            "ctx search 'foo' --mode regex --limit 200 --budget-tokens 4000"
        );
        assert_eq!(
            rewrite_command("rg -i -F foo src", cwd).unwrap(),
            "ctx search 'foo' --mode literal --ignore-case --path 'src' --limit 200 --budget-tokens 4000"
        );
        assert_eq!(
            rewrite_command("rg -ni 'fo o' src", cwd).unwrap(),
            "ctx search 'fo o' --mode regex --ignore-case --path 'src' --limit 200 --budget-tokens 4000"
        );
        assert_eq!(
            rewrite_command("rg -Fn \"a.b\"", cwd).unwrap(),
            "ctx search 'a.b' --mode literal --limit 200 --budget-tokens 4000"
        );
    }

    #[test]
    fn rewrites_grep_recursive_with_literal_or_plain_patterns() {
        let dir = indexed_dir();
        let cwd = dir.path();
        assert_eq!(
            rewrite_command("grep -rn foo src", cwd).unwrap(),
            "ctx search 'foo' --mode literal --path 'src' --limit 200 --budget-tokens 4000"
        );
        assert_eq!(
            rewrite_command("grep -Rni foo", cwd).unwrap(),
            "ctx search 'foo' --mode literal --ignore-case --limit 200 --budget-tokens 4000"
        );
        assert_eq!(
            rewrite_command("grep -rnF 'a.b' src", cwd).unwrap(),
            "ctx search 'a.b' --mode literal --path 'src' --limit 200 --budget-tokens 4000"
        );
        assert_eq!(
            rewrite_command("grep -r -n foo", cwd).unwrap(),
            "ctx search 'foo' --mode literal --limit 200 --budget-tokens 4000"
        );
        for pattern in [
            "a.b", "a[b]", "a*", "a+", "a?", "^a", "a$", "a|b", "(a)", "{a}", "a\\\\b",
        ] {
            assert!(
                rewrite_command(&format!("grep -rn '{pattern}'"), cwd).is_none(),
                "{pattern}"
            );
        }
        assert!(rewrite_command("grep -n foo src", cwd).is_none());
        assert!(rewrite_command("grep -rl foo", cwd).is_none());
    }

    #[test]
    fn rewrites_cat_of_long_regular_file_only() {
        let dir = indexed_dir();
        let cwd = dir.path();
        fs::write(cwd.join("src/long.rs"), lines(201)).unwrap();
        fs::write(cwd.join("src/short.rs"), lines(200)).unwrap();
        assert_eq!(
            rewrite_command("cat src/long.rs", cwd).unwrap(),
            "ctx read 'src/long.rs'"
        );
        assert!(rewrite_command("cat src/short.rs", cwd).is_none());
        assert!(rewrite_command("cat src/missing.rs", cwd).is_none());
        assert!(rewrite_command("cat src", cwd).is_none());
        assert!(rewrite_command("cat src/long.rs src/short.rs", cwd).is_none());
        assert!(rewrite_command("cat -n src/long.rs", cwd).is_none());
        assert!(rewrite_command("cat", cwd).is_none());
    }

    #[test]
    fn keeps_a_cd_prefix_and_resolves_checks_against_it() {
        let dir = indexed_dir();
        let cwd = dir.path();
        fs::write(cwd.join("src/long.rs"), lines(250)).unwrap();
        assert_eq!(
            rewrite_command("cd src && rg -n foo", cwd).unwrap(),
            "cd src && ctx search 'foo' --mode regex --limit 200 --budget-tokens 4000"
        );
        assert_eq!(
            rewrite_command("cd 'src'   &&   cat long.rs", cwd).unwrap(),
            "cd 'src'   &&   ctx read 'long.rs'"
        );
        let absolute = format!("cd {} && rg foo", cwd.display());
        assert!(rewrite_command(&absolute, Path::new("/")).is_some());
        assert!(rewrite_command("cd missing && rg foo", cwd).is_none());
        assert!(rewrite_command("cd src && rg foo && ls", cwd).is_none());
        assert!(rewrite_command("cd src; rg foo", cwd).is_none());
        assert!(rewrite_command("cd src && cd .. && rg foo", cwd).is_none());
        assert!(rewrite_command("cd src", cwd).is_none());
    }

    #[test]
    fn quotes_single_quotes_in_the_pattern() {
        let dir = indexed_dir();
        assert_eq!(
            rewrite_command("rg \"it's\"", dir.path()).unwrap(),
            "ctx search 'it'\\''s' --mode regex --limit 200 --budget-tokens 4000"
        );
        assert_eq!(
            rewrite_command("rg 'it'\\''s'", dir.path()).unwrap(),
            "ctx search 'it'\\''s' --mode regex --limit 200 --budget-tokens 4000"
        );
    }

    #[test]
    fn leaves_unsupported_commands_alone() {
        let dir = indexed_dir();
        let cwd = dir.path();
        for command in [
            "rg foo | head",
            "rg foo > out.txt",
            "rg foo < in.txt",
            "rg foo; ls",
            "rg foo && ls",
            "rg foo &",
            "rg $(echo foo)",
            "rg \"$HOME\"",
            "rg `echo foo`",
            "rg foo src/*.rs",
            "rg foo src/?.rs",
            "rg foo\nls",
            "rg --json foo",
            "rg -A 3 foo",
            "rg -e foo",
            "rg foo src src",
            "rg foo -n",
            "rg",
            "rg -n",
            "rg ''",
            "ls | head",
            "echo rg foo",
            "git grep foo",
            "",
            "   ",
            "rg 'unterminated",
        ] {
            assert!(rewrite_command(command, cwd).is_none(), "{command:?}");
        }
    }

    #[test]
    fn requires_an_index() {
        // An inherited CTX_DIR would make every directory look indexed.
        if std::env::var_os("CTX_DIR").is_some() {
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let cwd = temporary.path();
        fs::write(cwd.join("a.txt"), lines(300)).unwrap();
        assert!(rewrite_command("rg foo", cwd).is_none());
        assert!(rewrite_command("cat a.txt", cwd).is_none());
        assert!(rewrite_command("grep -rn foo", cwd).is_none());
    }

    #[test]
    fn response_shape_differs_between_harnesses_and_keeps_tool_input() {
        let dir = indexed_dir();
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "rg -n foo", "description": "find foo", "timeout": 5000},
            "cwd": dir.path(),
        });
        let claude = hook_response(&payload, "claude").unwrap();
        let output = &claude["hookSpecificOutput"];
        assert_eq!(output["hookEventName"], "PreToolUse");
        assert_eq!(output["permissionDecisionReason"], "ctx: bounded output");
        assert!(output.get("permissionDecision").is_none());
        assert_eq!(
            output["updatedInput"],
            json!({
                "command": "ctx search 'foo' --mode regex --limit 200 --budget-tokens 4000",
                "description": "find foo",
                "timeout": 5000,
            })
        );
        let codex = hook_response(&payload, "codex").unwrap();
        assert_eq!(codex["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(
            codex["hookSpecificOutput"]["updatedInput"],
            output["updatedInput"]
        );
        assert!(hook_response(&payload, "other").is_none());
    }

    #[test]
    fn response_ignores_other_tools_and_malformed_payloads() {
        let dir = indexed_dir();
        let cwd = dir.path();
        for tool in ["shell", "exec_command"] {
            let payload =
                json!({"tool_name": tool, "tool_input": {"command": "rg foo"}, "cwd": cwd});
            assert!(hook_response(&payload, "codex").is_some(), "{tool}");
        }
        for payload in [
            json!({"tool_name": "Read", "tool_input": {"command": "rg foo"}, "cwd": cwd}),
            json!({"tool_name": "Bash", "tool_input": {"command": 3}, "cwd": cwd}),
            json!({"tool_name": "Bash", "tool_input": {}, "cwd": cwd}),
            json!({"tool_name": "Bash", "cwd": cwd}),
            json!({"tool_input": {"command": "rg foo"}, "cwd": cwd}),
            json!({"tool_name": "Bash", "tool_input": {"command": "ls | head"}, "cwd": cwd}),
            json!("not an object"),
        ] {
            assert!(hook_response(&payload, "claude").is_none(), "{payload}");
        }
    }
}
