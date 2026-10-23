//! `ctx compact`: a stdin filter for search output (`path:line:text`, as printed by
//! `rg -n`, `grep -n` and `git grep -n`). It prints each path once, keeps every
//! line's number and text, shortens very long lines and bounds the total, saying
//! what it left out. Lines it does not recognise pass through unchanged.

use std::path::{Path, PathBuf};

/// Limits applied by `compact`.
#[derive(Clone, Debug)]
pub struct CompactOptions {
    /// Output lines kept before the rest is summarised.
    pub max_lines: usize,
    /// Characters of a match's text kept before it is shortened.
    pub max_line_chars: usize,
    /// Token budget for the bodies of the definitions the search found (0: none).
    pub expand_tokens: usize,
    /// Directory the search paths are relative to; no expansion without it.
    pub root: Option<PathBuf>,
}

impl Default for CompactOptions {
    fn default() -> Self {
        Self {
            max_lines: 250,
            max_line_chars: 240,
            expand_tokens: 0,
            root: None,
        }
    }
}

/// Largest single definition body shown in full, in tokens.
const MAX_BODY_TOKENS: usize = 700;
/// Lines shown before and after a match that is not a definition.
const WINDOW_BEFORE: usize = 8;
const WINDOW_AFTER: usize = 30;

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// A file's lines and the symbols parsed from it.
type Parsed = (Vec<String>, Vec<crate::model::Symbol>);

/// The bodies of the definitions whose first lines the search matched (a search for
/// a function usually precedes reading it), then the lines around the other
/// matches, exact and numbered, within the budget.
fn expand_definitions(matches: &[(String, usize, String)], root: &Path, budget: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    let mut seen = std::collections::HashSet::new();
    // Line spans already printed, so a window never repeats a definition body.
    let mut shown: Vec<(String, usize, usize)> = Vec::new();
    let mut parsed: std::collections::HashMap<String, Option<Parsed>> =
        std::collections::HashMap::new();
    let mut sources: std::collections::HashMap<String, Option<Vec<String>>> =
        std::collections::HashMap::new();
    for (path, line, text) in matches {
        let entry = parsed.entry(path.clone()).or_insert_with(|| {
            let full = root.join(path);
            let lang = crate::config::language(&full)?;
            let source = std::fs::read_to_string(&full).ok()?;
            let (symbols, _) = crate::parser::parse_source(&source, lang).ok()?;
            Some((source.lines().map(str::to_owned).collect(), symbols))
        });
        let Some((lines, symbols)) = entry else {
            continue;
        };
        // The definition that starts on the matched line (or just above it, for a
        // signature spread over a decorator or an export line).
        let Some(symbol) = symbols
            .iter()
            .filter(|symbol| matches!(symbol.kind.as_str(), "function" | "method"))
            .filter(|symbol| symbol.start <= *line && *line <= symbol.start + 2)
            // The matched line must be the definition itself, not a call just below it.
            .filter(|symbol| {
                text.contains(symbol.name.as_str())
                    && !text.trim_start().starts_with(&format!("{}(", symbol.name))
            })
            .min_by_key(|symbol| symbol.end - symbol.start)
        else {
            continue;
        };
        if !seen.insert((path.clone(), symbol.start)) {
            continue;
        }
        let end = symbol.end.min(lines.len());
        shown.push((path.clone(), symbol.start, end));
        let mut block = format!(
            "## {path} {}-{end} {} {}\n",
            symbol.start, symbol.kind, symbol.qualname
        );
        let mut cut = None;
        for number in symbol.start..=end {
            let text = format!("{number}\t{}\n", lines[number - 1]);
            if estimate_tokens(&block) + estimate_tokens(&text) > MAX_BODY_TOKENS {
                cut = Some(number);
                break;
            }
            block.push_str(&text);
        }
        if let Some(from) = cut {
            block.push_str(&format!("... {from}-{end} elided\n"));
        }
        let cost = estimate_tokens(&block);
        if used + cost > budget {
            break;
        }
        used += cost;
        out.push_str(&block);
    }
    let mut out = if out.is_empty() {
        out
    } else {
        format!("definitions found:\n{out}")
    };
    // Then the source around the other matches (a search is usually followed by
    // reading just that neighbourhood), while the budget lasts.
    let mut windows = String::new();
    for (path, line, _) in matches {
        if shown
            .iter()
            .any(|(p, from, to)| p == path && from <= line && line <= to)
        {
            continue;
        }
        let Some(lines) = sources.entry(path.clone()).or_insert_with(|| {
            std::fs::read_to_string(root.join(path))
                .ok()
                .map(|text| text.lines().map(str::to_owned).collect())
        }) else {
            continue;
        };
        if *line == 0 || *line > lines.len() {
            continue;
        }
        let from = line.saturating_sub(WINDOW_BEFORE).max(1);
        let to = (line + WINDOW_AFTER).min(lines.len());
        let mut block = format!("## {path} {from}-{to}\n");
        for number in from..=to {
            block.push_str(&format!("{number}\t{}\n", lines[number - 1]));
        }
        let cost = estimate_tokens(&block);
        if used + cost > budget {
            break;
        }
        used += cost;
        shown.push((path.clone(), from, to));
        windows.push_str(&block);
    }
    if !windows.is_empty() {
        out.push_str(&format!("around the matches:\n{windows}"));
    }
    out
}

