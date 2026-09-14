use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ctx_code::briefing::generate_briefing;
use ctx_code::config::ctx_dir;
use ctx_code::db::connect;
use ctx_code::graph::graph_query;
use ctx_code::indexer::index_repository;
use ctx_code::map::build_map;
use ctx_code::pack::pack_query;
use ctx_code::search::search_index;
use tempfile::TempDir;
use walkdir::WalkDir;

fn fixture() -> (TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("mini_repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini_repo");
    copy_tree(&source, &root);
    (temporary, root)
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in WalkDir::new(source).into_iter().map(Result::unwrap) {
        let relative = entry.path().strip_prefix(source).unwrap();
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(target).unwrap();
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn indexes_schema_and_skips_unchanged_files() {
    let (_temporary, root) = fixture();
    let first = index_repository(&root).unwrap();
    assert_eq!(first.files, 10);
    assert!(first.symbols >= 16);
    assert!(first.edges > 0);
    assert!(first.database.is_file());
    let second = index_repository(&root).unwrap();
    assert_eq!(second.changed, 0);

    let connection = connect(&first.database, false).unwrap();
    for table in ["files", "symbols", "edges", "files_fts", "symbols_fts"] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing {table}");
    }
}

#[test]
fn search_graph_and_pack_match_acceptance_contract() {
    let (_temporary, root) = fixture();
    index_repository(&root).unwrap();

    let search = search_index("login", "auto", None, 20, 1_500, &root).unwrap();
    assert_eq!(search.hits[0].path, "auth.py");
    assert_eq!(search.hits[0].symbol.as_deref(), Some("login"));
    assert_eq!(search.hits[0].kind, "def");

    let definition = graph_query("def", "AuthService", 2, 1_500, &root).unwrap();
    assert!(definition.hits.iter().any(|hit| hit.path == "auth.py"));

    let callers = graph_query("callers", "login", 2, 1_500, &root).unwrap();
    assert!(callers.hits.iter().any(|hit| hit.path == "app.py"));

    let pack = pack_query("retry paiement", 2_000, "explore", &root).unwrap();
    assert!(pack.hits.iter().any(|hit| {
        hit.path == "payments.py" && hit.symbol.as_deref() == Some("retry_payment")
    }));

    let one_shot = pack_query(
        "Where is login defined, which function calls login, which database function does login call, and where is retry_payment defined?",
        800,
        "explore",
        &root,
    )
    .unwrap();
    assert!(one_shot.tokens <= 800);
    // Static graph relations remain best-effort even when all requested hits fit.
    assert_eq!(one_shot.coverage, "partial");
    assert!(
        one_shot
            .hint
            .as_deref()
            .is_some_and(|hint| !hint.starts_with("answer-ready:") && hint.contains("expand "))
    );
    for hit in &one_shot.hits {
        if hit.kind == "def" {
            assert!(
                hit.snippet.is_empty() || hit.snippet.chars().count() <= hit.sig.chars().count(),
                "{} snippet longer than signature",
                hit.symbol.as_deref().unwrap_or("?")
            );
        } else {
            assert!(
                hit.snippet.is_empty(),
                "explore extra hit should be digest: {}",
                hit.why
            );
        }
    }
    for (path, symbol, why) in [
        ("auth.py", "login", "definition of login"),
        ("app.py", "login_route", "caller of login"),
        ("db.py", "get_user", "callee of login"),
        (
            "payments.py",
            "retry_payment",
            "definition of retry_payment",
        ),
        ("payments.py", "charge", "callee of retry_payment"),
    ] {
        assert!(one_shot.hits.iter().any(|hit| {
            hit.path == path && hit.symbol.as_deref() == Some(symbol) && hit.why.starts_with(why)
        }));
    }

    let punctuated = pack_query("login retry_payment.", 800, "explore", &root).unwrap();
    assert!(punctuated.hits.iter().any(|hit| {
        hit.symbol.as_deref() == Some("retry_payment")
            && hit.why.starts_with("definition of retry_payment")
    }));
}

#[test]
fn pack_preserves_requested_definitions_before_long_bodies_and_relations() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let mut source = String::new();
    for name in ["alpha", "beta", "gamma", "delta"] {
        source.push_str(&format!(
            "def {name}():\n    \"\"\"{}\"\"\"\n    return 1\n\n",
            "documentation ".repeat(180)
        ));
    }
    fs::write(root.join("long.py"), source).unwrap();
    index_repository(root).unwrap();
    let pack = pack_query("alpha beta gamma delta", 800, "explore", root).unwrap();
    for name in ["alpha", "beta", "gamma", "delta"] {
        assert!(
            pack.hits
                .iter()
                .any(|hit| hit.symbol.as_deref() == Some(name)),
            "missing {name}"
        );
    }
    assert!(pack.tokens <= 800);
    assert_eq!(pack.coverage, "partial");
    let hint = pack.hint.unwrap();
    assert!(!hint.starts_with("answer-ready:"));
    assert!(hint.contains("expand "));
    for hit in &pack.hits {
        if hit.kind != "def" && hit.kind != "test" {
            assert!(hit.snippet.is_empty(), "digest extra hit {}", hit.why);
        }
    }
}

