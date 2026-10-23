//! PreToolUse hook that makes a few shell commands cheaper to read without changing what they do.
//!
//! A search (`rg` or `grep` with options from a known safe set), alone or in a `;`/`&&` list
//! with plain reads (`sed -n 'A,Bp'`, `cat`, `nl`, `head`, `tail`, `wc`), becomes
//! `ctx compact --run -- ...`: ctx starts each step itself from PATH, without a shell, prints
//! reads as is and searches grouped by file with the source they point at, and exits with the
//! status the shell would give. Codex runs the rewrite without asking, which is why every step
//! is checked again before it starts. Anything else (a pipe, a redirect, an expansion) runs
//! untouched; the hook never blocks.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Token budget for the definition bodies appended to a compacted search.
const EXPAND_TOKENS: usize = 600;

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
    // A list `a; b && c` of searches and plain reads: ctx runs every step itself, from
    // PATH and without a shell, keeping `&&` and `;` and the last step's status.
    let segments = split_list(rest)?;
    let mut words = Vec::new();
    let mut searched = false;
    for (index, (segment, separator)) in segments.iter().enumerate() {
        if split_pipeline(segment)?.len() != 1 {
            return None;
        }
        let step = tokenize(segment)?;
        let (program, arguments) = step.split_first()?;
        if step
            .iter()
            .any(|word| RUN_SEPARATORS.contains(&word.as_str()))
            || (index + 1 < segments.len() && step.iter().any(|word| word == "--"))
        {
            return None;
        }
        if is_search(program, arguments) {
            // Output without a path on each line is not what ctx compact reads.
            if arguments
                .iter()
                .any(|argument| matches!(argument.as_str(), "--heading" | "--no-filename" | "-h"))
            {
                return None;
            }
            searched = true;
            // `-H -n` print the path and line number on every line, even for one file,
            // so the lines parse and the source around them can be appended.
            let numbered = arguments.iter().any(|argument| {
                argument == "--line-number"
                    || (argument.starts_with('-')
                        && !argument.starts_with("--")
                        && argument.contains('n'))
            });
            words.push(program.clone());
            words.push("-H".to_owned());
            if !numbered {
                words.push("-n".to_owned());
            }
            words.extend(arguments.iter().cloned());
        } else if is_plain_read(&step) {
            words.extend(step.iter().cloned());
        } else {
            return None;
        }
        match separator.trim_matches(BLANKS) {
            "&&" if index + 1 < segments.len() => words.push(RUN_AND.to_owned()),
            ";" if index + 1 < segments.len() => words.push(RUN_THEN.to_owned()),
            ";" | "" => {}
            _ => return None,
        }
    }
    let run = words
        .iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ");
    searched.then(|| format!("{prefix}ctx compact --run -- {run}"))
}

/// Separators of a `ctx compact --run` list: `&&` and `;`.
pub const RUN_AND: &str = "--and";
pub const RUN_THEN: &str = "--then";
const RUN_SEPARATORS: [&str; 2] = [RUN_AND, RUN_THEN];

/// One step of a `ctx compact --run` list.
#[derive(Debug, PartialEq, Eq)]
pub enum RunStep {
    /// An `rg` or `grep` search, compacted.
    Search(Vec<String>),
    /// A plain read, printed as is.
    Read(Vec<String>),
}

/// The steps of a `ctx compact --run` list, each with whether it runs only after a
/// success (`&&`); `None` when a step is neither a known-safe search nor a plain read.
pub fn parse_run(words: &[String]) -> Option<Vec<(bool, RunStep)>> {
    let mut steps = Vec::new();
    let (mut start, mut after_and) = (0, false);
    for index in 0..=words.len() {
        let separator = words
            .get(index)
            .map(String::as_str)
            .filter(|word| RUN_SEPARATORS.contains(word));
        if index < words.len() && separator.is_none() {
            continue;
        }
        let part = &words[start..index];
        // After `--` a separator-looking word could be an operand: such a step must end
        // the list, so no word after its `--` is ever read as a separator.
        if separator.is_some() && part.iter().any(|word| word == "--") {
            return None;
        }
        let (program, arguments) = part.split_first()?;
        let step = if is_search(program, arguments) {
            RunStep::Search(part.to_vec())
        } else if is_plain_read(part) {
            RunStep::Read(part.to_vec())
        } else {
            return None;
        };
        steps.push((after_and, step));
        after_and = separator == Some(RUN_AND);
        start = index + 1;
    }
    Some(steps)
}

/// A command that only prints files: `sed -n 'A,Bp'`, `cat`, `nl`, `head`/`tail -n N`,
/// `wc`, with no option that writes or runs anything. (`pwd` is not one: the external
/// program prints the physical directory where the shell builtin prints the logical one.)
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

