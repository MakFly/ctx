use std::collections::HashSet;
use std::fs;
use std::path::Path;

use ctx_code::db::{connect, get_meta};
use ctx_code::indexer::index_repository;
use ctx_code::search::{search_index, search_index_with_options};
use ctx_code::text_index::{content_hits, matcher, open_generation, stage_generation};

fn generation(root: &Path) -> String {
    get_meta(
        &connect(&root.join(".ctx/index.sqlite"), false).unwrap(),
        "text_generation",
        "",
    )
    .unwrap()
}

#[test]
fn fts_source_markers_do_not_shift_the_returned_citation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    // Include both the normal marker and the first fallback marker.
    let source = format!(
        "protocol: \u{1} and \u{1}ctx-match\u{1}\n{}targetneedle\n",
        "padding\n".repeat(300),
    );
    fs::write(root.join("protocol.custom"), &source).unwrap();
    index_repository(root).unwrap();
    let result = search_index("targetneedle", "text", None, 10, 500, root).unwrap();
    let hit = result
        .hits
        .iter()
        .find(|hit| hit.path == "protocol.custom")
        .unwrap();
    assert_eq!((hit.start, hit.end), (300, 302));
    assert!(hit.snippet.contains("targetneedle"));
    assert_eq!(
        hit.snippet,
        source.lines().collect::<Vec<_>>()[hit.start - 1..hit.end].join("\n"),
    );
}

#[test]
fn full_text_finds_tail_content_and_large_unparsed_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let text = format!("{}\ntailneedle\n", "padding\n".repeat(140_000));
    fs::write(root.join("large.rs"), &text).unwrap();
    fs::write(
        root.join("opaque.custom"),
        format!("{}\notherneedle\n", "padding\n".repeat(600)),
    )
    .unwrap();
    index_repository(root).unwrap();
    for (needle, path) in [("tailneedle", "large.rs"), ("otherneedle", "opaque.custom")] {
        for mode in ["literal", "text"] {
            let result = search_index(needle, mode, None, 10, 500, root).unwrap();
            let hit = result.hits.iter().find(|hit| hit.path == path).unwrap();
            assert!(hit.snippet.contains(needle));
            assert!(hit.start > 1);
            let source = fs::read_to_string(root.join(path)).unwrap();
            let lines = source.lines().collect::<Vec<_>>();
            assert_eq!(hit.snippet, lines[hit.start - 1..hit.end].join("\n"));
        }
    }
    let db = connect(&root.join(".ctx/index.sqlite"), false).unwrap();
    let language: Option<String> = db
        .query_row("SELECT lang FROM files WHERE path='large.rs'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(language, None);
}

#[test]
fn trigram_candidates_match_direct_regex_scans_including_unicode_and_multiline() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let documents = [
        ("a.data", "foo xx bar\nStraße STRASSE\nKelvin Kelvin\nαβγ\n"),
        ("b.data", "prefix foobar\nnext line\nCAFÉ café\nabc\n"),
        ("empty", ""),
        ("short", "xy"),
    ];
    for (path, content) in documents {
        fs::write(root.join(path), content).unwrap();
    }
    index_repository(root).unwrap();
    let cases = [
        ("foo", "literal", false),
        ("xy", "literal", false),
        ("é", "literal", false),
        ("kelvin", "literal", true),
        ("café", "literal", true),
        ("foo.*bar", "regex", false),
        ("abc|xy", "regex", false),
        ("(?i)kelvin", "regex", false),
        ("(?s)foobar.*next", "regex", false),
        ("[α-ω]+", "regex", false),
        ("^$", "regex", false),
        ("not-present", "literal", false),
    ];
    for (pattern, mode, ci) in cases {
        let regex = matcher(pattern, mode == "literal", ci).unwrap();
        let mut expected = documents
            .iter()
            .flat_map(|(path, content)| content_hits(path, content, &regex, "text"))
            .map(|hit| (hit.path, hit.start, hit.end, hit.snippet))
            .collect::<Vec<_>>();
        expected.sort();
        let actual =
            search_index_with_options(pattern, mode, ci, None, 100, 100_000, root).unwrap();
        let mut actual = actual
            .hits
            .into_iter()
            .map(|hit| (hit.path, hit.start, hit.end, hit.snippet))
            .collect::<Vec<_>>();
        actual.sort();
        assert_eq!(actual, expected, "{pattern:?}, mode={mode}, case={ci}");
    }
    assert!(search_index(r"foo(?=bar)", "regex", None, 10, 500, root).is_err());
}

