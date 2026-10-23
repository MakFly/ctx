//! `ctx compact`: a stdin filter for search output (`path:line:text`, as printed by
//! `rg -n`, `grep -n` and `git grep -n`). It prints each path once, keeps every
//! line's number and text, shortens very long lines and bounds the total, saying
//! what it left out. Lines it does not recognise pass through unchanged.

/// Limits applied by `compact`.
#[derive(Clone, Copy, Debug)]
pub struct CompactOptions {
    /// Output lines kept before the rest is summarised.
    pub max_lines: usize,
    /// Characters of a match's text kept before it is shortened.
    pub max_line_chars: usize,
}

impl Default for CompactOptions {
    fn default() -> Self {
        Self {
            max_lines: 250,
            max_line_chars: 240,
        }
    }
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
    for line in input.lines() {
        // `--` only separates context groups; the line numbers already show the gaps.
        if line == "--" {
            continue;
        }
        let rendered = match parse(line) {
            Some((path, number, separator, text)) => {
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
            },
        );
        assert!(out.lines().count() <= 11, "{out}");
        assert!(
            out.trim_end().ends_with("not shown; narrow the search"),
            "{out}"
        );
    }

    #[test]
    fn empty_input_prints_nothing() {
        assert_eq!(compact("", CompactOptions::default()), "");
    }
}