/// `word` as one shell word: bare when it holds only safe characters, else single-quoted.
fn shell_word(word: &str) -> String {
    // A leading `=` expands to a program path in zsh (`=ls`): it is quoted too.
    if !word.is_empty()
        && !word.starts_with('=')
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./=:,+@%-".contains(&byte))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
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
                let segment = command[start..index].trim_matches(BLANKS);
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
    let last = command[start..].trim_matches(BLANKS);
    if !last.is_empty() {
        segments.push((last.to_owned(), String::new()));
    }
    Some(segments)
}

/// Options of a search that only select, match or format: anything else (`rg --pre`,
/// `--hostname-bin`, `-z`, an abbreviated long option...) leaves the command alone.
struct SearchOptions {
    /// Short letters taking no value.
    flags: &'static str,
    /// Short letters taking a value (attached, or the next word).
    valued: &'static str,
    /// Long options taking no value.
    long_flags: &'static [&'static str],
    /// Long options taking a value (`--name=value`, or the next word).
    long_valued: &'static [&'static str],
}

const RG: SearchOptions = SearchOptions {
    flags: "nNHhiSswxFUPvcloLuI",
    valued: "egtTCABmMdj",
    long_flags: &[
        "--line-number",
        "--no-line-number",
        "--with-filename",
        "--no-filename",
        "--ignore-case",
        "--smart-case",
        "--case-sensitive",
        "--word-regexp",
        "--line-regexp",
        "--fixed-strings",
        "--multiline",
        "--multiline-dotall",
        "--pcre2",
        "--invert-match",
        "--count",
        "--count-matches",
        "--files-with-matches",
        "--files-without-match",
        "--only-matching",
        "--hidden",
        "--no-ignore",
        "--no-ignore-vcs",
        "--follow",
        "--no-heading",
        "--heading",
        "--trim",
        "--column",
        "--no-messages",
        "--byte-offset",
        "--vimgrep",
        "--files",
    ],
    long_valued: &[
        "--regexp",
        "--glob",
        "--iglob",
        "--type",
        "--type-not",
        "--context",
        "--after-context",
        "--before-context",
        "--max-count",
        "--max-columns",
        "--max-depth",
        "--sort",
        "--sortr",
        "--color",
        "--threads",
        "--max-filesize",
        "--engine",
    ],
};

const GREP: SearchOptions = SearchOptions {
    flags: "nrRiwxFEGPvclLHhosqIa",
    valued: "efABCm",
    long_flags: &[
        "--line-number",
        "--recursive",
        "--dereference-recursive",
        "--ignore-case",
        "--word-regexp",
        "--line-regexp",
        "--fixed-strings",
        "--extended-regexp",
        "--perl-regexp",
        "--invert-match",
        "--count",
        "--files-with-matches",
        "--files-without-match",
        "--with-filename",
        "--no-filename",
        "--only-matching",
        "--no-messages",
        "--quiet",
        "--silent",
    ],
    long_valued: &[
        "--regexp",
        "--file",
        "--after-context",
        "--before-context",
        "--context",
        "--max-count",
        "--include",
        "--exclude",
        "--exclude-dir",
        "--color",
        "--colour",
        "--binary-files",
    ],
};

/// A search whose output is `path:line:text` lines, with only options that select,
/// match or format (see [`SearchOptions`]); operands are patterns and paths.
fn is_search(program: &str, arguments: &[String]) -> bool {
    let options = match program {
        // An rg config file may add `--pre` and friends, which this check cannot see.
        "rg" if std::env::var_os("RIPGREP_CONFIG_PATH").is_some_and(|path| !path.is_empty()) => {
            return false;
        }
        "rg" => &RG,
        "grep" => &GREP,
        _ => return false,
    };
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        index += 1;
        if argument == "--" {
            return true;
        }
        if let Some(long) = argument.strip_prefix("--") {
            let name = format!("--{}", long.split('=').next().unwrap_or_default());
            if long.contains('=') {
                if !options.long_valued.contains(&name.as_str()) {
                    return false;
                }
            } else if options.long_valued.contains(&argument) {
                if index >= arguments.len() {
                    return false;
                }
                index += 1;
            } else if !options.long_flags.contains(&argument) {
                return false;
            }
            continue;
        }
        let Some(letters) = argument
            .strip_prefix('-')
            .filter(|letters| !letters.is_empty())
        else {
            continue;
        };
        for (offset, letter) in letters.char_indices() {
            if options.valued.contains(letter) {
                // The rest of the word is the value, or the next word is.
                if offset + letter.len_utf8() == letters.len() {
                    if index >= arguments.len() {
                        return false;
                    }
                    index += 1;
                }
                break;
            }
            if !options.flags.contains(letter) {
                return false;
            }
        }
    }
    true
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
    hook_response_with(payload, harness, None)
}

