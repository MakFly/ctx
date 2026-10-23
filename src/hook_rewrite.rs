//! PreToolUse hook that makes a few shell commands cheaper to read without changing what they do.
//!
//! A search (`rg`, `grep`, `git grep`, optionally piped to head/tail/sort/uniq) runs unchanged and
//! its output is piped to `ctx compact`, which groups matches by file and bounds the output. A
//! list is rewritten only when every other command in it is a plain read, since Codex runs the
//! rewritten command without asking. Anything else runs untouched; the hook never blocks.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Token budget for the definition bodies appended to a compacted search.
const EXPAND_TOKENS: usize = 1_500;

/// Rewrites `command` into a cheaper equivalent, or returns `None` to leave it unchanged.
pub fn rewrite_command(command: &str, cwd: &Path) -> Option<String> {
    // A newline separates commands in the shell, and other control characters have no
    // place in a search: either could hide a second command, so nothing is rewritten.
    if command.chars().any(|c| c.is_control() && c != '\t') {
        return None;
    }
    let (prefix, directory, rest) = split_cd_prefix(command)?;
    let effective = match &directory {
        Some(directory) => cwd.join(directory),
        None => cwd.to_path_buf(),
    };
    // A `cd` into a directory that does not exist fails in the shell: leave it alone.
    if directory.is_some() && !effective.is_dir() {
        return None;
    }
    // A list `a; b && c`: each search in it is compacted; any other command must be a
    // plain read, or the whole list is left alone.
    let segments = split_list(rest)?;
    let mut changed = false;
    let mut out = String::new();
    for (segment, separator) in &segments {
        match rewrite_search_pipeline(segment) {
            Some(rewritten) => {
                changed = true;
                out.push_str(&rewritten);
            }
            None => {
                let words = tokenize(segment)?;
                if split_pipeline(segment)?.len() != 1 || !is_plain_read(&words) {
                    return None;
                }
                out.push_str(segment.trim_end());
            }
        }
        out.push_str(separator);
    }
    changed.then(|| format!("{prefix}{out}"))
}

/// A command that only prints files or directory names: `sed -n 'A,Bp'`, `cat`, `nl`,
/// `head`/`tail -n N`, `wc`, `ls`, `pwd`, with no option that writes or runs anything.
fn is_plain_read(words: &[String]) -> bool {
    let Some((program, arguments)) = words.split_first() else {
        return false;
    };
    let operands_only = |allowed: &[&str]| {
        arguments
            .iter()
            .all(|argument| !argument.starts_with('-') || allowed.contains(&argument.as_str()))
    };
    match program.as_str() {
        "sed" => {
            arguments.first().is_some_and(|flag| flag == "-n")
                && arguments.len() >= 3
                && arguments[1..].iter().enumerate().all(|(index, argument)| {
                    if index == 0 {
                        is_print_range(argument)
                    } else {
                        !argument.starts_with('-')
                    }
                })
        }
        "cat" => operands_only(&["-n"]),
        "nl" => operands_only(&["-ba"]),
        "wc" => operands_only(&["-l", "-c", "-w"]),
        "ls" => operands_only(&["-l", "-a", "-la", "-al", "-1", "-R"]),
        "pwd" => arguments.is_empty(),
        "head" | "tail" => count_options(arguments).is_some(),
        _ => false,
    }
}

