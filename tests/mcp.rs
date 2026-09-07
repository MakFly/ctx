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
        if tool.name == "ctx_search" {
            assert!(tool.output_schema.is_some());
        }
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

    let one_shot = call(
        &client,
        "ctx_pack",
        json!({
            "query": "Where is login defined, which function calls login, which database function does login call, and where is retry_payment defined?"
        }),
    )
    .await?;
    assert!(one_shot["tokens"].as_u64().unwrap() <= 800);
    assert_eq!(one_shot["coverage"], "partial");
    assert!(
        !one_shot["hint"]
            .as_str()
            .unwrap()
            .starts_with("answer-ready:")
    );
    assert!(one_shot["hits"].as_array().unwrap().iter().any(|hit| {
        hit["path"] == "db.py"
            && hit["why"]
                .as_str()
                .is_some_and(|why| why.starts_with("callee of login"))
    }));

    let files = call(&client, "ctx_file", json!({"q": "auth"})).await?;
    assert!(
        files["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "auth.py")
    );
    let literal = call(
        &client,
        "ctx_search",
        json!({"query":"LOGIN", "mode":"literal", "ignore_case":true}),
    )
    .await?;
    assert!(
        literal["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "auth.py")
    );
    let regex = call(
        &client,
        "ctx_search",
        json!({"query":"def login", "mode":"regex"}),
    )
    .await?;
    assert!(
        regex["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "auth.py")
    );
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn compact_server_exposes_one_small_text_tool() -> anyhow::Result<()> {
    let (_temporary, root) = fixture();
    index_repository(&root)?;
    let executable = assert_cmd::cargo::cargo_bin!("ctx");
    let transport = TokioChildProcess::new(tokio::process::Command::new(executable).configure(
        |command| {
            command.args(["mcp", "--compact"]).current_dir(&root);
        },
    ))?;
    let client = ().serve(transport).await?;
    let tools = client.list_all_tools().await?;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "ctx_pack");
    assert_eq!(tools[0].input_schema["required"], json!(["query"]));
    assert_eq!(
        tools[0].input_schema["properties"]
            .as_object()
            .unwrap()
            .len(),
        1
    );

    let result = client
        .call_tool(
            CallToolRequestParams::new("ctx_pack".to_owned()).with_arguments(
                json!({"query": "login retry_payment"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    assert!(result.structured_content.is_none());
    let body = result.content[0].as_text().unwrap();
    let envelope: Value = serde_json::from_str(&body.text)?;
    assert!(envelope["tokens"].as_u64().unwrap() <= 800);
    assert!(
        envelope["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| { hit["path"] == "auth.py" && hit["symbol"] == "login" })
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
    if name == "ctx_search" {
        assert!(
            result.content.is_empty(),
            "modern search duplicated its payload"
        );
    }
    Ok(result.structured_content.unwrap())
}