/// [`hook_response`] with the token budget for the source appended to each search
/// (`None`: the `ctx compact` default).
pub fn hook_response_with(
    payload: &Value,
    harness: &str,
    expand_tokens: Option<usize>,
) -> Option<Value> {
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
    // Bash and shell send `command`; exec_command sends `cmd`: the same field is rewritten.
    let field = ["command", "cmd"]
        .into_iter()
        .find(|field| tool_input.get(*field).is_some_and(Value::is_string))?;
    let command = tool_input.get(field)?.as_str()?;
    let cwd = match payload.get("cwd").and_then(Value::as_str) {
        Some(cwd) if !cwd.is_empty() => PathBuf::from(cwd),
        _ => std::env::current_dir().ok()?,
    };
    // Codex runs the rewritten command without asking (it applies `updatedInput` only with
    // an explicit allow): no `cd` prefix, which the shell would still run, only what
    // `ctx compact --run` starts itself from PATH after checking every step.
    if harness == "codex" && split_cd_prefix(command)?.1.is_some() {
        return None;
    }
    let mut rewritten = rewrite_command(command, &cwd)?;
    if let Some(tokens) = expand_tokens {
        rewritten = rewritten.replacen(
            "ctx compact --run -- ",
            &format!("ctx compact --expand-tokens {tokens} --run -- "),
            1,
        );
    }
    let mut updated = tool_input.clone();
    updated.insert(field.to_owned(), Value::String(rewritten));
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
    let Some(after_cd) = command.trim_start_matches(BLANKS).strip_prefix("cd") else {
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
                        // In double quotes a backslash escapes only `"`, `\\`, `$` and a backtick.
                        escaped @ ('"' | '\\' | '$' | '`') => word.push(escaped),
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
            // Bash expands an unquoted `~` after `=` too (`--glob=~/x`): decline it anywhere.
            '~' => return None,
            // zsh expands an unquoted leading `=` to a program path (`=ls`).
            '#' | '=' if at_start => return None,
            other => word.push(other),
        }
        at_start = false;
    }
    Some((word, text.len()))
}

/// PostToolUse response that compacts a Grep `content` result (Claude), keeping the
/// tool's output shape; `None` when there is nothing to gain.
pub fn post_tool_response(payload: &Value, harness: &str) -> Option<Value> {
    post_tool_response_with(payload, harness, None)
}

