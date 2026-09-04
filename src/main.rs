use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use ctx_code::briefing::generate_briefing;
use ctx_code::cache::CacheStore;
use ctx_code::config::{ctx_dir, default_config_text, find_ctx, load_config, repo_root};
use ctx_code::db::{connect, get_meta};
use ctx_code::gitinfo::git_info;
use ctx_code::graph::graph_query;
use ctx_code::harness::{install, installation_plan};
use ctx_code::indexer::index_repository;
use ctx_code::map::build_map;
use ctx_code::model::Envelope;
use ctx_code::pack::pack_query;
use ctx_code::runner::{RunOptions, run_question};
use ctx_code::search::search_index;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "ctx",
    version,
    about = "Explore codebases locally with bounded evidence."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create local ctx configuration.
    Init,
    /// Index a repository into SQLite.
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        watch: bool,
    },
    /// Show index and Git freshness.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Search indexed text and symbols.
    Search {
        query: String,
        #[arg(long, value_enum, default_value_t = SearchMode::Auto)]
        mode: SearchMode,
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 1_500)]
        budget_tokens: usize,
        #[arg(long)]
        json: bool,
    },
    /// Query definitions and static relationships.
    Graph {
        #[arg(long, value_enum)]
        op: GraphOperation,
        #[arg(long)]
        symbol: String,
        #[arg(long, default_value_t = 2)]
        depth: usize,
        #[arg(long)]
        json: bool,
    },
    /// Build a bounded evidence pack.
    Pack {
        query: String,
        #[arg(long, value_enum, default_value_t = PackIntent::Explore)]
        intent: PackIntent,
        #[arg(long, default_value_t = 2_000)]
        budget_tokens: usize,
        #[arg(long)]
        json: bool,
    },
    /// Create a deterministic repository map.
    Map {
        #[arg(long, default_value = ".ctx/map.json")]
        out: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Index, map, and write a reusable briefing.
    Explore {
        #[arg(long, value_enum, default_value_t = ExploreIntent::Onboard)]
        intent: ExploreIntent,
        #[arg(long)]
        focus: Option<String>,
        #[arg(long, default_value = "none")]
        harness: String,
        #[arg(long, default_value = ".ctx")]
        out: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run the MCP server over stdio.
    Mcp {
        /// Expose only a minimal one-argument ctx_pack tool.
        #[arg(long)]
        compact: bool,
    },
    /// Ask a non-interactive harness with an exact local cache.
    Run {
        question: String,
        #[arg(long, value_enum, default_value_t = RunHarness::Auto)]
        harness: RunHarness,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long = "cache", value_enum, default_value_t = CacheMode::Auto)]
        cache_mode: CacheMode,
        #[arg(long, default_value_t = 90)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// Inspect or maintain the local response cache.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Inspect the future optional embedding layer.
    Embeddings {
        #[command(subcommand)]
        command: EmbeddingsCommand,
    },
    /// Detect and fetch optional language servers.
    Lsp {
        #[command(subcommand)]
        command: LspCommand,
    },
    /// Install ctx exploration skills for agent harnesses.
    Install(HarnessArgs),
    /// Refresh installed ctx agents, skills, and MCP configuration.
    Update(HarnessArgs),
}

