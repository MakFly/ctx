use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::language;
use crate::parser::parse_source;

const SUPPORTED_EXTENSIONS: &str =
    "py, pyi, pyw, js, jsx, mjs, cjs, ts, mts, cts, vue, svelte, astro, tsx, go, rs, php, phtml";
const FALLBACK_OUTLINE_LINES: usize = 40;

#[derive(Debug, Clone)]
pub struct ReadOptions {
    pub lines: Option<(usize, usize)>,
    pub symbol: Option<String>,
    pub outline: bool,
    pub budget_tokens: usize,
    pub raw: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadOutput {
    pub path: String,
    pub lang: Option<String>,
    pub total_lines: usize,
    pub mode: String,
    pub blocks: Vec<ReadBlock>,
    pub elided: Vec<(usize, usize)>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadBlock {
    pub start: usize,
    pub end: usize,
    pub kind: Option<String>,
    pub name: Option<String>,
    pub text: String,
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

pub fn read_file(path: &Path, options: &ReadOptions) -> Result<ReadOutput> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        bail!("binary file: {}", path.display());
    }
    let source = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("binary file: {} is not valid UTF-8", path.display()))?;
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let lang = language(path);
    let mut output = ReadOutput {
        path: path.display().to_string(),
        lang: lang.map(str::to_owned),
        total_lines: lines.len(),
        mode: "full".to_owned(),
        blocks: Vec::new(),
        elided: Vec::new(),
        note: None,
    };

    if let Some((start, end)) = options.lines {
        if start == 0 || start > end {
            bail!("invalid line range {start}:{end}: lines are 1-based and A must not exceed B");
        }
        if start > lines.len() {
            bail!(
                "line range starts at {start} but {} has {} lines",
                path.display(),
                lines.len()
            );
        }
        let end = end.min(lines.len());
        output.mode = "lines".to_owned();
        output.blocks.push(ReadBlock {
            start,
            end,
            kind: None,
            name: None,
            text: lines[start - 1..end].concat(),
        });
    } else if let Some(name) = &options.symbol {
        let Some(lang) = lang else {
            bail!(
                "cannot resolve symbols in {}: unsupported language (supported extensions: {SUPPORTED_EXTENSIONS}); use --lines A:B instead",
                path.display()
            );
        };
        let (mut symbols, _) = parse_source(&source, lang)?;
        symbols.retain(|symbol| symbol.name == *name || symbol.qualname == *name);
        if symbols.is_empty() {
            bail!(
                "symbol not found: {name} (ctx read {} --outline lists the definitions)",
                path.display()
            );
        }
        symbols.sort_by_key(|symbol| symbol.start);
        output.mode = "symbol".to_owned();
        for symbol in symbols {
            let start = symbol.start.clamp(1, lines.len());
            let end = symbol.end.clamp(start, lines.len());
            let mut kept = end;
            let mut chars = 0;
            for (offset, line) in lines[start - 1..end].iter().enumerate() {
                chars += line.chars().count();
                if offset > 0 && chars.div_ceil(4) > options.budget_tokens {
                    kept = start + offset - 1;
                    break;
                }
            }
            if kept < end {
                output.elided.push((kept + 1, end));
            }
            output.blocks.push(ReadBlock {
                start,
                end: kept,
                kind: Some(symbol.kind),
                name: Some(symbol.qualname),
                text: lines[start - 1..kept].concat(),
            });
        }
    } else if options.outline || estimate_tokens(&source) > options.budget_tokens {
        output.mode = "outline".to_owned();
        if !options.outline {
            output.note = Some(format!(
                "file has {} lines (~{} tokens), over the {}-token budget: outline shown; use --lines A:B or --symbol NAME",
                lines.len(),
                estimate_tokens(&source),
                options.budget_tokens
            ));
        }
        fill_outline(&mut output, &source, &lines, lang)?;
    } else if !lines.is_empty() {
        output.blocks.push(ReadBlock {
            start: 1,
            end: lines.len(),
            kind: None,
            name: None,
            text: source.clone(),
        });
    }
    Ok(output)
}