/// [`post_tool_response`] with the token budget for the appended source.
pub fn post_tool_response_with(
    payload: &Value,
    harness: &str,
    expand_tokens: Option<usize>,
) -> Option<Value> {
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
        expand_tokens: expand_tokens.unwrap_or(EXPAND_TOKENS),
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
        for (command, rewritten) in [
            ("rg -n foo", "ctx compact --run -- rg -H -n foo"),
            (
                "rg -n \"a|b\" src -g '*.ts'",
                "ctx compact --run -- rg -H -n 'a|b' src -g '*.ts'",
            ),
            ("grep -rn foo .", "ctx compact --run -- grep -H -rn foo ."),
            // Inside double quotes `\$` is a literal `$`, kept so by the requoted word.
            (
                "grep -F \"\\$HOME\" a.rs",
                "ctx compact --run -- grep -H -n -F '$HOME' a.rs",
            ),
            // zsh expands a bare leading `=` to a program path.
            ("rg -n x '=ls'", "ctx compact --run -- rg -H -n x '=ls'"),
            (
                "rg -n \"it's\" a.rs",
                "ctx compact --run -- rg -H -n 'it'\\''s' a.rs",
            ),
        ] {
            assert_eq!(
                rewrite_command(command, cwd).as_deref(),
                Some(rewritten),
                "{command}"
            );
        }
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        assert_eq!(
            rewrite_command("cd src && rg -n foo", cwd).unwrap(),
            "cd src && ctx compact --run -- rg -H -n foo"
        );
    }

    #[test]
    fn codex_gets_no_cd_prefix_since_it_runs_the_rewrite_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let payload = |command: &str| json!({"tool_name": "Bash", "cwd": dir.path(), "tool_input": {"command": command}});
        // exec_command sends `cmd`: that field is the one rewritten.
        let exec = json!({"tool_name": "exec_command", "cwd": dir.path(), "tool_input": {"cmd": "rg foo"}});
        let out = hook_response(&exec, "codex").unwrap();
        assert_eq!(
            out["hookSpecificOutput"]["updatedInput"]["cmd"],
            "ctx compact --run -- rg -H -n foo"
        );
        assert!(
            out["hookSpecificOutput"]["updatedInput"]
                .get("command")
                .is_none()
        );
        // The shell would still run the `cd`: Codex does not get it, Claude may.
        assert!(hook_response(&payload("cd src && rg foo"), "codex").is_none());
        assert!(hook_response(&payload("cd src && rg foo"), "claude").is_some());
        for command in ["rg foo", "sed -n '1,2p' a.rs; rg foo", "rg a && rg b"] {
            let out = hook_response(&payload(command), "codex").unwrap();
            assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
            let rewritten = out["hookSpecificOutput"]["updatedInput"]["command"]
                .as_str()
                .unwrap();
            assert!(
                rewritten.starts_with("ctx compact --run -- "),
                "{rewritten}"
            );
        }
    }

    #[test]
    fn a_run_list_is_parsed_into_checked_steps_with_their_operators() {
        let words = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            parse_run(&words("sed -n 1,2p a.rs --and rg -H foo --then cat b.rs")).unwrap(),
            vec![
                (false, RunStep::Read(words("sed -n 1,2p a.rs"))),
                (true, RunStep::Search(words("rg -H foo"))),
                (false, RunStep::Read(words("cat b.rs"))),
            ]
        );
        for refused in [
            "sh -c true",
            "rg --pre=sh foo",
            "rg foo --and",
            "pwd",
            "grep -- --then pwd",
            "grep -- x a --and cat b",
            "--then rg foo",
            "rm x",
        ] {
            assert!(parse_run(&words(refused)).is_none(), "{refused}");
        }
    }

    #[test]
    fn a_search_with_an_option_outside_the_known_safe_set_is_left_alone() {
        let cwd = Path::new("/");
        for command in [
            "rg --hostname-bin=sh foo",
            "rg --hostname-bin sh foo",
            "rg --pre=cat foo",
            "rg --search-zip foo",
            "rg --type-add 'x:*.x' foo",
            "rg --line-num foo",
            "grep --incl=*.rs foo .",
            "git grep -n foo",
            "git grep --textconv foo",
            "git grep --open-files-in-pag=printf foo",
            "rg foo ~/x",
            "rg --glob=~/x foo",
            "rg --heading foo",
            "rg -h foo",
            "grep --no-filename -r foo .",
            "rg =ls",
            "rg foo\u{a0}",
            "rg\u{a0}foo",
            "rg -e",
        ] {
            assert!(rewrite_command(command, cwd).is_none(), "{command:?}");
        }
        for command in [
            "rg -niC3 foo src",
            "rg -g '*.ts' --max-count=5 -t rust foo",
            "rg -- --pre src",
            "grep -rn --include='*.rs' -e foo .",
        ] {
            assert!(rewrite_command(command, cwd).is_some(), "{command:?}");
        }
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
            "ctx compact --run -- sed -n 1,20p a.rs --then rg -H -n foo src"
        );
        assert_eq!(
            rewrite_command("rg -n a && rg -n b", cwd).unwrap(),
            "ctx compact --run -- rg -H -n a --and rg -H -n b"
        );
        // A filtered search makes the pipeline's status the filter's: left alone.
        assert!(rewrite_command("rg -n a && rg -n b | head -5", cwd).is_none());
        assert!(rewrite_command("rg -n foo src | head -160", cwd).is_none());
        assert!(rewrite_command("sed -n '1,2p' a; ls", cwd).is_none());
        // A word equal to a run separator would be misread: left alone.
        assert!(rewrite_command("rg -- --and src", cwd).is_none());
        // After `--` a later separator could be an operand: such a search must come last.
        assert!(rewrite_command("rg -- x src && cat a.rs", cwd).is_none());
        assert!(rewrite_command("cat a.rs && rg -- x src", cwd).is_some());
        assert!(rewrite_command("pwd; rg foo", cwd).is_none());
        // Only plain reads and no search: nothing to compact.
        assert!(rewrite_command("cat a.rs; sed -n '1,2p' b.rs", cwd).is_none());
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
    fn a_budget_given_to_the_hook_reaches_the_rewritten_search() {
        let payload = json!({"tool_name": "Bash", "cwd": "/", "tool_input": {"command": "rg foo"}});
        let out = hook_response_with(&payload, "codex", Some(600)).unwrap();
        assert_eq!(
            out["hookSpecificOutput"]["updatedInput"]["command"],
            "ctx compact --expand-tokens 600 --run -- rg -H -n foo"
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
            "ctx compact --run -- rg -H -n foo"
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