#[derive(Debug, Subcommand)]
enum LspCommand {
    /// List the upstream GitHub repositories used by ctx.
    Sources {
        #[arg(long)]
        json: bool,
    },
    /// Detect fetched and PATH-provided language servers.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Fetch compatible official release artifacts from GitHub.
    Fetch {
        #[arg(long, value_enum)]
        language: Vec<LspLanguage>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Enrich static edges using installed language servers.
    Enrich {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum)]
        language: Vec<LspLanguage>,
        #[arg(long, default_value_t = 500)]
        max_symbols: usize,
        #[arg(long, default_value_t = 60.0)]
        timeout: f64,
        #[arg(long)]
        background: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Show cache size and hit counters.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Remove expired and least-recently-used entries.
    Prune {
        #[arg(long, default_value_t = 30)]
        max_age_days: u64,
        #[arg(long, default_value_t = 256)]
        max_size_mb: u64,
        #[arg(long)]
        json: bool,
    },
    /// Delete cached agent responses.
    Clear {
        #[arg(long, default_value = "agent")]
        kind: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum EmbeddingsCommand {
    /// Show embedding configuration without loading a model or using the network.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Reserved explicit setup entrypoint; providers remain disabled for now.
    Setup {
        #[arg(long, value_enum)]
        provider: EmbeddingProvider,
    },
    /// Reserved embedding index entrypoint; providers remain disabled for now.
    Index,
}

#[derive(Debug, Clone, Args)]
struct HarnessArgs {
    #[arg(long, value_enum, default_value_t = HarnessTarget::Auto)]
    target: HarnessTarget,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SearchMode {
    Auto,
    Text,
    Symbol,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum GraphOperation {
    Def,
    Refs,
    Callers,
    Callees,
    Path,
    Impact,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PackIntent {
    Explore,
    Edit,
    Review,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExploreIntent {
    Onboard,
    Change,
    Handoff,
    Impact,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum HarnessTarget {
    Auto,
    Claude,
    Codex,
    Opencode,
    Cursor,
    Both,
    All,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LspLanguage {
    All,
    Python,
    Typescript,
    Go,
    Rust,
    Php,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RunHarness {
    Auto,
    Codex,
    Claude,
    Opencode,
    Cursor,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CacheMode {
    Auto,
    Off,
    Refresh,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EmbeddingProvider {
    Local,
    Api,
}

impl SearchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Symbol => "symbol",
        }
    }
}

macro_rules! enum_string {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $name {
            fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }
    };
}

enum_string!(GraphOperation {
    Def => "def",
    Refs => "refs",
    Callers => "callers",
    Callees => "callees",
    Path => "path",
    Impact => "impact"
});
enum_string!(PackIntent {
    Explore => "explore",
    Edit => "edit",
    Review => "review"
});
enum_string!(ExploreIntent {
    Onboard => "onboard",
    Change => "change",
    Handoff => "handoff",
    Impact => "impact"
});
enum_string!(HarnessTarget {
    Auto => "auto",
    Claude => "claude",
    Codex => "codex",
    Opencode => "opencode",
    Cursor => "cursor",
    Both => "both",
    All => "all"
});
enum_string!(LspLanguage {
    All => "all",
    Python => "python",
    Typescript => "typescript",
    Go => "go",
    Rust => "rust",
    Php => "php"
});
enum_string!(RunHarness {
    Auto => "auto",
    Codex => "codex",
    Claude => "claude",
    Opencode => "opencode",
    Cursor => "cursor"
});
enum_string!(CacheMode {
    Auto => "auto",
    Off => "off",
    Refresh => "refresh"
});
enum_string!(EmbeddingProvider {
    Local => "local",
    Api => "api"
});

#[tokio::main]
async fn main() {
    if let Err(error) = execute(Cli::parse()).await {
        eprintln!("ctx: {error:#}");
        std::process::exit(1);
    }
}

async fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init => init(),
        Command::Index { path, watch } => {
            if watch {
                eprintln!("ctx: --watch est optionnel au MVP; index unique effectué");
            }
            let result = index_repository(path)?;
            println!(
                "indexed {} files ({} changed), {} symbols, {} edges",
                result.files, result.changed, result.symbols, result.edges
            );
            println!("{}", result.database.display());
            Ok(())
        }
        Command::Status { json } => emit(&status()?, json),
        Command::Search {
            query,
            mode,
            path,
            limit,
            budget_tokens,
            json,
        } => emit_envelope(
            &search_index(
                &query,
                mode.as_str(),
                path.as_deref(),
                limit.max(1),
                budget_tokens.max(1),
                ".",
            )?,
            json,
        ),
        Command::Graph {
            op,
            symbol,
            depth,
            json,
        } => emit_envelope(
            &graph_query(op.as_str(), &symbol, depth.max(1), 1_500, ".")?,
            json,
        ),
        Command::Pack {
            query,
            intent,
            budget_tokens,
            json,
        } => emit_envelope(
            &pack_query(&query, budget_tokens.max(1), intent.as_str(), ".")?,
            json,
        ),
        Command::Map { out, json } => {
            let value = build_map(".")?;
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&out, format!("{}\n", serde_json::to_string_pretty(&value)?))?;
            emit(&value, json)
        }
        Command::Explore {
            intent,
            focus,
            mut harness,
            out,
            force,
            json,
        } => {
            if harness != "none" {
                eprintln!("harness claude/codex = v1.1, fallback none");
                harness = "none".to_owned();
            }
            let root = repo_root(".")?;
            if !ctx_dir(&root).join("index.sqlite").is_file() {
                index_repository(&root)?;
            }
            let (data, skipped) = generate_briefing(
                &root,
                intent.as_str(),
                focus.as_deref(),
                &harness,
                &out,
                force,
            )?;
            if json {
                emit(&data, true)
            } else {
                println!(
                    "{}",
                    if skipped {
                        "briefing unchanged (same clean SHA)".to_owned()
                    } else {
                        format!(
                            "wrote {} then {}",
                            out.join("briefing.json").display(),
                            out.join("briefing.md").display()
                        )
                    }
                );
                Ok(())
            }
        }
        Command::Mcp { compact } => {
            let root = std::env::current_dir()?;
            if compact {
                ctx_code::mcp::run_compact(root).await
            } else {
                ctx_code::mcp::run(root).await
            }
        }
        Command::Run {
            question,
            harness,
            model,
            effort,
            cache_mode,
            timeout,
            json,
        } => {
            let root = repo_root(".")?;
            let result = run_question(
                &root,
                RunOptions {
                    question,
                    harness: harness.as_str().to_owned(),
                    model,
                    effort,
                    cache_mode: cache_mode.as_str().to_owned(),
                    timeout: Duration::from_secs(timeout.max(1)),
                },
            )
            .await?;
            if json {
                emit(&result, true)
            } else {
                println!("{}", result.answer);
                eprintln!(
                    "ctx: {} via {} in {}ms",
                    if result.cached {
                        "cache hit"
                    } else {
                        "cache miss"
                    },
                    result.harness,
                    result.duration_ms
                );
                Ok(())
            }
        }
        Command::Cache { command } => execute_cache(command),
        Command::Embeddings { command } => execute_embeddings(command),
        Command::Lsp { command } => execute_lsp(command).await,
        Command::Install(arguments) => execute_harness("install", arguments),
        Command::Update(arguments) => execute_harness("update", arguments),
    }
}

fn init() -> Result<()> {
    let root = std::env::current_dir()?;
    fs::create_dir_all(ctx_dir(&root))?;
    let config = ctx_dir(&root).join("config.toml");
    if !config.exists() {
        fs::write(&config, default_config_text())?;
    }
    let ignore = root.join(".ctxignore");
    if !ignore.exists() {
        fs::write(&ignore, "# Additional ctx ignore patterns\n*.generated.*\n")?;
    }
    let gitignore = root.join(".gitignore");
    if gitignore.is_file() {
        let current = fs::read_to_string(&gitignore)?;
        if !current.lines().any(|line| line == ".ctx/") {
            fs::write(&gitignore, format!("{}\n.ctx/\n", current.trim_end()))?;
        }
    }
    println!("initialized {}", ctx_dir(&root).display());
    Ok(())
}

fn execute_cache(command: CacheCommand) -> Result<()> {
    let root = repo_root(".")?;
    let store = CacheStore::open(&root)?;
    match command {
        CacheCommand::Status { json } => {
            let mut status = store.status()?;
            let policy = load_config(&root)?.cache;
            status["enabled"] = json!(policy.enabled);
            status["max_size_mb"] = json!(policy.max_size_mb);
            status["max_age_days"] = json!(policy.max_age_days);
            emit(&status, json)
        }
        CacheCommand::Prune {
            max_age_days,
            max_size_mb,
            json,
        } => emit(
            &json!({
                "removed": store.prune(max_age_days, max_size_mb)?,
                "max_age_days": max_age_days,
                "max_size_mb": max_size_mb,
            }),
            json,
        ),
        CacheCommand::Clear { kind, json } => {
            emit(&json!({"removed": store.clear(&kind)?, "kind": kind}), json)
        }
    }
}

fn execute_embeddings(command: EmbeddingsCommand) -> Result<()> {
    let root = repo_root(".")?;
    let settings = load_config(&root)?.embeddings;
    match command {
        EmbeddingsCommand::Status { json } => emit(
            &json!({
                "enabled": settings.enabled,
                "provider": settings.provider,
                "model": settings.model,
                "endpoint": settings.endpoint,
                "api_key_env": settings.api_key_env,
                "allow_remote_code": settings.allow_remote_code,
                "ready": false,
                "hint": "embeddings are staged for a later opt-in release; lexical and symbol retrieval remain active"
            }),
            json,
        ),
        EmbeddingsCommand::Setup { provider } => bail!(
            "embeddings {} désactivés pour cette version; configurez enabled=false en attendant l'étape opt-in",
            provider.as_str()
        ),
        EmbeddingsCommand::Index => bail!(
            "embeddings désactivés pour cette version; aucun modèle chargé et aucun réseau utilisé"
        ),
    }
}

fn status() -> Result<Value> {
    let database = find_ctx(".")?.join("index.sqlite");
    let connection = connect(&database, false)?;
    let root = PathBuf::from(get_meta(
        &connection,
        "repo_root",
        &std::env::current_dir()?.to_string_lossy(),
    )?);
    let count = |table: &str| -> Result<i64> {
        let query = format!("SELECT count(*) FROM {table}");
        Ok(connection.query_row(&query, [], |row| row.get(0))?)
    };
    let lsp_edges: i64 =
        connection.query_row("SELECT count(*) FROM edges WHERE source='lsp'", [], |row| {
            row.get(0)
        })?;
    let mut statement = connection.prepare("SELECT path,mtime FROM files")?;
    let mut stale = false;
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })? {
        let (path, indexed_mtime) = row?;
        let file = root.join(path);
        let modified = file
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        if !file.is_file() || modified > indexed_mtime + 0.000_001 {
            stale = true;
            break;
        }
    }
    let info = git_info(&root);
    Ok(json!({
        "sha": info.sha,
        "indexed_sha": get_meta(&connection, "indexed_sha", "")?,
        "dirty": info.dirty,
        "files": count("files")?,
        "symbols": count("symbols")?,
        "edges": count("edges")?,
        "lsp_edges": lsp_edges,
        "database": database,
        "size_bytes": fs::metadata(&database)?.len(),
        "stale": stale,
    }))
}

async fn execute_lsp(command: LspCommand) -> Result<()> {
    match command {
        LspCommand::Sources { json } => emit(&json!({"servers": ctx_code::lsp::sources()}), json),
        LspCommand::Status { json } => emit(
            &ctx_code::lsp::status(&std::env::current_dir()?, None)?,
            json,
        ),
        LspCommand::Fetch {
            language,
            dry_run,
            force,
            json,
        } => {
            let languages = language
                .into_iter()
                .map(|value| value.as_str().to_owned())
                .collect::<Vec<_>>();
            let root = std::env::current_dir()?;
            let value = tokio::task::spawn_blocking(move || {
                ctx_code::lsp::fetch(&root, &languages, dry_run, force)
            })
            .await??;
            emit(&value, json)
        }
        LspCommand::Enrich {
            path,
            language,
            max_symbols,
            timeout,
            background,
            json,
        } => {
            let root = repo_root(path)?;
            let languages = language
                .into_iter()
                .map(|value| value.as_str().to_owned())
                .collect::<Vec<_>>();
            let timeout = Duration::from_secs_f64(timeout.max(1.0));
            let value = tokio::task::spawn_blocking(move || {
                if background {
                    ctx_code::lsp::start_background(&root, &languages, max_symbols.max(1), timeout)
                } else {
                    ctx_code::lsp::enrich(&root, &languages, max_symbols.max(1), timeout)
                }
            })
            .await??;
            emit(&value, json)
        }
    }
}

fn execute_harness(mode: &str, arguments: HarnessArgs) -> Result<()> {
    let root = repo_root(".")?;
    if arguments.dry_run {
        let plan = installation_plan(&root, arguments.target.as_str(), mode, None, None)?;
        if arguments.json {
            return emit(&plan, true);
        }
        for (name, details) in &plan.detected {
            println!(
                "{}: {} ({})",
                name,
                if details.detected {
                    "detected"
                } else {
                    "absent"
                },
                if details.signals.is_empty() {
                    "no signal".to_owned()
                } else {
                    details.signals.join(", ")
                }
            );
        }
        println!(
            "selected: {}",
            if plan.selected.is_empty() {
                "none".to_owned()
            } else {
                plan.selected.join(", ")
            }
        );
        for path in plan.files {
            println!("would {mode}: {path}");
        }
        if let Some(hint) = plan.hint {
            println!("hint: {hint}");
        }
        return Ok(());
    }
    let verb = if mode == "update" {
        "updated"
    } else {
        "installed"
    };
    for path in install(&root, arguments.target.as_str(), mode)? {
        println!("{verb} {}", path.display());
    }
    Ok(())
}

fn emit_envelope(value: &Envelope, as_json: bool) -> Result<()> {
    if as_json {
        return emit(value, true);
    }
    for hit in &value.hits {
        println!(
            "{}:{} {} {} — {}",
            hit.path,
            hit.start,
            hit.kind,
            hit.symbol.as_deref().unwrap_or_default(),
            hit.why
        );
    }
    println!(
        "{} tokens; coverage={}; {}ms",
        value.tokens, value.coverage, value.freshness_ms
    );
    Ok(())
}

fn emit(value: &impl Serialize, _as_json: bool) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
