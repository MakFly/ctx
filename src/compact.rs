//! `ctx compact`: a stdin filter for search output (`path:line:text`, as printed by
//! `rg -n`, `grep -n` and `git grep -n`; the hook only routes `rg` and `grep` here). It prints each path once, keeps every
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
    /// Whether a bare `--` line may be a context-group separator (the search asked
    /// for context lines). In `--null` output any other line without a NUL is part of
    /// a file name holding a newline.
    pub context_separators: bool,
}

impl Default for CompactOptions {
    fn default() -> Self {
        Self {
            max_lines: 250,
            max_line_chars: 240,
            expand_tokens: 0,
            root: None,
            context_separators: true,
        }
    }
}

/// Largest single definition body shown in full, in tokens.
const MAX_BODY_TOKENS: usize = 700;
/// Tokens kept for the line that says a body was cut.
const ELIDED_TOKENS: usize = 8;
/// Lines shown before a match, and after a lone one.
const WINDOW_BEFORE: usize = 8;
const WINDOW_AFTER: usize = 30;
/// Matches at most this many lines apart share one window.
const CLUSTER_GAP: usize = 15;
/// Longest window shown around a group of matches.
const MAX_WINDOW_LINES: usize = 80;

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// A file's lines and the symbols parsed from it (none for an unsupported language).
struct Source {
    lines: Vec<String>,
    symbols: Vec<crate::model::Symbol>,
}

/// Largest source file read to show its lines; a bigger one gets matches only.
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

fn load(root: &Path, path: &str) -> Option<Source> {
    let full = root.join(path);
    if std::fs::metadata(&full).ok()?.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&full).ok()?;
    let symbols = crate::config::language(&full)
        .and_then(|lang| crate::parser::parse_source(&text, lang).ok())
        .map(|(symbols, _)| symbols)
        .unwrap_or_default();
    Some(Source {
        lines: text.lines().map(str::to_owned).collect(),
        symbols,
    })
}

/// A span of source worth showing: a matched definition, or the window around a
/// group of nearby matches.
type Sources = std::collections::HashMap<String, Option<Source>>;

/// The innermost function or method holding a line.
fn enclosing(source: &Source, line: usize) -> Option<&crate::model::Symbol> {
    source
        .symbols
        .iter()
        .filter(|symbol| matches!(symbol.kind.as_str(), "function" | "method"))
        .filter(|symbol| symbol.start <= line && line <= symbol.end)
        .min_by_key(|symbol| symbol.end - symbol.start)
}

struct Candidate {
    path: String,
    from: usize,
    to: usize,
    /// `kind qualname` for a definition, `None` for a window.
    label: Option<String>,
    order: usize,
}

/// The definition whose first lines a match is (or just above it, for a signature
/// spread over a decorator or an export line); not a call just below it.
fn definition<'a>(source: &'a Source, line: usize, text: &str) -> Option<&'a crate::model::Symbol> {
    source
        .symbols
        .iter()
        .filter(|symbol| matches!(symbol.kind.as_str(), "function" | "method"))
        .filter(|symbol| symbol.start <= line && line <= symbol.start + 2)
        .filter(|symbol| {
            text.contains(symbol.name.as_str())
                && !text.trim_start().starts_with(&format!("{}(", symbol.name))
        })
        .min_by_key(|symbol| symbol.end - symbol.start)
}

