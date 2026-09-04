use std::path::{Path, PathBuf};

use globset::Glob;
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Json, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::config::find_ctx;
use crate::db::connect;
use crate::graph::graph_query;
use crate::model::{Envelope, Hit};
use crate::pack::pack_query;
use crate::search::search_index;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchRequest {
    #[schemars(description = "Text or symbol query")]
    pub query: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub path: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_search_budget")]
    pub budget_tokens: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphRequest {
    #[schemars(description = "def, refs, callers, callees, path, or impact")]
    pub op: String,
    pub symbol: String,
    #[serde(default = "default_depth")]
    pub depth: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PackRequest {
    pub query: String,
    #[serde(default = "default_pack_budget")]
    pub budget_tokens: usize,
    #[serde(default = "default_intent")]
    pub intent: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileRequest {
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct CtxMcp {
    root: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl CtxMcp {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl CtxMcp {
    #[tool(
        name = "ctx_search",
        description = "Search code symbols and bounded excerpts."
    )]
    fn ctx_search(
        &self,
        Parameters(request): Parameters<SearchRequest>,
    ) -> Result<Json<Envelope>, ErrorData> {
        search_index(
            &request.query,
            &request.mode,
            request.path.as_deref(),
            request.limit,
            request.budget_tokens,
            &self.root,
        )
        .map(Json)
        .map_err(mcp_error)
    }

    #[tool(
        name = "ctx_graph",
        description = "Find definitions, references, callers, callees, paths, or impact."
    )]
    fn ctx_graph(
        &self,
        Parameters(request): Parameters<GraphRequest>,
    ) -> Result<Json<Envelope>, ErrorData> {
        graph_query(
            &request.op,
            &request.symbol,
            request.depth,
            1_500,
            &self.root,
        )
        .map(Json)
        .map_err(mcp_error)
    }

    #[tool(
        name = "ctx_pack",
        description = "Build a ranked, bounded context pack."
    )]
    fn ctx_pack(
        &self,
        Parameters(request): Parameters<PackRequest>,
    ) -> Result<Json<Envelope>, ErrorData> {
        pack_query(
            &request.query,
            request.budget_tokens,
            &request.intent,
            &self.root,
        )
        .map(Json)
        .map_err(mcp_error)
    }

    #[tool(
        name = "ctx_file",
        description = "Find indexed paths by glob or fuzzy substring."
    )]
    fn ctx_file(
        &self,
        Parameters(request): Parameters<FileRequest>,
    ) -> Result<Json<Envelope>, ErrorData> {
        file_query(&self.root, &request.q, request.limit)
            .map(Json)
            .map_err(mcp_error)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for CtxMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Use ctx_pack before broad Grep. Cite returned path:line spans.")
    }
}

pub async fn run(root: PathBuf) -> anyhow::Result<()> {
    CtxMcp::new(root)
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}

pub fn file_query(root: &Path, query: &str, limit: usize) -> anyhow::Result<Envelope> {
    let database = find_ctx(root)?.join("index.sqlite");
    let connection = connect(&database, false)?;
    let paths = indexed_paths(&connection)?;
    let matcher = if query.contains(['*', '?', '[']) {
        Glob::new(query).ok().map(|glob| glob.compile_matcher())
    } else {
        None
    };
    let needle = query.replace('*', "").to_ascii_lowercase();
    let matches = paths
        .into_iter()
        .filter(|path| {
            matcher
                .as_ref()
                .is_some_and(|matcher| matcher.is_match(path))
                || path.to_ascii_lowercase().contains(&needle)
        })
        .take(limit)
        .collect::<Vec<_>>();
    let tokens = matches
        .iter()
        .map(|path| path.chars().count().div_ceil(4).max(1))
        .sum();
    Ok(Envelope {
        hits: matches
            .into_iter()
            .map(|path| Hit {
                path,
                start: 1,
                end: 1,
                symbol: None,
                kind: "config".to_owned(),
                sig: String::new(),
                snippet: String::new(),
                score: 1.0,
                why: "path match".to_owned(),
            })
            .collect(),
        tokens,
        freshness_ms: 0,
        coverage: "complete".to_owned(),
        hint: None,
    })
}

fn indexed_paths(connection: &Connection) -> anyhow::Result<Vec<String>> {
    let mut statement = connection.prepare("SELECT path FROM files ORDER BY path")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn mcp_error(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}

fn default_mode() -> String {
    "auto".to_owned()
}
fn default_limit() -> usize {
    20
}
fn default_search_budget() -> usize {
    1_500
}
fn default_pack_budget() -> usize {
    2_000
}
fn default_depth() -> usize {
    2
}
fn default_intent() -> String {
    "explore".to_owned()
}