fn fill_outline(
    output: &mut ReadOutput,
    source: &str,
    lines: &[&str],
    lang: Option<&str>,
) -> Result<()> {
    let Some(lang) = lang else {
        let shown = lines.len().min(FALLBACK_OUTLINE_LINES);
        if shown > 0 {
            output.blocks.push(ReadBlock {
                start: 1,
                end: shown,
                kind: None,
                name: None,
                text: lines[..shown].concat(),
            });
        }
        if shown < lines.len() {
            output.elided.push((shown + 1, lines.len()));
        }
        let message = format!(
            "no outline for this language (supported extensions: {SUPPORTED_EXTENSIONS}): showing the first {shown} lines"
        );
        output.note = Some(match output.note.take() {
            Some(previous) => format!("{previous}; {message}"),
            None => message,
        });
        return Ok(());
    };
    let (mut symbols, _) = parse_source(source, lang)?;
    symbols.sort_by_key(|symbol| (symbol.start, std::cmp::Reverse(symbol.end)));
    let mut shown = vec![false; lines.len() + 1];
    for symbol in symbols {
        let start = symbol.start.clamp(1, lines.len().max(1));
        let end = symbol.end.max(start);
        if start <= lines.len() {
            shown[start] = true;
        }
        output.blocks.push(ReadBlock {
            start,
            end,
            kind: Some(symbol.kind),
            name: Some(symbol.qualname),
            text: symbol.sig,
        });
    }
    let mut gap_start = None;
    for line in 1..=lines.len() {
        match (shown[line], gap_start) {
            (false, None) => gap_start = Some(line),
            (true, Some(first)) => {
                output.elided.push((first, line - 1));
                gap_start = None;
            }
            _ => {}
        }
    }
    if let Some(first) = gap_start {
        output.elided.push((first, lines.len()));
    }
    Ok(())
}

fn push_numbered(out: &mut String, block: &ReadBlock) {
    for (offset, line) in block.text.split_inclusive('\n').enumerate() {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.push_str(&format!("{}\t{line}\n", block.start + offset));
    }
}

pub fn render_read_text(output: &ReadOutput, raw: bool) -> String {
    let mut out = String::new();
    if raw {
        for block in &output.blocks {
            out.push_str(&block.text);
            if output.mode == "outline" && block.kind.is_some() {
                out.push('\n');
            }
        }
        return out;
    }
    out.push_str(&format!(
        "{} ({} lines, {})\n",
        output.path,
        output.total_lines,
        output.lang.as_deref().unwrap_or("text")
    ));
    // Blocks sort before elided ranges at the same line, so an outline entry precedes the gap after it.
    let mut items = output
        .blocks
        .iter()
        .map(|block| (block.start, 0, Some(block), (0, 0)))
        .chain(output.elided.iter().map(|range| (range.0, 1, None, *range)))
        .collect::<Vec<_>>();
    items.sort_by_key(|item| (item.0, item.1));
    for (_, _, block, range) in items {
        let Some(block) = block else {
            out.push_str(&format!("... {}-{} elided\n", range.0, range.1));
            continue;
        };
        let kind = block.kind.as_deref().unwrap_or_default();
        let name = block.name.as_deref().unwrap_or_default();
        if output.mode == "outline" && block.kind.is_some() {
            out.push_str(&format!(
                "{}-{} {kind} {name}  {}\n",
                block.start, block.end, block.text
            ));
            continue;
        }
        if output.mode == "symbol" {
            out.push_str(&format!("## {}-{} {kind} {name}\n", block.start, block.end));
        }
        push_numbered(&mut out, block);
    }
    if let Some(note) = &output.note {
        out.push_str(&format!("note: {note}\n"));
    }
    out
}

