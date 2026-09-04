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
    assert!(
        one_shot
            .hint
            .as_deref()
            .is_some_and(|hint| hint.starts_with("answer-ready:"))
    );
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