#[test]
fn pack_explore_is_signature_only_while_edit_keeps_bodies() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(
        root.join("long.py"),
        format!(
            "def long_function():\n    \"\"\"{}\"\"\"\n    return 'ending'\n",
            "description ".repeat(80)
        ),
    )
    .unwrap();
    index_repository(root).unwrap();
    let explore = pack_query("long_function", 800, "explore", root).unwrap();
    let explored = explore
        .hits
        .iter()
        .find(|hit| hit.symbol.as_deref() == Some("long_function") && hit.kind == "def")
        .expect("explore definition");
    assert!(!explored.sig.is_empty());
    assert!(
        explored.snippet.is_empty()
            || explored.snippet.chars().count() <= explored.sig.chars().count()
    );
    assert!(explored.snippet_truncated);

    let edit = pack_query("long_function", 2_000, "edit", root).unwrap();
    let edited = edit
        .hits
        .iter()
        .find(|hit| hit.symbol.as_deref() == Some("long_function") && hit.kind == "def")
        .expect("edit definition");
    assert!(!edited.sig.is_empty());
    assert!(edited.snippet.chars().count() > edited.sig.chars().count());
    assert!(edited.snippet.contains("ending"));
}

#[test]
fn pack_expand_round_trip_restores_omitted_body() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(
        root.join("long.py"),
        format!(
            "def long_function():\n    \"\"\"{}\"\"\"\n    return 'ending'\n",
            "description ".repeat(80)
        ),
    )
    .unwrap();
    index_repository(root).unwrap();
    let first = pack_query("long_function", 800, "explore", root).unwrap();
    let truncated = first
        .hits
        .iter()
        .find(|hit| hit.symbol.as_deref() == Some("long_function"))
        .expect("truncated definition");
    assert!(truncated.snippet_truncated);
    let hint = first.hint.expect("expand hint");
    assert!(hint.contains("expand "));
    let expanded = pack_query(&hint, 800, "explore", root).unwrap();
    let restored = expanded
        .hits
        .iter()
        .find(|hit| hit.path == truncated.path && hit.start == truncated.start)
        .expect("expanded span");
    assert!(!restored.snippet_truncated);
    assert!(restored.snippet.contains("ending"));
    assert!(restored.snippet.chars().count() > truncated.sig.chars().count());
}

#[test]
fn pack_keeps_qualified_homonyms_and_propagates_source_truncation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("methods.py"), "class A:\n    def run(self):\n        return 1\nclass B:\n    def run(self):\n        return 2\n").unwrap();
    fs::write(
        root.join("long.py"),
        format!(
            "def long_function():\n    \"\"\"{}\"\"\"\n    return 'ending'\n",
            "description ".repeat(250)
        ),
    )
    .unwrap();
    index_repository(root).unwrap();
    let pack = pack_query("A.run B.run", 2_000, "explore", root).unwrap();
    for start in [2, 5] {
        assert!(
            pack.hits
                .iter()
                .any(|hit| hit.path == "methods.py" && hit.start == start && hit.kind == "def")
        );
    }
    let pack = pack_query("long_function", 800, "explore", root).unwrap();
    assert_eq!(pack.coverage, "partial");
    assert!(pack.hits.iter().any(|hit| hit.snippet_truncated));
    let hint = pack.hint.unwrap();
    assert!(!hint.starts_with("answer-ready:"));
    assert!(hint.contains("expand "));
}