#[test]
fn incremental_changes_remove_old_postings_and_leave_unchanged_generation_alone() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.txt"), "oldneedle").unwrap();
    fs::write(root.join("b.txt"), "deleteneedle").unwrap();
    index_repository(root).unwrap();
    let first = generation(root);
    assert_eq!(index_repository(root).unwrap().changed, 0);
    assert_eq!(generation(root), first);
    fs::write(root.join("a.txt"), "newneedle and extra").unwrap();
    fs::remove_file(root.join("b.txt")).unwrap();
    fs::write(root.join("c.txt"), "anotherneedle").unwrap();
    index_repository(root).unwrap();
    assert_ne!(generation(root), first);
    for mode in ["literal", "text"] {
        for query in ["oldneedle", "deleteneedle"] {
            assert!(
                search_index(query, mode, None, 10, 500, root)
                    .unwrap()
                    .hits
                    .is_empty()
            );
        }
        assert_eq!(
            search_index("newneedle", mode, None, 10, 500, root)
                .unwrap()
                .hits[0]
                .path,
            "a.txt"
        );
    }
    let db = connect(&root.join(".ctx/index.sqlite"), false).unwrap();
    assert_eq!(
        open_generation(&db, &root.join(".ctx"))
            .unwrap()
            .num_files(),
        2
    );
}

#[test]
fn stale_missing_and_corrupt_indexes_fall_back_without_losing_matches() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.txt"), "before").unwrap();
    // Exact search also works before an initial index exists.
    assert_eq!(
        search_index("before", "literal", None, 10, 500, root)
            .unwrap()
            .hits
            .len(),
        1
    );
    index_repository(root).unwrap();
    fs::write(root.join("a.txt"), "afterneedle").unwrap();
    fs::write(root.join("new.txt"), "afterneedle").unwrap();
    assert_eq!(
        search_index("afterneedle", "literal", None, 10, 500, root)
            .unwrap()
            .hits
            .len(),
        2
    );
    index_repository(root).unwrap();
    let old = generation(root);
    fs::write(root.join(".ctx/text").join(&old).join("lookup.bin"), b"!").unwrap();
    assert_eq!(
        search_index("afterneedle", "literal", None, 10, 500, root)
            .unwrap()
            .hits
            .len(),
        2
    );
    assert_eq!(index_repository(root).unwrap().changed, 0);
    assert_ne!(generation(root), old);
}

#[test]
fn generation_publication_rolls_back_and_leases_keep_old_readers_alive() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.txt"), "original").unwrap();
    index_repository(root).unwrap();
    let first = generation(root);
    let mut db = connect(&root.join(".ctx/index.sqlite"), false).unwrap();
    let reader = open_generation(&db, &root.join(".ctx")).unwrap();
    {
        let tx = db.transaction().unwrap();
        tx.execute("UPDATE file_contents SET content='uncommitted'", [])
            .unwrap();
        let staged = stage_generation(&tx, root, &HashSet::from(["a.txt".into()]), &HashSet::new())
            .unwrap()
            .unwrap();
        let unpublished = get_meta(&tx, "text_generation", "").unwrap();
        assert_ne!(unpublished, first);
        drop(staged);
        assert!(!root.join(".ctx/text").join(unpublished).exists());
    }
    assert_eq!(generation(root), first);
    for text in ["second", "third", "fourth"] {
        fs::write(root.join("a.txt"), text).unwrap();
        index_repository(root).unwrap();
    }
    assert!(root.join(".ctx/text").join(&first).exists());
    assert_eq!(reader.file_path(0), Some("a.txt"));
    drop(reader);
    index_repository(root).unwrap();
    assert!(!root.join(".ctx/text").join(first).exists());
}

