use std::fs;
use std::time::Duration;

use ctx_code::config::ctx_dir;
use ctx_code::db::connect;
use ctx_code::graph::graph_query;
use ctx_code::indexer::index_repository;
use ctx_code::lsp::enrich;
use serde_json::json;

#[test]
#[cfg(unix)]
fn lsp_enrichment_adds_and_invalidates_high_confidence_references() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repo");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("target.py"), "def foo():\n    return 1\n").unwrap();
    fs::write(root.join("caller.py"), "foo()\n").unwrap();
    index_repository(&root).unwrap();

    let caller_uri = url::Url::from_file_path(root.join("caller.py"))
        .unwrap()
        .to_string();
    let server = temporary.path().join("fake-lsp.sh");
    fs::write(
        &server,
        r#"#!/bin/sh
caller_uri="$1"
while :; do
  length=""
  while IFS= read -r header; do
    header=$(printf '%s' "$header" | tr -d '\r')
    [ -z "$header" ] && break
    case "$header" in
      Content-Length:*) length=${header#Content-Length: } ;;
    esac
  done
  [ -z "$length" ] && exit 0
  payload=$(dd bs=1 count="$length" 2>/dev/null)
  case "$payload" in
    *'"method":"exit"'*) exit 0 ;;
  esac
  id=$(printf '%s' "$payload" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$payload" in
    *'"method":"initialize"'*)
      result='{"capabilities":{"referencesProvider":true}}'
      ;;
    *'"method":"textDocument/references"'*)
      result='[{"uri":"'"$caller_uri"'","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}}}]'
      ;;
    *) result='null' ;;
  esac
  response='{"jsonrpc":"2.0","id":'"$id"',"result":'"$result"'}'
  printf 'Content-Length: %s\r\n\r\n%s' "${#response}" "$response"
done
"#,
    )
    .unwrap();
    let manifest = ctx_dir(&root).join("lsp/servers.json");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(
        manifest,
        serde_json::to_vec_pretty(&json!({
            "python": {
                "command": ["sh", server, caller_uri],
                "version": "test"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let result = enrich(&root, &["python".to_owned()], 1, Duration::from_secs(5)).unwrap();
    assert_eq!(result["edges"], 1);
    let callers = graph_query("callers", "foo", 2, 1_500, &root).unwrap();
    let hit = callers
        .hits
        .iter()
        .find(|hit| hit.path == "caller.py")
        .unwrap();
    assert_eq!(hit.score, 1.0);
    assert_eq!(hit.why, "lsp ref of foo");

    index_repository(&root).unwrap();
    let connection = connect(&ctx_dir(&root).join("index.sqlite"), false).unwrap();
    let preserved: i64 = connection
        .query_row("SELECT count(*) FROM edges WHERE source='lsp'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(preserved, 1);
    drop(connection);

    fs::write(root.join("target.py"), "def foo():\n    return 2\n").unwrap();
    index_repository(&root).unwrap();
    let connection = connect(&ctx_dir(&root).join("index.sqlite"), false).unwrap();
    let invalidated: i64 = connection
        .query_row("SELECT count(*) FROM edges WHERE source='lsp'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(invalidated, 0);
}