/// The source the search points at (a search is usually followed by reading it):
/// matched definitions and windows around groups of nearby matches, the spans
/// holding the most matches first, exact and numbered, within the budget.
fn expand_definitions(
    matches: &[(String, usize, String)],
    root: &Path,
    budget: usize,
    sources: &mut Sources,
) -> String {
    let mut paths: Vec<&String> = Vec::new();
    for (path, _, _) in matches {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut numbers_by_path: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for path in paths {
        let Some(source) = sources
            .entry(path.clone())
            .or_insert_with(|| load(root, path))
        else {
            continue;
        };
        let length = source.lines.len();
        let mut numbers: Vec<usize> = matches
            .iter()
            .filter(|(p, line, _)| p == path && (1..=length).contains(line))
            .map(|(_, line, _)| *line)
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        let mut defined = Vec::new();
        for (_, line, text) in matches.iter().filter(|(p, ..)| p == path) {
            let Some(symbol) = definition(source, *line, text) else {
                continue;
            };
            if defined.contains(&symbol.start) {
                continue;
            }
            defined.push(symbol.start);
            candidates.push(Candidate {
                path: path.clone(),
                from: symbol.start,
                to: symbol.end.min(length),
                label: Some(format!("{} {}", symbol.kind, symbol.qualname)),
                order: candidates.len(),
            });
        }
        let mut index = 0;
        while index < numbers.len() {
            let first = numbers[index];
            let mut last = first;
            let mut count = 1;
            let start = first.saturating_sub(WINDOW_BEFORE).max(1);
            // A group grows while its window still fits: a longer run starts a new one.
            while index + count < numbers.len()
                && numbers[index + count] <= last + CLUSTER_GAP
                && numbers[index + count] + WINDOW_BEFORE < start + MAX_WINDOW_LINES
            {
                last = numbers[index + count];
                count += 1;
            }
            index += count;
            let from = first.saturating_sub(WINDOW_BEFORE).max(1);
            let to = if count == 1 {
                first + WINDOW_AFTER
            } else {
                last + WINDOW_BEFORE
            };
            candidates.push(Candidate {
                path: path.clone(),
                from,
                to: to.min(from + MAX_WINDOW_LINES - 1).min(length),
                label: None,
                order: candidates.len(),
            });
        }
        numbers_by_path.insert(path.clone(), numbers);
    }
    // The two section headings count against the budget too.
    let mut used = estimate_tokens("definitions found:\naround the matches:\n");
    let mut chosen: Vec<(usize, bool, String)> = Vec::new();
    let mut spans: Vec<(String, usize, usize)> = Vec::new();
    // Each candidate as it would be printed now: clipped against the spans already
    // shown, cut at the body cap, with the matches it still holds.
    let render = |candidate: &Candidate, spans: &[(String, usize, usize)], room: usize| {
        let (mut from, mut to) = (candidate.from, candidate.to);
        for (path, start, end) in spans {
            if *path != candidate.path || *end < from || *start > to {
                continue;
            }
            if *start <= from && to <= *end {
                return None;
            } else if *start <= from {
                from = end + 1;
            } else {
                to = start - 1;
            }
        }
        let lines = &sources[&candidate.path].as_ref()?.lines;
        let mut block = match &candidate.label {
            Some(label) => format!("## {} {from}-{to} {label}\n", candidate.path),
            None => format!("## {} {from}-{to}\n", candidate.path),
        };
        let mut cut = None;
        for number in from..=to {
            let text = format!("{number}\t{}\n", lines[number - 1]);
            // Room is kept for the `... A-B elided` line, so a cut body still fits.
            if estimate_tokens(&block) + estimate_tokens(&text) + ELIDED_TOKENS
                > MAX_BODY_TOKENS.min(room)
            {
                cut = Some(number);
                break;
            }
            block.push_str(&text);
        }
        if let Some(cut) = cut {
            block.push_str(&format!("... {cut}-{to} elided\n"));
            to = cut - 1;
        }
        let hits = numbers_by_path[&candidate.path]
            .iter()
            .filter(|line| (from..=to).contains(line))
            .count();
        (hits > 0 && from <= to).then_some((hits, from, to, block))
    };
    let mut remaining: Vec<Candidate> = candidates;
    loop {
        let best = remaining
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                render(candidate, &spans, budget.saturating_sub(used))
                    .map(|rendered| (index, rendered))
            })
            .max_by_key(|(index, (hits, ..))| (*hits, std::cmp::Reverse(remaining[*index].order)));
        let Some((index, (_, from, to, block))) = best else {
            break;
        };
        let candidate = remaining.swap_remove(index);
        let cost = estimate_tokens(&block);
        if used + cost > budget {
            continue;
        }
        used += cost;
        spans.push((candidate.path.clone(), from, to));
        chosen.push((candidate.order, candidate.label.is_some(), block));
    }
    chosen.sort_by_key(|(order, ..)| *order);
    let mut out = String::new();
    let definitions: String = chosen
        .iter()
        .filter(|(_, is_definition, _)| *is_definition)
        .map(|(.., block)| block.as_str())
        .collect();
    if !definitions.is_empty() {
        out.push_str(&format!("definitions found:\n{definitions}"));
    }
    let windows: String = chosen
        .iter()
        .filter(|(_, is_definition, _)| !*is_definition)
        .map(|(.., block)| block.as_str())
        .collect();
    if !windows.is_empty() {
        out.push_str(&format!("around the matches:\n{windows}"));
    }
    out
}