#[test]
fn search_budget_counts_serialized_hits_even_for_tiny_budgets() {
    let (_temporary, root) = fixture();
    index_repository(&root).unwrap();
    for budget in [0, 1, 50, 100, 200] {
        let result = search_index("login", "auto", None, 20, budget, &root).unwrap();
        let actual: usize = result
            .hits
            .iter()
            .map(|hit| {
                serde_json::to_string(hit)
                    .unwrap()
                    .chars()
                    .count()
                    .div_ceil(4)
            })
            .sum();
        assert_eq!(result.tokens, actual);
        assert!(actual <= budget);
        if budget <= 1 {
            assert!(result.hits.is_empty());
        }
    }
}

#[test]
fn text_search_preserves_bm25_relevance() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("rank.py"), "def z_best():\n    # unicorn unicorn unicorn unicorn\n    pass\ndef a_weak():\n    # unicorn common words unrelated filler words here\n    pass\ndef filler():\n    # entirely different content\n    pass\n").unwrap();
    let index = index_repository(root).unwrap();
    let connection = connect(&index.database, false).unwrap();
    let expected: String = connection.query_row("SELECT name FROM symbols_fts WHERE symbols_fts MATCH 'unicorn' ORDER BY bm25(symbols_fts,5.0,3.0,1.0) LIMIT 1", [], |row| row.get(0)).unwrap();
    assert_eq!(expected, "z_best");
    let result = search_index("unicorn", "text", None, 2, 2_000, root).unwrap();
    assert_eq!(result.hits[0].symbol.as_deref(), Some(expected.as_str()));
}

#[test]
fn index_removes_binary_transitions_and_force_checks_equal_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let file = root.join("sample.py");
    fs::write(&file, "def alpha():\n    return 1\n").unwrap();
    index_repository(root).unwrap();
    let time = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(&file, "def bravo():\n    return 1\n").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
    ctx_code::indexer::index_repository_with_options(root, true).unwrap();
    assert!(
        graph_query("def", "alpha", 1, 800, root)
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(
        !graph_query("def", "bravo", 1, 800, root)
            .unwrap()
            .hits
            .is_empty()
    );
    fs::write(&file, b"\0binary").unwrap();
    let result = index_repository(root).unwrap();
    assert_eq!((result.files, result.symbols), (0, 0));
}

#[test]
fn map_ignores_edges_to_deleted_files() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(
        root.join("a.py"),
        "from b import target\ndef source():\n    target()\n",
    )
    .unwrap();
    fs::write(root.join("b.py"), "def target():\n    return 1\n").unwrap();
    index_repository(root).unwrap();
    fs::remove_file(root.join("b.py")).unwrap();
    let map = build_map(root).unwrap();
    assert!(map.hubs.iter().all(|hub| hub.path != "b.py"));
}

#[test]
fn index_migrates_legacy_edges_before_creating_dependent_indexes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir(root.join(".ctx")).unwrap();
    let connection = rusqlite::Connection::open(root.join(".ctx/index.sqlite")).unwrap();
    connection.execute_batch("CREATE TABLE edges(id INTEGER PRIMARY KEY,src_symbol_id INTEGER,dst_name TEXT NOT NULL,dst_symbol_id INTEGER,kind TEXT,file_id INTEGER,line INTEGER);").unwrap();
    drop(connection);
    fs::write(root.join("a.py"), "def alpha():\n    pass\n").unwrap();
    let result = index_repository(root).unwrap();
    assert_eq!(result.symbols, 1);
    let connection = connect(&result.database, false).unwrap();
    assert_eq!(
        ctx_code::db::get_meta(&connection, "schema_version", "").unwrap(),
        "4"
    );
}

