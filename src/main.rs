use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use ctx_code::briefing::generate_briefing;
use ctx_code::cache::CacheStore;
use ctx_code::config::{ctx_dir, default_config_text, find_ctx, load_config, repo_root};
use ctx_code::db::{connect, get_meta};
use ctx_code::gitinfo::git_info;
use ctx_code::graph::graph_query;
use ctx_code::harness::{install, installation_plan};
use ctx_code::indexer::IndexProgress;
use ctx_code::map::build_map;
use ctx_code::model::Envelope;
use ctx_code::pack::pack_query;
use ctx_code::read::{ReadOptions, read_file, render_read_notes, render_read_text};
use ctx_code::runner::{RunOptions, run_question};
use ctx_code::search::search_index_with_options;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "ctx",
    version,
    about = "Explore codebases locally with bounded evidence.",
    after_help = "Run ctx gain to see the usage dashboard."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show the usage dashboard (global by default).
    Gain(MetricsReportArgs),
    /// Create local ctx configuration.
    Init,
    /// Index a repository into SQLite.
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        watch: bool,
        /// Verify file contents even when size and modification time are unchanged.
        #[arg(long)]
        force: bool,
    },
    /// Queue or run an incremental reindex.
    Reindex {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        background: bool,
        #[arg(long)]
        force: bool,
    },
    /// Internal detached reindex worker.
    #[command(name = "reindex-worker", hide = true)]
    ReindexWorker {
        #[arg(long)]
        root: PathBuf,
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
        /// Fold Unicode case in literal/regex searches.
        #[arg(long)]
        ignore_case: bool,
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
    /// Measure ctx usage, token counts and estimated savings.
    Metrics {
        #[command(subcommand)]
        command: MetricsCommand,
    },
    /// Read a file, a line range or one definition, within a token budget.
    Read {
        path: String,
        #[arg(long, value_parser = parse_range)]
        lines: Option<(usize, usize)>,
        #[arg(long)]
        symbol: Option<String>,
        #[arg(long)]
        outline: bool,
        #[arg(long, default_value_t = 2_000)]
        budget_tokens: usize,
        #[arg(long)]
        raw: bool,
        #[arg(long)]
        json: bool,
    },
    /// Receive a harness lifecycle event and enqueue background work.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
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
enum MetricsCommand {
    /// Show queued and persisted metric counters.
    Status {
        #[arg(long)]
        global: bool,
        #[arg(long)]
        json: bool,
    },
    /// Aggregate metrics for the current project or all projects.
    Report(MetricsReportArgs),
    /// Export raw metric events as JSON.
    Export {
        #[arg(long)]
        global: bool,
        #[arg(long, default_value_t = 30)]
        since_days: u64,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Record a no-ctx measurement to pair with a ctx.run event.
    Baseline {
        #[arg(long)]
        harness: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        query: String,
        #[arg(long)]
        input_tokens: u64,
        #[arg(long)]
        output_tokens: u64,
    },
    /// Import paired benchmark measurements (JSON lines) as measured savings.
    ImportBench {
        file: PathBuf,
        /// Arm measured with ctx.
        #[arg(long, default_value = "ctx")]
        arm: String,
        /// Arm used as the plain baseline.
        #[arg(long, default_value = "base")]
        base: String,
    },
    /// Configure model pricing used by reports.
    Pricing {
        #[command(subcommand)]
        command: MetricsPricingCommand,
    },
    /// Internal asynchronous queue worker.
    #[command(hide = true)]
    Drain {
        #[arg(long)]
        root: PathBuf,
    },
    /// Enqueue a harness hook payload without waiting for aggregation.
    #[command(hide = true)]
    Enqueue {
        #[arg(long)]
        event_kind: String,
        #[arg(long)]
        harness: String,
        #[arg(long)]
        model: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum MetricsPricingCommand {
    /// Set input, cached-input and output rates per million tokens.
    Set {
        #[arg(long)]
        model: String,
        #[arg(long)]
        input_per_million: f64,
        #[arg(long)]
        cached_input_per_million: f64,
        #[arg(long)]
        output_per_million: f64,
        #[arg(long)]
        json: bool,
    },
    /// List configured model prices.
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct MetricsReportArgs {
    /// Include all projects (the default).
    #[arg(long)]
    global: bool,
    /// Read metrics from the current project's CTX_DIR.
    #[arg(long, conflicts_with = "global")]
    project: bool,
    /// Number of recent days to include.
    #[arg(long, default_value_t = 30)]
    since_days: u64,
    /// Return structured data without terminal formatting.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    /// Enqueue metrics and incremental reindex work.
    AfterTurn {
        #[arg(long)]
        harness: String,
        #[arg(long)]
        model: Option<String>,
    },
    /// Rewrite a shell command into a bounded ctx equivalent (PreToolUse hook).
    Rewrite {
        #[arg(long)]
        harness: String,
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
    Literal,
    Regex,
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
    Grok,
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

struct IndexProgressBar {
    enabled: bool,
    active: bool,
    last_draw: Instant,
}

impl IndexProgressBar {
    fn new() -> Self {
        Self {
            enabled: ctx_code::terminal::stderr_enabled(),
            active: false,
            last_draw: Instant::now() - Duration::from_secs(1),
        }
    }

    fn update(&mut self, progress: IndexProgress) {
        if !self.enabled {
            return;
        }
        let force_draw = matches!(
            progress,
            IndexProgress::Indexing { current, total } if current == total
        ) || matches!(progress, IndexProgress::Finalizing);
        if !force_draw && self.last_draw.elapsed() < Duration::from_millis(80) {
            return;
        }
        let (message, color) = match progress {
            IndexProgress::Scanning { files } => (format!("ctx: scanning files ({files})"), 33),
            IndexProgress::Indexing { current: _, total } if total == 0 => {
                ("ctx: indexing files (no changes)".to_owned(), 34)
            }
            IndexProgress::Indexing { current, total } => {
                let percent = (current.min(total) as u128 * 100 / total as u128) as usize;
                let width = 32;
                let filled = width * percent / 100;
                let bar = format!(
                    "{}>{}",
                    "=".repeat(filled.saturating_sub(1)),
                    " ".repeat(width.saturating_sub(filled))
                );
                (
                    format!("ctx: indexing [{bar}] {percent:>3}% ({current}/{total})"),
                    if percent == 100 { 32 } else { 36 },
                )
            }
            IndexProgress::Finalizing => ("ctx: finalizing index".to_owned(), 35),
        };
        self.draw(&message, color);
    }

    fn draw(&mut self, message: &str, color: u8) {
        let mut stderr = io::stderr().lock();
        let message = ctx_code::terminal::stderr(message, color);
        let _ = write!(stderr, "\r\x1b[2K{message}");
        let _ = stderr.flush();
        self.active = true;
        self.last_draw = Instant::now();
    }

    fn finish(&mut self) {
        if self.active {
            let mut stderr = io::stderr().lock();
            let _ = write!(stderr, "\r\x1b[2K");
            let _ = stderr.flush();
            self.active = false;
        }
    }

    fn fail(&mut self) {
        if self.active {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "\r\x1b[2K");
            let _ = stderr.flush();
            self.active = false;
        }
    }
}

impl SearchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Symbol => "symbol",
            Self::Literal => "literal",
            Self::Regex => "regex",
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
    Grok => "grok",
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
        eprintln!(
            "{}",
            ctx_code::terminal::stderr(format!("ctx: {error:#}"), 31)
        );
        std::process::exit(1);
    }
}

async fn execute(cli: Cli) -> Result<()> {
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    match command {
        Command::Gain(arguments) => execute_metrics_report(arguments),
        Command::Init => init(),
        Command::Index { path, watch, force } => {
            if watch {
                let root = path.canonicalize()?;
                let _session = ctx_code::watcher::WatchSession::start_with_force(&root, force)?;
                eprintln!(
                    "{}",
                    ctx_code::terminal::stderr(
                        format!("ctx: watching {}; Ctrl-C to stop", root.display()),
                        36
                    )
                );
                tokio::signal::ctrl_c().await?;
                return Ok(());
            }
            let mut progress = IndexProgressBar::new();
            let result = {
                let mut report_progress = |event| progress.update(event);
                ctx_code::indexer::index_repository_with_options_and_progress(
                    path,
                    force,
                    &mut report_progress,
                )
            };
            let result = match result {
                Ok(result) => {
                    progress.finish();
                    result
                }
                Err(error) => {
                    progress.fail();
                    return Err(error);
                }
            };
            println!(
                "{}",
                ctx_code::terminal::stdout(
                    format!(
                        "indexed {} files ({} changed), {} symbols, {} edges",
                        result.files, result.changed, result.symbols, result.edges
                    ),
                    32
                )
            );
            println!("{}", result.database.display());
            Ok(())
        }
        Command::Reindex {
            path,
            background,
            force,
        } => {
            if background {
                let root = repo_root(&path)?;
                ctx_code::reindex::request_async(&root, force)?;
                println!(
                    "{}",
                    ctx_code::terminal::stdout(format!("reindex queued {}", root.display()), 36)
                );
                return Ok(());
            }
            let result = ctx_code::indexer::index_repository_with_options(path, force)?;
            println!(
                "{}",
                ctx_code::terminal::stdout(
                    format!(
                        "reindexed {} files ({} changed), {} symbols, {} edges",
                        result.files, result.changed, result.symbols, result.edges
                    ),
                    32
                )
            );
            Ok(())
        }
        Command::ReindexWorker { root } => {
            ctx_code::reindex::drain(&root)?;
            Ok(())
        }
        Command::Status { json } => emit(&status()?, json),
        Command::Search {
            query,
            mode,
            ignore_case,
            path,
            limit,
            budget_tokens,
            json,
        } => emit_envelope(
            &search_index_with_options(
                &query,
                mode.as_str(),
                ignore_case,
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
            ctx_code::indexer::index_repository_with_options(&root, force)?;
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
                        "briefing unchanged (same indexed content and repository state)".to_owned()
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
                let cached = result.cached;
                eprintln!(
                    "{}",
                    ctx_code::terminal::stderr(
                        format!(
                            "ctx: {} via {} in {}ms",
                            if cached { "cache hit" } else { "cache miss" },
                            result.harness,
                            result.duration_ms
                        ),
                        if cached { 32 } else { 33 }
                    )
                );
                Ok(())
            }
        }
        Command::Cache { command } => execute_cache(command),
        Command::Metrics { command } => execute_metrics(command),
        Command::Read {
            path,
            lines,
            symbol,
            outline,
            budget_tokens,
            raw,
            json,
        } => execute_read(
            &path,
            ReadOptions {
                lines,
                symbol,
                outline,
                budget_tokens,
                raw,
            },
            json,
        ),
        Command::Hook { command } => execute_hook(command),
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
    println!(
        "{}",
        ctx_code::terminal::stdout(format!("initialized {}", ctx_dir(&root).display()), 32)
    );
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

fn execute_metrics(command: MetricsCommand) -> Result<()> {
    match command {
        MetricsCommand::Status { global, json } => {
            let root = std::env::current_dir()?;
            emit(&ctx_code::metrics::status(&root, global)?, json)
        }
        MetricsCommand::Report(arguments) => execute_metrics_report(arguments),
        MetricsCommand::Export {
            global,
            since_days,
            output,
        } => {
            let root = std::env::current_dir()?;
            let events = ctx_code::metrics::export(&root, global, since_days)?;
            let body = format!("{}\n", serde_json::to_string_pretty(&events)?);
            if let Some(path) = output {
                fs::write(path, body)?;
            } else {
                print!("{body}");
            }
            Ok(())
        }
        MetricsCommand::Baseline {
            harness,
            model,
            query,
            input_tokens,
            output_tokens,
        } => {
            let root = std::env::current_dir()?;
            ctx_code::metrics::record_baseline_async(
                &root,
                &harness,
                &model,
                &ctx_code::metrics::hash_query(&query),
                input_tokens,
                output_tokens,
            )
        }
        MetricsCommand::ImportBench { file, arm, base } => {
            let root = std::env::current_dir()?;
            let (imported, skipped) = ctx_code::metrics::import_bench(&root, &file, &arm, &base)?;
            println!("imported {imported} pairs ({skipped} skipped)");
            Ok(())
        }
        MetricsCommand::Pricing { command } => match command {
            MetricsPricingCommand::Set {
                model,
                input_per_million,
                cached_input_per_million,
                output_per_million,
                json,
            } => {
                let pricing = ctx_code::metrics::set_pricing(
                    &model,
                    input_per_million,
                    cached_input_per_million,
                    output_per_million,
                )?;
                emit(&pricing, json)
            }
            MetricsPricingCommand::List { json } => {
                let pricing = ctx_code::metrics::list_pricing()?;
                emit(&pricing, json)
            }
        },
        MetricsCommand::Drain { root } => {
            let _ = ctx_code::metrics::drain(&root)?;
            Ok(())
        }
        MetricsCommand::Enqueue {
            event_kind,
            harness,
            model,
        } => {
            let mut body = String::new();
            io::stdin().read_to_string(&mut body)?;
            let payload = if body.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&body).context("hook payload JSON invalide")?
            };
            let root = std::env::current_dir()?;
            ctx_code::metrics::record_hook_async(
                &root,
                &event_kind,
                &harness,
                model.as_deref(),
                &payload,
            )?;
            Ok(())
        }
    }
}

fn execute_metrics_report(arguments: MetricsReportArgs) -> Result<()> {
    let root = std::env::current_dir()?;
    let global = arguments.global || !arguments.project;
    let report = ctx_code::metrics::report(&root, global, arguments.since_days)?;
    if arguments.json {
        return emit(&report, true);
    }
    let directory = if global {
        ctx_code::config::global_metrics_dir()
    } else {
        ctx_dir(&root)
    };
    let database = root.join(directory).join("metrics.sqlite");
    print_metrics_dashboard(&report, &database);
    Ok(())
}

fn print_metrics_dashboard(report: &ctx_code::metrics::MetricsReport, database: &Path) {
    let scope = if report.scope == "global" {
        "Global Scope"
    } else {
        "Project Scope"
    };
    println!(
        "{}",
        ctx_code::terminal::stdout(format!("CTX Token Savings ({scope})"), 32)
    );
    println!(
        "{}",
        ctx_code::terminal::stdout(
            "============================================================",
            32
        )
    );
    println!();
    print_metric("Period", format!("Last {} days", report.since_days), 36);
    println!("Metrics file: {}", database.display());
    println!();

    if report.events == 0 {
        println!(
            "{}",
            ctx_code::terminal::stdout(
                format!("No usage recorded in the last {} days.", report.since_days),
                33
            )
        );
        if database.is_file() {
            println!("The metrics file exists, but this period contains no events.");
            println!("Try a longer period: ctx gain --since-days 90");
        } else {
            println!("No metrics file exists at the path above.");
        }
        println!("Usage is recorded by agent MCP calls, ctx run, and installed hooks.");
        println!("Standalone ctx search/pack commands do not record usage metrics.");
        if report.scope == "project" {
            println!("Check CTX_DIR if your agent uses a different index directory.");
            println!("Use ctx gain --global to see metrics across projects.");
        }
        print_dashboard_help();
        return;
    }

    let commands = report.mcp_calls + report.run_calls;
    let average_ms = if commands == 0 {
        0
    } else {
        report.total_duration_ms / commands
    };
    print_metric("Total commands", commands.to_string(), 36);
    print_metric(
        "Provider input tokens",
        format_count(report.input_tokens),
        36,
    );
    print_metric("Context tokens (est.)", format_count(report.ctx_tokens), 36);
    print_metric(
        "Provider output tokens",
        format_count(report.output_tokens),
        36,
    );
    let estimated = matches!(report.savings_confidence.as_str(), "estimated" | "mixed");
    let measured_saved = report
        .measured_saved_input_tokens
        .filter(|_| report.measured_baseline_input_tokens > 0);
    let measured_percent = measured_saved
        .map(|saved| saved as f64 * 100.0 / report.measured_baseline_input_tokens as f64);
    if let Some((saved, percent)) = measured_saved.zip(measured_percent) {
        let note = if saved < 0 {
            "measured: ctx used more"
        } else {
            "measured"
        };
        print_metric(
            if estimated {
                "Tokens saved (measured)"
            } else {
                "Tokens saved"
            },
            format!("{} ({percent:.1}%, {note})", format_signed_count(saved)),
            if saved < 0 { 33 } else { 32 },
        );
        if !estimated {
            print_metric(
                "Efficiency meter",
                efficiency_meter(Some(percent)),
                if saved < 0 { 33 } else { 32 },
            );
        }
    }
    if estimated {
        if let Some(avoided) = report.estimated_context_avoided_tokens {
            print_metric(
                "Context avoided vs whole files (estimate)",
                format_count(avoided),
                33,
            );
        }
    } else if measured_saved.is_none() {
        print_metric("Tokens saved", "n/a (baseline unavailable)".to_owned(), 33);
        print_metric("Efficiency meter", efficiency_meter(None), 33);
    }
    print_metric(
        "Total exec time",
        format!(
            "{} (avg {})",
            format_duration(report.total_duration_ms),
            format_duration(average_ms)
        ),
        36,
    );
    match report.estimated_cost_usd {
        Some(cost) => print_metric("Estimated cost", format!("{cost:.6} USD"), 32),
        None => print_metric("Estimated cost", "unknown pricing".to_owned(), 33),
    }
    if let Some(cost) = report.estimated_saved_cost_usd
        && report
            .measured_saved_input_tokens
            .is_none_or(|saved| saved >= 0)
    {
        print_metric(
            if estimated {
                "Est. cost avoided"
            } else {
                "Estimated saved cost"
            },
            format!("{cost:.6} USD"),
            32,
        );
    }
    if report.cache_hits > 0 {
        print_metric("Cache hits", report.cache_hits.to_string(), 32);
    }

    println!();
    println!("Provider totals include only usage reported by the harness.");
    if estimated {
        println!(
            "Estimates compare returned context with whole cited files; they are not a measured reduction in model usage."
        );
    } else if report.savings_confidence == "exact" {
        println!("Savings use recorded provider usage and a paired no-ctx baseline.");
    }
    println!();
    println!("{}", ctx_code::terminal::stdout("By Operation", 32));
    let rule = ctx_code::terminal::stdout("-".repeat(71), 32);
    println!("{rule}");
    println!(
        "{:<3} {:<16} {:>8} {:>10} {:>7} {:>9} {:<12}",
        "#",
        "Operation",
        "Count",
        if estimated { "Avoided" } else { "Saved" },
        "Avg%",
        "Time",
        "Impact"
    );
    println!("{rule}");
    if report.by_operation.is_empty() {
        println!(
            "{}",
            ctx_code::terminal::stdout("No MCP or ctx run calls recorded in this period.", 33)
        );
    } else {
        for (index, operation) in report.by_operation.iter().enumerate() {
            let saved = operation
                .saved_input_tokens
                .map(format_count)
                .unwrap_or_else(|| "-".to_owned());
            let average = operation
                .saved_input_tokens
                .zip(operation.baseline_input_tokens)
                .filter(|(_, baseline)| *baseline > 0)
                .map(|(saved, baseline)| format!("{:.1}%", saved as f64 * 100.0 / baseline as f64))
                .unwrap_or_else(|| "-".to_owned());
            let impact = operation
                .saved_input_tokens
                .zip(operation.baseline_input_tokens)
                .filter(|(_, baseline)| *baseline > 0)
                .map(|(saved, baseline)| saved as f64 * 100.0 / baseline as f64);
            let name = operation_name(&operation.name);
            let name_color = if operation.saved_input_tokens.is_some() {
                36
            } else {
                37
            };
            println!(
                "{:<3} {} {:>8} {:>10} {:>7} {:>9} {}",
                format!("{}.", index + 1),
                ctx_code::terminal::stdout(format!("{name:<16}"), name_color),
                operation.events,
                saved,
                average,
                format_duration(operation.duration_ms),
                ctx_code::terminal::stdout(
                    impact_bar(impact, 10),
                    if impact.is_some() { 36 } else { 33 }
                )
            );
        }
    }
    println!("{rule}");
    if report.scope == "global" && !report.by_project.is_empty() {
        println!(
            "{}",
            ctx_code::terminal::stdout(
                format!("Projects tracked: {}", report.by_project.len()),
                36
            )
        );
    }
    print_dashboard_help();
}

fn print_dashboard_help() {
    println!();
    println!("Views: ctx gain --project | ctx gain --since-days 7 | ctx gain --json");
    println!("Help:  ctx --help");
}

fn print_metric(label: &str, value: String, value_color: u8) {
    println!(
        "{} {}",
        ctx_code::terminal::stdout(format!("{:<24}", format!("{label}:")), 36),
        ctx_code::terminal::stdout(value, value_color)
    );
}

fn operation_name(value: &str) -> &str {
    match value {
        "mcp_pack" => "ctx_pack",
        "mcp_search" => "ctx_search",
        "mcp_graph" => "ctx_graph",
        "mcp_file" => "ctx_file",
        "ctx_run" => "ctx.run",
        _ => value,
    }
}

fn format_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn format_signed_count(value: i64) -> String {
    let count = format_count(value.unsigned_abs());
    if value < 0 {
        format!("-{count}")
    } else {
        count
    }
}

fn format_duration(milliseconds: u64) -> String {
    if milliseconds >= 60_000 {
        format!(
            "{}m{:02}s",
            milliseconds / 60_000,
            (milliseconds / 1_000) % 60
        )
    } else if milliseconds >= 1_000 {
        format!("{:.1}s", milliseconds as f64 / 1_000.0)
    } else {
        format!("{milliseconds}ms")
    }
}

fn efficiency_meter(percent: Option<f64>) -> String {
    let width = 36;
    let Some(percent) = percent else {
        return format!("[{}] n/a", ".".repeat(width));
    };
    let bounded = percent.clamp(0.0, 100.0);
    let filled = (width as f64 * bounded / 100.0).round() as usize;
    format!(
        "[{}{}] {:.1}%",
        "#".repeat(filled),
        ".".repeat(width.saturating_sub(filled)),
        percent
    )
}

fn impact_bar(percent: Option<f64>, width: usize) -> String {
    let Some(percent) = percent else {
        return format!("[{}]", ".".repeat(width));
    };
    let filled = (width as f64 * percent.clamp(0.0, 100.0) / 100.0).round() as usize;
    format!(
        "[{}{}]",
        "#".repeat(filled),
        ".".repeat(width.saturating_sub(filled))
    )
}

fn parse_range(value: &str) -> Result<(usize, usize), String> {
    let number = |text: &str| {
        text.trim()
            .parse::<usize>()
            .map_err(|_| format!("invalid line range `{value}`: expected A:B or A"))
    };
    match value.split_once(':') {
        Some((start, end)) => Ok((number(start)?, number(end)?)),
        None => number(value).map(|line| (line, line)),
    }
}

fn execute_read(path: &str, options: ReadOptions, as_json: bool) -> Result<()> {
    let output = read_file(Path::new(path), &options)?;
    if as_json {
        let text = if io::stdout().is_terminal() {
            serde_json::to_string_pretty(&output)?
        } else {
            serde_json::to_string(&output)?
        };
        println!("{text}");
        return Ok(());
    }
    print!("{}", render_read_text(&output, options.raw));
    if options.raw {
        eprint!("{}", render_read_notes(&output));
    }
    Ok(())
}

fn execute_hook(command: HookCommand) -> Result<()> {
    match command {
        HookCommand::AfterTurn { harness, model } => {
            let mut body = String::new();
            io::stdin().read_to_string(&mut body)?;
            let payload = if body.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&body).context("hook payload JSON invalide")?
            };
            let root = std::env::current_dir()?;
            let _ = ctx_code::metrics::record_hook_async(
                &root,
                "after-turn",
                &harness,
                model.as_deref(),
                &payload,
            );
            ctx_code::reindex::request_async(&root, false)?;
            Ok(())
        }
        HookCommand::Rewrite { harness } => {
            // A PreToolUse hook never blocks: unreadable or unsupported input prints nothing.
            let mut body = String::new();
            let response = io::stdin()
                .read_to_string(&mut body)
                .ok()
                .and_then(|_| serde_json::from_str::<Value>(&body).ok())
                .and_then(|payload| ctx_code::hook_rewrite::hook_response(&payload, &harness));
            if let Some(response) = response {
                println!("{response}");
            }
            Ok(())
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
    let drift = ctx_code::watcher::index_drift(&root);
    let stale = drift.is_stale();
    let info = git_info(&root);
    let watch = ctx_code::watcher::status(&root);
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
        "drift": {
            "added": drift.added,
            "modified": drift.modified,
            "removed": drift.removed,
        },
        "text_generation": get_meta(&connection, "text_generation", "")?,
        "previous_text_generation": get_meta(&connection, "previous_text_generation", "")?,
        "excluded_too_large": get_meta(&connection, "excluded_too_large", "0")?.parse::<u64>().unwrap_or(0),
        "text_max_file_bytes": get_meta(&connection, "text_max_file_bytes", "0")?.parse::<u64>().unwrap_or(0),
        "watcher": watch["watcher"],
        "catching_up": watch["catching_up"],
        "last_reconciliation_ms": watch["last_reconciliation_ms"],
        "watcher_error": watch["error"],
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
            "{}:{} {} {}: {}",
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