/// What a raw rendering leaves out (elided ranges and the note), meant for stderr.
pub fn render_read_notes(output: &ReadOutput) -> String {
    let mut out = String::new();
    for (start, end) in &output.elided {
        out.push_str(&format!("... {start}-{end} elided\n"));
    }
    if let Some(note) = &output.note {
        out.push_str(&format!("note: {note}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, content: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(name);
        fs::write(&path, content).unwrap();
        (directory, path)
    }

    fn options() -> ReadOptions {
        ReadOptions {
            lines: None,
            symbol: None,
            outline: false,
            budget_tokens: 2_000,
            raw: false,
        }
    }

    fn by_symbol(name: &str) -> ReadOptions {
        ReadOptions {
            symbol: Some(name.to_owned()),
            ..options()
        }
    }

    fn long_body(open: &str, line: &str, close: &str) -> String {
        let mut body = format!("{open}\n");
        for index in 0..120 {
            body.push_str(&line.replace("{i}", &index.to_string()));
            body.push('\n');
        }
        body.push_str(close);
        body.push('\n');
        body
    }

    #[test]
    fn raw_lines_are_byte_identical_to_the_source_slice() {
        let (_dir, path) = write("a.txt", b"one\ntwo\nthree\nfour\n");
        let output = read_file(
            &path,
            &ReadOptions {
                lines: Some((2, 3)),
                raw: true,
                ..options()
            },
        )
        .unwrap();
        assert_eq!(output.mode, "lines");
        assert_eq!(render_read_text(&output, true), "two\nthree\n");
    }

    #[test]
    fn raw_lines_keep_crlf_and_numbered_mode_strips_it() {
        let (_dir, path) = write("a.txt", b"one\r\ntwo\r\nthree");
        let output = read_file(
            &path,
            &ReadOptions {
                lines: Some((2, 3)),
                ..options()
            },
        )
        .unwrap();
        assert_eq!(render_read_text(&output, true), "two\r\nthree");
        assert_eq!(
            render_read_text(&output, false),
            format!("{} (3 lines, text)\n2\ttwo\n3\tthree\n", output.path)
        );
    }

    #[test]
    fn lines_clamp_the_end_and_reject_bad_ranges() {
        let (_dir, path) = write("a.txt", b"one\ntwo\n");
        let output = read_file(
            &path,
            &ReadOptions {
                lines: Some((2, 99)),
                ..options()
            },
        )
        .unwrap();
        assert_eq!((output.blocks[0].start, output.blocks[0].end), (2, 2));
        for range in [(3, 2), (0, 2), (5, 6)] {
            let result = read_file(
                &path,
                &ReadOptions {
                    lines: Some(range),
                    ..options()
                },
            );
            assert!(result.is_err(), "{range:?}");
        }
    }

    #[test]
    fn symbol_returns_the_full_body_in_every_language() {
        let cases = [
            (
                "a.ts",
                format!(
                    "// before\n{}function after() {{}}\n",
                    long_body(
                        "export function target(a: number) {",
                        "  const value{i} = a + {i}; // padding padding padding",
                        "}",
                    )
                ),
            ),
            (
                "a.rs",
                format!(
                    "// before\n{}fn after() {{}}\n",
                    long_body(
                        "pub fn target(a: i32) -> i32 {",
                        "    let value{i} = a + {i}; // padding padding padding",
                        "}",
                    )
                ),
            ),
            (
                "a.py",
                format!(
                    "# before\n{}\ndef after():\n    pass\n",
                    long_body(
                        "def target(a):",
                        "    value{i} = a + {i}  # padding padding padding",
                        "",
                    )
                ),
            ),
            (
                "a.go",
                format!(
                    "package main\n\n{}func after() {{}}\n",
                    long_body(
                        "func target(a int) int {",
                        "\tvalue{i} := a + {i} // padding padding padding",
                        "}",
                    )
                ),
            ),
            (
                "a.php",
                format!(
                    "<?php\n{}function after() {{}}\n",
                    long_body(
                        "function target($a) {",
                        "    $value{i} = $a + {i}; // padding padding padding",
                        "}",
                    )
                ),
            ),
        ];
        for (name, source) in cases {
            let (_dir, path) = write(name, source.as_bytes());
            let output = read_file(&path, &by_symbol("target")).unwrap();
            assert_eq!(output.mode, "symbol");
            assert_eq!(output.blocks.len(), 1, "{name}");
            assert!(output.elided.is_empty(), "{name}");
            let block = &output.blocks[0];
            let expected = source
                .split_inclusive('\n')
                .skip(block.start - 1)
                .take(block.end - block.start + 1)
                .collect::<String>();
            assert_eq!(block.text, expected, "{name}");
            assert!(block.text.len() > 2_000, "{name}");
            assert!(block.text.contains("value119"), "{name}");
            assert!(!block.text.contains("after"), "{name}");
        }
    }

    #[test]
    fn symbol_matches_a_method_by_qualname() {
        let source = "class Repo:\n    def save(self):\n        return 1\n\nclass Cache:\n    def save(self):\n        return 2\n";
        let (_dir, path) = write("a.py", source.as_bytes());
        let both = read_file(&path, &by_symbol("save")).unwrap();
        assert_eq!(both.blocks.len(), 2);
        let one = read_file(&path, &by_symbol("Cache.save")).unwrap();
        assert_eq!(one.blocks.len(), 1);
        assert_eq!(one.blocks[0].start, 6);
        assert_eq!(one.blocks[0].name.as_deref(), Some("Cache.save"));
        assert_eq!(one.blocks[0].kind.as_deref(), Some("method"));
        assert!(
            render_read_text(&one, false)
                .contains("## 6-7 method Cache.save\n6\t    def save(self):")
        );
    }

    #[test]
    fn symbol_not_found_and_unsupported_language_are_errors() {
        let (_dir, path) = write("a.py", b"def one():\n    pass\n");
        let error = read_file(&path, &by_symbol("missing"))
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("symbol not found: missing (ctx read "),
            "{error}"
        );
        assert!(error.contains("--outline lists the definitions)"));
        let (_dir, text) = write("a.txt", b"hello\n");
        let error = read_file(&text, &by_symbol("hello"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("--lines") && error.contains("rs"), "{error}");
    }

    #[test]
    fn symbol_over_budget_keeps_leading_lines_and_elides_the_rest() {
        let body = long_body("def target(a):", "    value{i} = a + {i}", "");
        let (_dir, path) = write("a.py", body.as_bytes());
        let output = read_file(
            &path,
            &ReadOptions {
                budget_tokens: 50,
                ..by_symbol("target")
            },
        )
        .unwrap();
        let block = &output.blocks[0];
        assert!(estimate_tokens(&block.text) <= 50);
        assert!(block.end < 121);
        assert_eq!(output.elided, vec![(block.end + 1, 121)]);
        assert!(
            render_read_text(&output, false).contains(&format!("... {}-121 elided", block.end + 1))
        );
    }

    #[test]
    fn outline_lists_symbols_in_order_with_elided_gaps() {
        let source = "import os\n\nclass A:\n    def one(self):\n        return 1\n\n    def two(self):\n        return 2\n\ndef free():\n    pass\n";
        let (_dir, path) = write("a.py", source.as_bytes());
        let output = read_file(
            &path,
            &ReadOptions {
                outline: true,
                ..options()
            },
        )
        .unwrap();
        assert_eq!(output.mode, "outline");
        let listed = output
            .blocks
            .iter()
            .map(|block| (block.name.clone().unwrap(), block.start, block.end))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            vec![
                ("A".to_owned(), 3, 8),
                ("A.one".to_owned(), 4, 5),
                ("A.two".to_owned(), 7, 8),
                ("free".to_owned(), 10, 11),
            ]
        );
        assert_eq!(output.blocks[1].text, "def one(self):");
        assert_eq!(output.elided, vec![(1, 2), (5, 6), (8, 9), (11, 11)]);
        let expected = format!(
            "{} (11 lines, python)\n... 1-2 elided\n3-8 class A  class A:\n4-5 method A.one  def one(self):\n... 5-6 elided\n7-8 method A.two  def two(self):\n... 8-9 elided\n10-11 function free  def free():\n... 11-11 elided\n",
            output.path
        );
        assert_eq!(render_read_text(&output, false), expected);
    }

    #[test]
    fn no_flag_switches_to_outline_over_budget() {
        let source = long_body("def target(a):", "    value{i} = a + {i}", "");
        let (_dir, path) = write("a.py", source.as_bytes());
        let small = read_file(&path, &options()).unwrap();
        assert_eq!(small.mode, "full");
        assert_eq!(small.blocks.len(), 1);
        assert_eq!(small.blocks[0].text, source);
        assert!(small.note.is_none());
        let tokens = estimate_tokens(&source);
        let over = read_file(
            &path,
            &ReadOptions {
                budget_tokens: 100,
                ..options()
            },
        )
        .unwrap();
        assert_eq!(over.mode, "outline");
        assert_eq!(
            over.note.unwrap(),
            format!(
                "file has 122 lines (~{tokens} tokens), over the 100-token budget: outline shown; use --lines A:B or --symbol NAME"
            )
        );
    }

    #[test]
    fn unsupported_extension_outline_falls_back_to_leading_lines() {
        let source = (1..=60).map(|n| format!("row {n}\n")).collect::<String>();
        let (_dir, path) = write("a.txt", source.as_bytes());
        let output = read_file(
            &path,
            &ReadOptions {
                outline: true,
                ..options()
            },
        )
        .unwrap();
        assert_eq!(output.mode, "outline");
        assert_eq!((output.blocks[0].start, output.blocks[0].end), (1, 40));
        assert_eq!(output.elided, vec![(41, 60)]);
        assert!(output.note.is_some());
        let text = render_read_text(&output, false);
        assert!(text.contains("40\trow 40\n... 41-60 elided\n"));
    }

    #[test]
    fn binary_and_missing_files_are_errors() {
        let (_dir, path) = write("a.bin", b"abc\0def");
        let error = read_file(&path, &options()).unwrap_err().to_string();
        assert!(error.contains("binary file"), "{error}");
        let error = read_file(&path.with_file_name("nope.txt"), &options())
            .unwrap_err()
            .to_string();
        assert!(error.contains("nope.txt"), "{error}");
    }
}