/// `N p` or `A,B p` scripts only: `sed` with anything else may write files (`w`) or run (`e`).
fn is_print_range(script: &str) -> bool {
    let Some(range) = script.strip_suffix('p') else {
        return false;
    };
    let mut bounds = range.split(',');
    let first = bounds.next().unwrap_or_default();
    let second = bounds.next();
    bounds.next().is_none()
        && !first.is_empty()
        && first.bytes().all(|byte| byte.is_ascii_digit())
        && second.is_none_or(|second| {
            second == "$"
                || (!second.is_empty() && second.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

/// `head`/`tail` options: `-n N`, `-nN`, `-N`, `--lines=N`; returns the operands (files).
fn count_options(arguments: &[String]) -> Option<Vec<&String>> {
    let mut operands = Vec::new();
    let mut index = 0;
    let number = |text: &str| {
        !text.is_empty()
            && text
                .trim_start_matches(['+', '-'])
                .bytes()
                .all(|byte| byte.is_ascii_digit())
    };
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "-n" {
            if !number(arguments.get(index + 1)?) {
                return None;
            }
            index += 2;
            continue;
        }
        if let Some(count) = argument
            .strip_prefix("--lines=")
            .or_else(|| argument.strip_prefix("-n"))
            .or_else(|| argument.strip_prefix('-'))
            .filter(|_| argument.starts_with('-'))
        {
            if !number(count) {
                return None;
            }
        } else {
            operands.push(argument);
        }
        index += 1;
    }
    Some(operands)
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
    let command = command.trim();
    // `-H` prints the path even for a single file, so the lines parse and the
    // source around them can be appended.
    let command = match program.as_str() {
        "rg" | "grep" => match command.strip_prefix(program.as_str()) {
            Some(rest) if rest.starts_with([' ', '\t']) => format!("{program} -H{rest}"),
            _ => command.to_owned(),
        },
        _ => command.to_owned(),
    };
    // With no filter the search's own status is the pipeline's: `ctx compact --fail-empty`
    // exits 1 on no output, as `rg` and `grep` do on no match, so `rg x && y` still gates `y`.
    if filters.is_empty() {
        Some(format!("{command} | ctx compact --fail-empty"))
    } else {
        Some(format!("{command} | ctx compact"))
    }
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

/// `head`, `tail`, `sort` or `uniq` reading the pipe, with options that only filter or
/// reorder lines (no `sort -o` output file, no `--compress-program`, no file operand).
fn is_line_filter(stage: &[String]) -> bool {
    let Some((program, arguments)) = stage.split_first() else {
        return false;
    };
    let letters = |allowed: &str| {
        arguments.iter().all(|argument| {
            argument.len() > 1
                && argument.starts_with('-')
                && !argument.starts_with("--")
                && argument[1..].chars().all(|letter| allowed.contains(letter))
        })
    };
    match program.as_str() {
        "head" | "tail" => count_options(arguments).is_some_and(|operands| operands.is_empty()),
        "sort" => {
            let mut index = 0;
            while index < arguments.len() {
                let argument = &arguments[index];
                if argument == "-k" || argument == "-t" {
                    match arguments.get(index + 1) {
                        Some(value) if !value.starts_with('-') || value.len() == 1 => index += 2,
                        _ => return false,
                    }
                    continue;
                }
                if !(argument.len() > 1
                    && argument.starts_with('-')
                    && !argument.starts_with("--")
                    && (argument[1..]
                        .chars()
                        .all(|letter| "nrufbhV".contains(letter))
                        || (argument.starts_with("-k")
                            && argument[2..]
                                .chars()
                                .all(|c| c.is_ascii_digit() || ",.nr".contains(c)))))
                {
                    return false;
                }
                index += 1;
            }
            true
        }
        "uniq" => letters("cdui"),
        _ => false,
    }
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
    // A search of one file prints bare `line:text`; name the file so the lines
    // parse and the source around them can be appended.
    let single = payload
        .pointer("/tool_input/path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .map(|path| match &root {
            Some(root) if path.is_relative() => root.join(path),
            _ => path,
        })
        .filter(|path| path.is_file());
    let named;
    let content = match &single {
        Some(file) => {
            let shown = root
                .as_deref()
                .and_then(|root| file.strip_prefix(root).ok())
                .unwrap_or(file)
                .to_string_lossy()
                .into_owned();
            named = content
                .lines()
                .map(|line| {
                    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
                    if digits > 0 && matches!(line.as_bytes().get(digits), Some(b':' | b'-')) {
                        format!("{shown}:{line}")
                    } else {
                        line.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            named.as_str()
        }
        None => content,
    };
    let options = crate::compact::CompactOptions {
        expand_tokens: EXPAND_TOKENS,
        root,
        ..crate::compact::CompactOptions::default()
    };
    let plain = crate::compact::compact(
        content,
        crate::compact::CompactOptions {
            expand_tokens: 0,
            ..options.clone()
        },
    );
    let compacted = crate::compact::compact(content, options);
    // The appended source is the point (it saves the read that follows), so it is
    // kept even when it makes the result longer; plain compaction must shrink it.
    let expanded = compacted.len() > plain.len();
    let compacted = compacted.trim_end_matches('\n');
    if !expanded && compacted.len() >= content.len() {
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
                Some(
                    format!(
                        "{} | ctx compact{}",
                        command
                            .replacen("rg ", "rg -H ", 1)
                            .replacen("grep -rn", "grep -H -rn", 1)
                            .replacen("git grep -H -rn", "git grep -rn", 1),
                        if command.contains(" | ") {
                            ""
                        } else {
                            " --fail-empty"
                        }
                    )
                    .as_str()
                ),
                "{command}"
            );
        }
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        assert_eq!(
            rewrite_command("cd src && rg -n foo", cwd).unwrap(),
            "cd src && rg -H -n foo | ctx compact --fail-empty"
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
            "rg -H foo | ctx compact",
            "sed -n '1,40p' src/a.rs",
            "ls | head",
            "",
            "rg foo\r",
            "rg foo\nrm victim",
            "rg foo \\\nrm victim",
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
            "sed -n '1,20p' a.rs; rg -H -n foo src | ctx compact --fail-empty"
        );
        assert_eq!(
            rewrite_command("rg -n a && rg -n b | head -5", cwd).unwrap(),
            "rg -H -n a | ctx compact --fail-empty && rg -H -n b | head -5 | ctx compact"
        );
        assert!(rewrite_command("sed -n '1,2p' a; ls", cwd).is_none());
        assert!(rewrite_command("rg a; rm x > y", cwd).is_none());
        assert!(rewrite_command("rg a & rg b", cwd).is_none());
        assert!(rewrite_command("rg a || rg b", cwd).is_none());
    }

    #[test]
    fn a_list_is_rewritten_only_when_every_other_command_is_a_plain_read() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        for command in [
            "rg foo; rm victim",
            "rg foo && rm victim",
            "rg foo; bun test",
            "rg foo; git diff",
            "rg foo; sed -n '1w /tmp/x' a",
            "rg foo; sed -n 'e rm x' a",
            "rg foo; sed -i -n '1p' a",
            "rg foo; cat --squeeze-blank a",
            "rg foo | sort -o/tmp/victim",
            "rg foo | sort -o /tmp/victim",
            "rg foo | sort --compress-program=sh",
            "rg foo | sort -T /tmp",
            "rg foo | uniq - out.txt",
            "rg foo | head -c 5",
            "cat a.rs",
        ] {
            assert!(rewrite_command(command, cwd).is_none(), "{command:?}");
        }
        for command in [
            "rg foo | sort -k2 -n | uniq -c",
            "sed -n '10,$p' a.rs && rg foo",
            "cat a.rs; nl -ba b.rs; head -n 20 c.rs; rg foo",
        ] {
            assert!(rewrite_command(command, cwd).is_some(), "{command:?}");
        }
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
        // A single match grows once the lines around it are appended, and is still sent.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ts"), "one\ntwo\nthree\n").unwrap();
        let near = json!({"tool_name": "Grep", "cwd": dir.path(),
            "tool_response": {"mode": "content", "content": "a.ts:2:two", "numLines": 1}});
        let out = post_tool_response(&near, "claude").unwrap();
        let content = out["hookSpecificOutput"]["updatedToolOutput"]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.contains("around the matches:\n## a.ts 1-3\n1\tone\n"),
            "{content}"
        );
        // A search of one file prints bare `line:text`: the file is named from the input.
        let one = json!({"tool_name": "Grep", "cwd": dir.path(), "tool_input": {"path": "a.ts"},
            "tool_response": {"mode": "content", "content": "2:two", "numLines": 1}});
        let out = post_tool_response(&one, "claude").unwrap();
        let content = out["hookSpecificOutput"]["updatedToolOutput"]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.starts_with("a.ts\n2:two\naround the matches:\n## a.ts 1-3\n"),
            "{content}"
        );
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
            "rg -H -n foo | ctx compact --fail-empty"
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