#[test]
fn map_and_briefing_only_reference_real_paths_and_skip_clean_snapshot() {
    let (_temporary, root) = fixture();
    index_repository(&root).unwrap();
    let repository_map = build_map(&root).unwrap();
    assert!(
        repository_map
            .entrypoints
            .iter()
            .any(|entry| entry.path == "app.py")
    );
    assert!(
        repository_map
            .router
            .iter()
            .any(|route| route.path == "app.py" && route.route == "/login")
    );

    let out = ctx_dir(&root);
    let (briefing, skipped) =
        generate_briefing(&root, "change", Some("login"), "none", &out, false).unwrap();
    assert!(!skipped);
    assert!(out.join("briefing.json").is_file());
    assert!(out.join("briefing.md").is_file());
    for hit in briefing["hits"].as_array().unwrap() {
        assert!(
            root.join(hit["path"].as_str().unwrap()).is_file(),
            "briefing referenced missing path"
        );
    }
    let markdown = fs::read_to_string(out.join("briefing.md")).unwrap();
    assert!(markdown.matches(":1`").count() + markdown.matches(":2`").count() >= 3);

    let before = fs::metadata(out.join("briefing.json"))
        .unwrap()
        .modified()
        .unwrap();
    let (second, skipped) =
        generate_briefing(&root, "change", Some("login"), "none", &out, false).unwrap();
    assert!(skipped);
    assert_eq!(second["skipped"], true);
    assert_eq!(
        fs::metadata(out.join("briefing.json"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );

    let (_, skipped) =
        generate_briefing(&root, "change", Some("logout"), "none", &out, false).unwrap();
    assert!(!skipped, "a different focus must invalidate the briefing");
}

#[test]
fn indexes_all_supported_languages() {
    let (_temporary, root) = fixture();
    index_repository(&root).unwrap();
    let connection = connect(&ctx_dir(&root).join("index.sqlite"), false).unwrap();
    for language in ["python", "typescript", "go", "rust", "php"] {
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM files WHERE lang=?1",
                [language],
                |row| row.get(0),
            )
            .unwrap();
        assert!(count > 0, "missing indexed language {language}");
    }
}

#[test]
fn pack_is_bounded_and_fast_after_indexing() {
    let (_temporary, root) = fixture();
    index_repository(&root).unwrap();
    let started = Instant::now();
    let pack = pack_query("retry paiement", 200, "explore", &root).unwrap();
    let elapsed = started.elapsed();
    assert!(pack.tokens <= 200);
    assert!(
        elapsed.as_millis() < 500,
        "pack took {}ms",
        elapsed.as_millis()
    );
}

fn has_symbol(envelope: &ctx_code::model::Envelope, name: &str) -> bool {
    envelope
        .hits
        .iter()
        .any(|hit| hit.symbol.as_deref() == Some(name))
}

fn assert_hits_have_path_and_spans(envelope: &ctx_code::model::Envelope) {
    assert!(
        !envelope.hits.is_empty(),
        "expected graph hits with path and line spans"
    );
    for hit in &envelope.hits {
        assert!(
            !hit.path.is_empty(),
            "hit missing path for symbol {:?}",
            hit.symbol
        );
        assert!(
            hit.start >= 1,
            "{}:{}-{} missing a 1-based start span",
            hit.path,
            hit.start,
            hit.end
        );
        assert!(
            hit.end >= hit.start,
            "{}:{}-{} has an inverted span",
            hit.path,
            hit.start,
            hit.end
        );
    }
}

fn distinguishes_paths(envelope: &ctx_code::model::Envelope, left: &str, right: &str) -> bool {
    let hint = envelope.hint.as_deref().unwrap_or("");
    let mentions =
        |path: &str| envelope.hits.iter().any(|hit| hit.path == path) || hint.contains(path);
    mentions(left) && mentions(right)
}

#[test]
fn python_call_chain_path_and_impact_honor_depth() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(
        root.join("a.py"),
        "from b import chain_b\n\ndef chain_a():\n    return chain_b()\n",
    )
    .unwrap();
    fs::write(
        root.join("b.py"),
        "from c import chain_c\n\ndef chain_b():\n    return chain_c()\n",
    )
    .unwrap();
    fs::write(root.join("c.py"), "def chain_c():\n    return 1\n").unwrap();
    index_repository(root).unwrap();

    for operation in ["path", "impact"] {
        let depth_one = graph_query(operation, "chain_a", 1, 4_000, root).unwrap();
        assert_hits_have_path_and_spans(&depth_one);
        assert!(
            !has_symbol(&depth_one, "chain_c"),
            "{operation} depth 1 must not include chain_c: {:?}",
            depth_one
                .hits
                .iter()
                .map(|hit| (hit.path.as_str(), hit.symbol.as_deref(), hit.start, hit.end))
                .collect::<Vec<_>>()
        );

        let depth_two = graph_query(operation, "chain_a", 2, 4_000, root).unwrap();
        assert_hits_have_path_and_spans(&depth_two);
        let reached = depth_two
            .hits
            .iter()
            .find(|hit| hit.symbol.as_deref() == Some("chain_c"))
            .unwrap_or_else(|| {
                panic!(
                    "{operation} depth 2 must include chain_c: {:?}",
                    depth_two
                        .hits
                        .iter()
                        .map(|hit| (hit.path.as_str(), hit.symbol.as_deref(), hit.start, hit.end))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(reached.path, "c.py");
        assert!(reached.start >= 1);
        assert!(reached.end >= reached.start);
    }
}

fn write_unique_and_homonym_python(root: &Path) {
    fs::write(
        root.join("unique_src.py"),
        "from unique_dst import unique_dst\n\ndef unique_src():\n    return unique_dst()\n",
    )
    .unwrap();
    fs::write(
        root.join("unique_dst.py"),
        "def unique_dst():\n    return 1\n",
    )
    .unwrap();
    fs::write(root.join("util.py"), "def leaf():\n    return 1\n").unwrap();
    fs::write(
        root.join("left.py"),
        "from util import leaf\n\ndef shared():\n    return leaf()\n",
    )
    .unwrap();
    fs::write(
        root.join("right.py"),
        "from util import leaf\n\ndef shared():\n    return leaf()\n",
    )
    .unwrap();
    fs::write(
        root.join("caller.py"),
        "from left import shared\n\ndef user():\n    return shared()\n",
    )
    .unwrap();
}

#[test]
fn unique_def_callees_are_complete_and_include_callee() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write_unique_and_homonym_python(root);
    index_repository(root).unwrap();

    let unique = graph_query("callees", "unique_src", 1, 4_000, root).unwrap();
    assert_eq!(unique.coverage, "complete");
    assert!(
        has_symbol(&unique, "unique_dst"),
        "unique-def callees must include unique_dst: {:?}",
        unique
            .hits
            .iter()
            .map(|hit| (hit.path.as_str(), hit.symbol.as_deref()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn homonym_callees_and_callers_are_partial_and_not_collapsed() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write_unique_and_homonym_python(root);
    index_repository(root).unwrap();

    let callees = graph_query("callees", "shared", 1, 4_000, root).unwrap();
    let callers = graph_query("callers", "shared", 1, 4_000, root).unwrap();
    assert_eq!(callees.coverage, "partial");
    assert_eq!(callers.coverage, "partial");
    assert!(
        distinguishes_paths(&callees, "left.py", "right.py"),
        "homonym callees collapsed to one path: hits={:?} hint={:?}",
        callees
            .hits
            .iter()
            .map(|hit| (hit.path.as_str(), hit.symbol.as_deref()))
            .collect::<Vec<_>>(),
        callees.hint
    );
    assert!(
        distinguishes_paths(&callers, "left.py", "right.py"),
        "homonym callers collapsed to one path: hits={:?} hint={:?}",
        callers
            .hits
            .iter()
            .map(|hit| (hit.path.as_str(), hit.symbol.as_deref()))
            .collect::<Vec<_>>(),
        callers.hint
    );
}

#[test]
fn new_admitted_python_file_without_reindex_is_not_complete_coverage() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("keep.py"), "def fresh_widget():\n    return 1\n").unwrap();
    index_repository(root).unwrap();

    fs::write(root.join("extra.py"), "def fresh_widget():\n    return 2\n").unwrap();

    let search = search_index("fresh_widget", "auto", None, 20, 4_000, root).unwrap();
    let definition = graph_query("def", "fresh_widget", 1, 4_000, root).unwrap();
    let pack = pack_query("fresh_widget", 4_000, "edit", root).unwrap();
    assert_ne!(
        search.coverage, "complete",
        "search must not claim complete coverage with an unindexed admitted file"
    );
    assert_ne!(
        definition.coverage, "complete",
        "graph def must not claim complete coverage with an unindexed admitted file"
    );
    assert_ne!(
        pack.coverage, "complete",
        "pack must not claim complete coverage with an unindexed admitted file"
    );
}