/// One parsed `path:line:text` match line, or a `path-line-text` context line.
/// The lines of `--null` search output. Every real line carries a NUL after its path,
/// but for a `--` separator when context was asked; any other line is the head of a
/// file name holding a newline, so it and the record that ends that name are shown as
/// they are, unattributed (the record's visible path is only the name's tail).
fn null_records(input: &str, context_separators: bool) -> Vec<(&str, bool)> {
    let mut broken = false;
    input
        .lines()
        .map(|line| {
            if line.contains('\0') {
                let literal = broken;
                broken = false;
                (line, literal)
            } else if context_separators && line == "--" {
                (line, false)
            } else {
                broken = true;
                (line, true)
            }
        })
        .collect()
}

#[cfg(test)]
fn parse(line: &str) -> Option<(&str, &str, char, &str)> {
    parse_in(line, None, None)
}

/// As [`parse`], choosing between the `path:N:` and `path-N-` readings of an ambiguous
/// line (`a.rs-1-t = "10:20"`): the one naming the file being listed, else a file that
/// exists under `root`, else the one whose delimiter comes first.
fn parse_in<'a>(
    line: &'a str,
    current: Option<&str>,
    root: Option<&Path>,
) -> Option<(&'a str, &'a str, char, &'a str)> {
    // `--null` output ends the path with a NUL: no guessing.
    if let Some((path, rest)) = line.split_once('\0') {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let separator = rest.as_bytes().get(digits).copied()?;
        return (!path.is_empty() && digits > 0 && matches!(separator, b':' | b'-')).then(|| {
            (
                path,
                &rest[..digits],
                separator as char,
                &rest[digits + 1..],
            )
        });
    }
    // Every split a path may end at: `a:1:b.rs:12:x` reads as `a` or as `a:1:b.rs`.
    let readings: Vec<_> = parse_with(line, ':')
        .into_iter()
        .chain(parse_with(line, '-'))
        .collect();
    if let Some(root) = root {
        let existing: Vec<_> = readings
            .iter()
            .filter(|(path, ..)| root.join(path).is_file())
            .collect();
        match existing.as_slice() {
            [one] => return Some(**one),
            // Two real files could own this line: it is left as it is, unattributed.
            [_, _, ..] => return None,
            [] => {}
        }
    }
    readings
        .iter()
        .find(|(path, ..)| Some(*path) == current)
        .or_else(|| readings.iter().min_by_key(|(path, ..)| path.len()))
        .copied()
}