#[test]
fn admission_limits_and_output_budgets_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join(".ctx")).unwrap();
    fs::write(
        root.join(".ctx/config.toml"),
        "[index]\nmax_file_mb=1\nbuffer_mb=1\n",
    )
    .unwrap();
    fs::write(root.join("huge.txt"), "z".repeat(1_048_577)).unwrap();
    fs::write(root.join("binary.custom"), b"needle\0hidden").unwrap();
    for name in ["a", "b", "c"] {
        fs::write(root.join(name), "needle").unwrap();
    }
    index_repository(root).unwrap();
    let db = connect(&root.join(".ctx/index.sqlite"), false).unwrap();
    assert_eq!(get_meta(&db, "excluded_too_large", "").unwrap(), "1");
    let result = search_index("needle", "literal", None, 1, 1000, root).unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.coverage, "partial");
    assert!(result.hint.is_some());
    let result = search_index("needle", "literal", None, 10, 1, root).unwrap();
    assert!(result.hits.is_empty());
    assert_eq!(result.coverage, "partial");
}

#[test]
fn code_alternatives_keep_live_changes_and_session_publications_visible() {
    use ctx_code::text_index::{ReaderSession, exact_search, scan_search};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.py"), "def unrelated(): pass\n").unwrap();
    fs::write(root.join("b.py"), "def solve_dependencies(): pass\n").unwrap();
    index_repository(root).unwrap();
    let _session = ReaderSession::start(root).unwrap();
    let pattern = r"def (solve_dependencies|get_dependant)\(";
    let verify = |count| {
        let indexed = exact_search(pattern, "regex", false, None, 100, 10000, root).unwrap();
        let direct = scan_search(pattern, "regex", false, None, 100, 10000, root).unwrap();
        assert_eq!(indexed.hits, direct.hits);
        assert_eq!(indexed.hits.len(), count);
    };
    verify(1);
    // a.py was excluded by the cached generation; its new content must be read.
    fs::write(root.join("a.py"), "def get_dependant(): pass\n").unwrap();
    fs::write(root.join("new.py"), "def solve_dependencies(): pass\n").unwrap();
    // b.py was a candidate; it must still be read after losing its match.
    fs::write(root.join("b.py"), "def different(): pass\n").unwrap();
    verify(2);
    index_repository(root).unwrap();
    verify(2);
    fs::remove_file(root.join("a.py")).unwrap();
    index_repository(root).unwrap();
    verify(1);
}

#[test]
fn progressive_evidence_matches_eager_results_at_every_budget_boundary() {
    use ctx_code::search::apply_budget;
    use std::time::Instant;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = format!(
        "needle é\r\n{}needle \"escaped\"\n{}needle end",
        "padding\n".repeat(12),
        "gap\n".repeat(12)
    );
    fs::write(root.join("a.py"), &source).unwrap();
    index_repository(root).unwrap();
    let eager = content_hits(
        "a.py",
        &source,
        &matcher("needle", true, false).unwrap(),
        "text",
    );
    for limit in [0, 1, 2, 3, 4, 100] {
        let effective_limit = limit.max(1);
        for budget in 0..350 {
            let mut expected = apply_budget(
                eager.iter().take(effective_limit).cloned().collect(),
                budget,
                Instant::now(),
                "text_only",
                None,
            );
            if eager.len() > effective_limit {
                expected.coverage = "partial".into();
                expected.hint = Some("full-content search: results omitted or files changed/unreadable during the scan".into());
            }
            let actual = search_index("needle", "literal", None, limit, budget, root).unwrap();
            assert_eq!(actual.hits, expected.hits, "limit {limit}, budget {budget}");
            assert_eq!(
                (actual.tokens, actual.coverage, actual.hint),
                (expected.tokens, expected.coverage, expected.hint)
            );
        }
    }
}
