use regex::Regex;
use serde::Serialize;

use crate::model::{Envelope, Hit};

pub fn render_envelope_text(envelope: &Envelope, line_filter: Option<&Regex>) -> String {
    let mut groups: Vec<(String, Vec<&Hit>)> = Vec::new();
    for hit in &envelope.hits {
        if let Some((_, hits)) = groups.iter_mut().find(|(path, _)| path == &hit.path) {
            hits.push(hit);
        } else {
            groups.push((hit.path.clone(), vec![hit]));
        }
    }

    let mut lines = Vec::new();
    for (path, hits) in &groups {
        lines.push(path.clone());
        let mut previous_end: Option<usize> = None;

        for hit in hits {
            let mut hit_lines = Vec::new();
            let mut printed_range = None;

            if !hit.snippet.is_empty() && (is_content_kind(&hit.kind) || hit.kind == "call") {
                let printed = hit
                    .snippet
                    .lines()
                    .enumerate()
                    .filter_map(|(index, line)| {
                        if line_filter.is_some_and(|matcher| !matcher.is_match(line)) {
                            return None;
                        }
                        let number = hit.start + index;
                        hit_lines.push(format!("{number}:{}", line.trim_end()));
                        Some(number)
                    })
                    .collect::<Vec<_>>();
                if let (Some(first), Some(last)) = (printed.first(), printed.last()) {
                    printed_range = Some((*first, *last));
                }
            } else if hit.snippet.is_empty() {
                hit_lines.push(format!("{} {} {}", hit.start, hit.kind, hit.why));
                printed_range = Some((hit.start, hit.start));
            } else if hit.symbol.is_some() && !is_content_kind(&hit.kind) {
                let sig = hit.sig.split_whitespace().collect::<Vec<_>>().join(" ");
                hit_lines.push(format!(
                    "{}-{} {} {}: {}",
                    hit.start,
                    hit.end,
                    hit.kind,
                    hit.symbol.as_deref().unwrap_or_default(),
                    sig
                ));
                printed_range = Some((hit.start, hit.end));
            } else {
                hit_lines.push(format!("{} {} {}", hit.start, hit.kind, hit.why));
                printed_range = Some((hit.start, hit.start));
            }

            if hit.snippet_truncated {
                let number = hit.start + hit.snippet.lines().count();
                hit_lines.push(format!(
                    "{number}: ... (truncated, read lines {}-{})",
                    hit.start, hit.end
                ));
                printed_range = Some(match printed_range {
                    Some((start, end)) => (start, end.max(number)),
                    None => (number, number),
                });
            }

            if let (Some((start, end)), Some(previous)) = (printed_range, previous_end) {
                if start > previous.saturating_add(1) {
                    lines.push("--".to_owned());
                }
                previous_end = Some(previous.max(end));
            } else if let Some((_, end)) = printed_range {
                previous_end = Some(end);
            }
            lines.extend(hit_lines);
        }
    }

    let body = lines.join("\n");
    let printed_chars = if body.is_empty() {
        0
    } else {
        body.chars().count() + 1
    };
    let mut output = lines;
    output.push(format!(
        "{} hits in {} files; ~{} tokens printed; coverage={}",
        envelope.hits.len(),
        groups.len(),
        printed_chars.div_ceil(4),
        envelope.coverage
    ));
    if let Some(hint) = &envelope.hint {
        output.push(format!("hint: {hint}"));
    }
    output.join("\n")
}

pub fn render_json<T: Serialize>(value: &T, pretty: bool) -> anyhow::Result<String> {
    if pretty {
        Ok(serde_json::to_string_pretty(value)?)
    } else {
        Ok(serde_json::to_string(value)?)
    }
}

fn is_content_kind(kind: &str) -> bool {
    matches!(kind, "text" | "test" | "doc" | "config")
}

#[cfg(test)]
mod tests {
    use super::{render_envelope_text, render_json};
    use crate::model::{Envelope, Hit};
    use regex::Regex;

    fn hit(path: &str, start: usize, end: usize, kind: &str, snippet: &str) -> Hit {
        Hit {
            path: path.to_owned(),
            start,
            end,
            symbol: None,
            kind: kind.to_owned(),
            sig: String::new(),
            snippet: snippet.to_owned(),
            snippet_truncated: false,
            score: 1.0,
            why: "matched".to_owned(),
        }
    }

    fn envelope(hits: Vec<Hit>) -> Envelope {
        Envelope {
            hits,
            tokens: 0,
            freshness_ms: 12,
            coverage: "complete".to_owned(),
            hint: None,
        }
    }

    #[test]
    fn groups_interleaved_paths_by_first_occurrence() {
        let output = render_envelope_text(
            &envelope(vec![
                hit("a.rs", 1, 1, "text", "first"),
                hit("b.rs", 3, 3, "text", "other"),
                hit("a.rs", 8, 8, "text", "last"),
            ]),
            None,
        );

        assert_eq!(
            output.lines().take(7).collect::<Vec<_>>(),
            [
                "a.rs",
                "1:first",
                "--",
                "8:last",
                "b.rs",
                "3:other",
                "3 hits in 2 files; ~9 tokens printed; coverage=complete"
            ]
        );
        assert_eq!(output.matches("a.rs").count(), 1);
    }

    #[test]
    fn line_filter_keeps_only_matching_snippet_lines_and_trims_their_ends() {
        let matcher = Regex::new("needle").unwrap();
        let output = render_envelope_text(
            &envelope(vec![hit(
                "src.rs",
                10,
                13,
                "text",
                "before  \nneedle here  \nafter\nneedle again\t",
            )]),
            Some(&matcher),
        );

        assert!(output.contains("11:needle here\n13:needle again"));
        assert!(!output.contains("before"));
        assert!(!output.contains("after"));
    }

    #[test]
    fn symbol_hit_prints_its_range_and_single_line_signature() {
        let mut symbol = hit("src.rs", 2, 5, "def", "body");
        symbol.symbol = Some("run".to_owned());
        symbol.sig = "fn run(\n    value: u8\n)".to_owned();
        let output = render_envelope_text(&envelope(vec![symbol]), None);

        assert!(output.contains("2-5 def run: fn run( value: u8 )"));
    }

    #[test]
    fn empty_snippet_prints_its_reason() {
        let output = render_envelope_text(&envelope(vec![hit("src.rs", 7, 9, "ref", "")]), None);

        assert!(output.contains("7 ref matched"));
    }

    #[test]
    fn truncated_snippet_marks_the_next_line_and_read_range() {
        let mut excerpt = hit("src.rs", 4, 10, "text", "first\nsecond");
        excerpt.snippet_truncated = true;
        let output = render_envelope_text(&envelope(vec![excerpt]), None);

        assert!(output.contains("6: ... (truncated, read lines 4-10)"));
    }

    #[test]
    fn footer_counts_hits_files_and_printed_tokens() {
        let output = render_envelope_text(&envelope(vec![hit("a", 1, 1, "text", "abcd")]), None);

        assert!(output.ends_with("1 hits in 1 files; ~3 tokens printed; coverage=complete"));
    }

    #[test]
    fn json_can_be_compact_or_pretty() {
        let value = serde_json::json!({"key": [1, 2]});

        assert_eq!(render_json(&value, false).unwrap(), r#"{"key":[1,2]}"#);
        assert_eq!(
            render_json(&value, true).unwrap(),
            "{\n  \"key\": [\n    1,\n    2\n  ]\n}"
        );
    }
}