/// Splits at the first `<sep><digits><sep>` after a non-empty path (a match line
/// may also close its number with `-`, as context lines do).
fn parse_with(line: &str, separator: char) -> Vec<(&str, &str, char, &str)> {
    let bytes = line.as_bytes();
    let mut readings = Vec::new();
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
            readings.push((
                &line[..at],
                &line[at + 1..after],
                bytes[after] as char,
                &line[after + 1..],
            ));
        }
        index = at + 1;
    }
    readings
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
    let mut sources = Sources::new();
    // With a root, each group of lines is preceded by the function holding it.
    // Even with no budget for source: a scope line costs a few tokens and often answers
    // "where is this called from" on its own.
    let scoped = options.root.as_deref();
    let mut scope: Option<usize> = None;
    // A NUL-separated file list (`-l --null`) is one record per name, each shown as is:
    // a name is never read as a match, and each counts against the line limit.
    let records: Vec<(&str, bool)> =
        match input.strip_suffix('\n').unwrap_or(input).strip_suffix('\0') {
            // Split on NUL alone: a listed name may itself hold a newline.
            Some(list) => list.split('\0').map(|name| (name, true)).collect(),
            None if input.contains('\0') => null_records(input, options.context_separators),
            None => input.lines().map(|line| (line, false)).collect(),
        };
    for (line, literal) in records {
        // `--` only separates context groups; the line numbers already show the gaps.
        if !literal && line == "--" {
            continue;
        }
        // Once a line is left out, so is every later one: count it, read nothing for it.
        if omitted > 0 {
            omitted += 1;
            if let Some((path, ..)) = (!literal)
                .then(|| parse_in(line, current, options.root.as_deref()))
                .flatten()
            {
                omitted_paths.insert(path.to_owned());
            }
            continue;
        }
        let rendered = match (!literal)
            .then(|| parse_in(line, current, options.root.as_deref()))
            .flatten()
        {
            Some((path, number, separator, text)) => {
                // Kept for the expansion only once the line is shown.
                let found = (separator == ':')
                    .then(|| number.parse::<usize>().ok())
                    .flatten()
                    .map(|number| (path.to_owned(), number, text.to_owned()));
                let header = (current != Some(path)).then(|| path.to_owned());
                // Display state moves only once these lines are accepted below.
                let mut next_scope = if header.is_some() { None } else { scope };
                let holder = scoped
                    .zip(number.parse::<usize>().ok())
                    .and_then(|(root, line)| {
                        let source = sources
                            .entry(path.to_owned())
                            .or_insert_with(|| load(root, path))
                            .as_ref()?;
                        enclosing(source, line).map(|symbol| {
                            (
                                symbol.start,
                                format!(
                                    "[{} {} {}-{}]",
                                    symbol.kind, symbol.qualname, symbol.start, symbol.end
                                ),
                            )
                        })
                    });
                let at_start = holder
                    .as_ref()
                    .is_some_and(|(start, _)| number.parse() == Ok(*start));
                let scope_line = match holder {
                    // A definition line names its function itself.
                    Some((start, _)) if at_start => {
                        next_scope = Some(start);
                        None
                    }
                    Some((start, label)) if next_scope != Some(start) => {
                        next_scope = Some(start);
                        Some(label)
                    }
                    Some(_) => None,
                    None => {
                        next_scope = None;
                        None
                    }
                };
                (
                    header,
                    scope_line,
                    format!(
                        "{number}{separator}{}",
                        shorten(text, options.max_line_chars)
                    ),
                    Some(path),
                    next_scope,
                    found,
                )
            }
            // `--null` on a count: the NUL stands for the colon.
            None => (None, None, line.replace('\0', ":"), None, None, None),
        };
        let needed = 1 + usize::from(rendered.0.is_some()) + usize::from(rendered.1.is_some());
        if out.len() + needed > options.max_lines {
            omitted += 1;
            if let Some(path) = rendered.3 {
                omitted_paths.insert(path.to_owned());
            }
            continue;
        }
        current = rendered.3;
        scope = rendered.4;
        matches.extend(rendered.5);
        if let Some(header) = rendered.0 {
            out.push(header);
        }
        if let Some(scope_line) = rendered.1 {
            out.push(scope_line);
        }
        out.push(rendered.2);
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
        text.push_str(&expand_definitions(
            &matches,
            root,
            options.expand_tokens,
            &mut sources,
        ));
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
            format!(
                "import os\n\ndef target(a):\n    b = a + 1\n    return b\n\n{}def other():\n    target(2)\n",
                "x = 1\n".repeat(30)
            ),
        )
        .unwrap();
        let input = "a.py:3:def target(a):\na.py:38:    target(2)\n";
        let options = CompactOptions {
            expand_tokens: 500,
            root: Some(dir.path().to_path_buf()),
            ..CompactOptions::default()
        };
        let out = compact(input, options.clone());
        assert!(out.contains("definitions found:\n## a.py 3-5 function target\n3\tdef target(a):\n4\t    b = a + 1\n5\t    return b\n"), "{out}");
        assert!(!out.contains("## a.py 37-38 function other"), "{out}");
        // The call is placed in its function; the definition line needs no such line.
        assert!(
            out.starts_with("a.py\n3:def target(a):\n[function other 37-38]\n38:    target(2)\n"),
            "{out}"
        );
        // The call is not a definition: the lines around it follow instead.
        assert!(
            out.contains("around the matches:\n## a.py 30-38\n"),
            "{out}"
        );
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
        // Lines 15 and 20 are close: one window from 8 before the first to 8 after the last.
        let out = compact("a.txt:15:line 15\na.txt:20:line 20\n", options.clone());
        assert!(
            out.contains("around the matches:\n## a.txt 7-28\n7\tline 7\n"),
            "{out}"
        );
        assert_eq!(out.matches("## a.txt").count(), 1, "{out}");
        let tight = compact(
            "a.txt:15:line 15\n",
            CompactOptions {
                expand_tokens: 20,
                ..options.clone()
            },
        );
        assert!(!tight.contains("around the matches"), "{tight}");
        // Under a tight budget the dense group wins over the lone first match.
        let dense = compact(
            "a.txt:2:line 2\na.txt:60:line 60\na.txt:62:line 62\na.txt:65:line 65\n",
            CompactOptions {
                expand_tokens: 150,
                ..options
            },
        );
        assert!(dense.contains("## a.txt 52-73\n52\tline 52\n"), "{dense}");
        // The lone match only gets what budget is left: cut, and said so.
        assert!(
            dense.contains("## a.txt 1-32\n") && dense.contains("elided"),
            "{dense}"
        );
    }

    #[test]
    fn a_context_line_holding_colons_stays_with_its_file() {
        let input = "a.rs:1:let s = 1;\na.rs-2-let t = \"10:20:30\";\n";
        assert_eq!(
            compact(input, CompactOptions::default()),
            "a.rs\n1:let s = 1;\n2-let t = \"10:20:30\";\n"
        );
    }

    #[test]
    fn a_path_holding_delimiters_is_read_whole_when_that_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a:1:b.rs"), "x\n").unwrap();
        assert_eq!(
            parse_in("a:1:b.rs:12:match", None, Some(dir.path())),
            Some(("a:1:b.rs", "12", ':', "match"))
        );
        assert_eq!(
            parse_in("a:1:b.rs:12:match", Some("a:1:b.rs"), None),
            Some(("a:1:b.rs", "12", ':', "match"))
        );
        // Two real files could own the line: it is left unattributed.
        std::fs::write(dir.path().join("a"), "x\n").unwrap();
        assert_eq!(
            parse_in("a:1:b.rs:12:match", Some("a"), Some(dir.path())),
            None
        );
        // A NUL ends the path (`--null`): no guessing at all.
        assert_eq!(
            parse_in("a:1:b.rs\u{0}12:match", Some("a"), Some(dir.path())),
            Some(("a:1:b.rs", "12", ':', "match"))
        );
        // With nothing to go on, the earliest delimiter wins.
        assert_eq!(
            parse_in("a:1:b.rs:12:match", None, None),
            Some(("a", "1", ':', "b.rs:12:match"))
        );
    }

    #[test]
    fn a_cut_body_still_fits_the_budget_it_was_cut_for() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=200)
            .map(|n| format!("    let value_{n} = {n};\n"))
            .collect();
        std::fs::write(dir.path().join("a.rs"), format!("fn long() {{\n{body}}}\n")).unwrap();
        for budget in [60, 120, 300, 600] {
            let out = compact(
                "a.rs:1:fn long() {\n",
                CompactOptions {
                    expand_tokens: budget,
                    root: Some(dir.path().to_path_buf()),
                    ..CompactOptions::default()
                },
            );
            assert!(
                out.contains("function long") && out.contains("elided"),
                "{budget}: {out}"
            );
        }
    }

    #[test]
    fn once_a_line_is_left_out_no_later_line_shows_under_the_wrong_file() {
        let input = "a.rs:1:x\na.rs:2:y\nb.rs:1:z\na.rs:3:w\n";
        let out = compact(
            input,
            CompactOptions {
                max_lines: 3,
                ..CompactOptions::default()
            },
        );
        assert_eq!(
            out,
            "a.rs\n1:x\n2:y\n... 2 more lines in 2 files not shown; narrow the search\n"
        );
    }

    #[test]
    fn a_file_name_holding_a_newline_is_never_attributed_to_its_tail() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "needle\n").unwrap();
        let options = CompactOptions {
            expand_tokens: 600,
            root: Some(dir.path().to_path_buf()),
            context_separators: false,
            ..CompactOptions::default()
        };
        // The real file is `prefix\nREADME.md`; its visible tail names another file.
        let out = compact("prefix\nREADME.md\u{0}1:needle\n", options.clone());
        assert_eq!(out, "prefix\nREADME.md:1:needle\n", "{out}");
        // Ordinary records still group, with a context separator when one was asked for.
        let normal = compact(
            "README.md\u{0}1:needle\n--\nREADME.md\u{0}1:needle\n",
            CompactOptions {
                context_separators: true,
                ..options
            },
        );
        assert!(normal.starts_with("README.md\n1:needle\n"), "{normal}");
    }

    #[test]
    fn a_null_separated_file_list_or_count_reads_as_without_null() {
        assert_eq!(
            compact("a.rs\u{0}b.rs\u{0}\n", CompactOptions::default()),
            "a.rs\nb.rs\n"
        );
        // A listed name is never read as a match, and each one counts against the limit.
        assert_eq!(
            compact(
                "a.rs\u{0}12:foo\u{0}a:1:b.rs\u{0}",
                CompactOptions::default()
            ),
            "a.rs\n12:foo\na:1:b.rs\n"
        );
        let many: String = (0..20).map(|n| format!("f{n}.rs\u{0}")).collect();
        let out = compact(
            &many,
            CompactOptions {
                max_lines: 5,
                ..CompactOptions::default()
            },
        );
        assert_eq!(out.lines().count(), 6, "{out}");
        // A name holding a newline is one record; a name `--` is kept.
        assert_eq!(
            compact("a\nname\u{0}--\u{0}b\u{0}", CompactOptions::default()),
            "a\nname\n--\nb\n"
        );
        assert_eq!(
            compact("a.rs\u{0}3\n", CompactOptions::default()),
            "a.rs:3\n"
        );
    }

    #[test]
    fn empty_input_prints_nothing() {
        assert_eq!(compact("", CompactOptions::default()), "");
    }
}