/// One parsed `path:line:text` match line, or a `path-line-text` context line.
fn parse(line: &str) -> Option<(&str, &str, char, &str)> {
    parse_with(line, ':').or_else(|| parse_with(line, '-'))
}

/// Splits at the first `<sep><digits><sep>` after a non-empty path (a match line
/// may also close its number with `-`, as context lines do).
fn parse_with(line: &str, separator: char) -> Option<(&str, &str, char, &str)> {
    let bytes = line.as_bytes();
    let mut index = 0;
    while let Some(offset) = line[index..].find(separator) {
        let at = index + offset;
        let digits = bytes[at + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let after = at + 1 + digits;
        let closes = match separator {
            ':' => matches!(bytes.get(after), Some(b':') | Some(b'-')),
            _ => bytes.get(after) == Some(&b'-'),
        };
        if at > 0 && digits > 0 && closes {
            return Some((
                &line[..at],
                &line[at + 1..after],
                bytes[after] as char,
                &line[after + 1..],
            ));
        }
        index = at + 1;
    }
    None
}

fn shorten(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept} [+{} chars]", count - max_chars)
}

/// Compacts search output; see the module documentation.
pub fn compact(input: &str, options: CompactOptions) -> String {
    let mut out = Vec::new();
    let mut current: Option<&str> = None;
    let (mut omitted, mut omitted_paths) = (0usize, std::collections::BTreeSet::new());
    let mut matches = Vec::new();
    for line in input.lines() {
        // `--` only separates context groups; the line numbers already show the gaps.
        if line == "--" {
            continue;
        }
        let rendered = match parse(line) {
            Some((path, number, separator, text)) => {
                if separator == ':'
                    && let Ok(number) = number.parse::<usize>()
                {
                    matches.push((path.to_owned(), number, text.to_owned()));
                }
                let header = (current != Some(path)).then(|| path.to_owned());
                current = Some(path);
                (
                    header,
                    format!(
                        "{number}{separator}{}",
                        shorten(text.trim_end(), options.max_line_chars)
                    ),
                    Some(path),
                )
            }
            None => {
                current = None;
                (None, line.to_owned(), None)
            }
        };
        let needed = 1 + usize::from(rendered.0.is_some());
        if out.len() + needed > options.max_lines {
            omitted += 1;
            if let Some(path) = rendered.2 {
                omitted_paths.insert(path.to_owned());
            }
            continue;
        }
        if let Some(header) = rendered.0 {
            out.push(header);
        }
        out.push(rendered.1);
    }
    let mut text = out.join("\n");
    if omitted > 0 {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "... {omitted} more lines in {} files not shown; narrow the search",
            omitted_paths.len().max(1)
        ));
    }
    if !text.is_empty() {
        text.push('\n');
    }
    if options.expand_tokens > 0
        && let Some(root) = &options.root
    {
        text.push_str(&expand_definitions(&matches, root, options.expand_tokens));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_matches_by_path_and_keeps_numbers_and_text() {
        let input = "src/a.rs:3:fn a() {}\nsrc/a.rs:9:    a();\nsrc/b.rs:1:use a;\n";
        assert_eq!(
            compact(input, CompactOptions::default()),
            "src/a.rs\n3:fn a() {}\n9:    a();\nsrc/b.rs\n1:use a;\n"
        );
    }

    #[test]
    fn keeps_context_lines_and_drops_group_separators() {
        let input = "a.py-1-import os\na.py:2:def f():\n--\na.py:9:f()\n";
        assert_eq!(
            compact(input, CompactOptions::default()),
            "a.py\n1-import os\n2:def f():\n9:f()\n"
        );
    }

    #[test]
    fn a_path_with_colons_or_text_with_numbers_is_split_at_the_first_line_number() {
        assert_eq!(parse("C:12:x"), Some(("C", "12", ':', "x")));
        assert_eq!(
            parse("a:b/c.rs:7:let t = \"10:20\";"),
            Some(("a:b/c.rs", "7", ':', "let t = \"10:20\";"))
        );
        assert_eq!(parse("no line number here"), None);
        assert_eq!(parse(":5:x"), None);
    }

    #[test]
    fn unrecognised_lines_pass_through_unchanged() {
        let input = "src/a.rs\nsrc/b.rs\n3 matches\n";
        assert_eq!(compact(input, CompactOptions::default()), input);
    }

    #[test]
    fn long_lines_are_shortened_and_say_by_how_much() {
        let input = format!("a.js:1:{}\n", "x".repeat(300));
        let out = compact(
            &input,
            CompactOptions {
                max_lines: 10,
                max_line_chars: 100,
                ..CompactOptions::default()
            },
        );
        assert!(
            out.contains(&format!("1:{} [+200 chars]", "x".repeat(100))),
            "{out}"
        );
    }

    #[test]
    fn the_total_is_bounded_and_the_rest_is_counted() {
        let input: String = (1..=50).map(|n| format!("f{}.rs:{n}:x\n", n % 3)).collect();
        let out = compact(
            &input,
            CompactOptions {
                max_lines: 10,
                max_line_chars: 100,
                ..CompactOptions::default()
            },
        );
        assert!(out.lines().count() <= 11, "{out}");
        assert!(
            out.trim_end().ends_with("not shown; narrow the search"),
            "{out}"
        );
    }

    #[test]
    fn the_body_of_a_matched_definition_is_appended_within_budget() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.py"),
            "import os\n\ndef target(a):\n    b = a + 1\n    return b\n\ndef other():\n    target(2)\n",
        )
        .unwrap();
        let input = "a.py:3:def target(a):\na.py:8:    target(2)\n";
        let options = CompactOptions {
            expand_tokens: 500,
            root: Some(dir.path().to_path_buf()),
            ..CompactOptions::default()
        };
        let out = compact(input, options.clone());
        assert!(out.contains("definitions found:\n## a.py 3-5 function target\n3\tdef target(a):\n4\t    b = a + 1\n5\t    return b\n"), "{out}");
        assert!(!out.contains("function other"), "{out}");
        let none = compact(
            input,
            CompactOptions {
                expand_tokens: 0,
                ..options
            },
        );
        assert!(!none.contains("definitions found"), "{none}");
    }

    #[test]
    fn the_lines_around_other_matches_follow_once_within_budget() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();
        let options = CompactOptions {
            expand_tokens: 500,
            root: Some(dir.path().to_path_buf()),
            ..CompactOptions::default()
        };
        // Line 20 falls inside the window of line 15, so it adds nothing.
        let out = compact("a.txt:15:line 15\na.txt:20:line 20\n", options.clone());
        assert!(
            out.contains("around the matches:\n## a.txt 7-45\n7\tline 7\n"),
            "{out}"
        );
        assert_eq!(out.matches("## a.txt").count(), 1, "{out}");
        let tight = compact(
            "a.txt:15:line 15\n",
            CompactOptions {
                expand_tokens: 20,
                ..options
            },
        );
        assert!(!tight.contains("around the matches"), "{tight}");
    }

    #[test]
    fn empty_input_prints_nothing() {
        assert_eq!(compact("", CompactOptions::default()), "");
    }
}
