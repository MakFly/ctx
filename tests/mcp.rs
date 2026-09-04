use std::fs;
use std::path::{Path, PathBuf};

use ctx_code::indexer::index_repository;
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::{Map, Value, json};
use walkdir::WalkDir;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("mini_repo");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini_repo");
    for entry in WalkDir::new(&source).into_iter().map(Result::unwrap) {
        let relative = entry.path().strip_prefix(&source).unwrap();
        let destination = root.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(destination).unwrap();
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
    (temporary, root)
}

#[tokio::test]
async fn stdio_server_lists_and_calls_four_tools() -> anyhow::Result<()> {
    let (_temporary, root) = fixture();
    index_repository(&root)?;
    let executable = assert_cmd::cargo::cargo_bin!("ctx");
    let transport = TokioChildProcess::new(tokio::process::Command::new(executable).configure(
        |command| {
            command.arg("mcp").current_dir(&root);
        },
    ))?;
    let client = ().serve(transport).await?;
    let tools = client.list_all_tools().await?;
    let mut names = tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["ctx_file", "ctx_graph", "ctx_pack", "ctx_search"]);
    for tool in &tools {
        let annotations = tool.annotations.as_ref().expect("missing tool annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(false));
    }

    let search = call(&client, "ctx_search", json!({"query": "login"})).await?;
    assert_eq!(search["hits"][0]["path"], "auth.py");
    assert_eq!(search["hits"][0]["symbol"], "login");

    let graph = call(
        &client,
        "ctx_graph",
        json!({"op": "callers", "symbol": "login"}),
    )
    .await?;
    assert!(
        graph["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "app.py")
    );

    let pack = call(&client, "ctx_pack", json!({"query": "retry paiement"})).await?;
    assert!(
        pack["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "payments.py")
    );

    let files = call(&client, "ctx_file", json!({"q": "auth"})).await?;
    assert!(
        files["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "auth.py")
    );
    client.cancel().await?;
    Ok(())
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: Value,
) -> anyhow::Result<Value> {
    let arguments = arguments.as_object().cloned().unwrap_or_else(Map::new);
    let result = client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
        .await?;
    assert_ne!(result.is_error, Some(true));
    Ok(result.structured_content.unwrap())
}
