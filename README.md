# ctx

Local code search and bounded context for coding agents.

`ctx` is a Rust CLI and MCP server that indexes a repository, finds source code,
traces static relationships, and returns cited excerpts within a token budget.
It combines full-text SQLite FTS5 search, a disk-backed trigram index, and
Tree-sitter analysis in one native binary.

Indexing and retrieval run locally without an account, API key, embedding model,
or cloud service. The optional `ctx run` command invokes an external coding-agent
harness using that harness's credentials.

## Get started

Install from source with Rust 1.88 or newer. SQLite is bundled.

```console
git clone https://github.com/MakFly/ctx.git
cd ctx
cargo install --path .
```

From the repository you want to explore:

```console
ctx init
ctx index .
ctx search 'solve_dependencies' --mode literal --json
ctx search 'def (login|logout)' --mode regex --json
ctx graph --op callers --symbol login --json
ctx pack 'where is authentication handled?' --json
```

Artifacts live under `.ctx/`. Set `CTX_DIR` to use another index directory.
Git is optional and enables Git-aware freshness information.

## Choose the right search

| Command or mode | Purpose |
|---|---|
| `search --mode auto` | Ranked retrieval; the default mode |
| `search --mode text` | Ranked full-text search |
| `search --mode symbol` | Ranked symbol search |
| `search --mode literal` | Exact text in complete admitted files |
| `search --mode regex` | Rust regex matches in complete admitted files |
| `graph` | Definitions, references, callers, callees, paths and impact |
| `pack` | A bounded evidence pack for exploration, edits or review |
| `map` / `explore` | Repository maps and reusable onboarding briefings |

Exact searches return merged matches with two lines of surrounding context.
Use `--ignore-case` with literal or regex mode for Unicode case folding. Regexes
support inline flags, including multiline and dot-all; lookarounds and
backreferences are unsupported. Ranked queries are not interpreted as regexes.

```console
ctx search 'Vec<T>' --mode literal --budget-tokens 1500 --json
ctx search '(?s)start.*finish' --mode regex --json
ctx search 'HTTPException' --mode literal --ignore-case --json
ctx explore --intent change --focus authentication --harness none
```

Results include source paths, line ranges, snippets, token estimates and coverage.
Shortened excerpts carry `snippet_truncated: true`; missing or bounded evidence
is reported as partial. Token estimates use serialized hit characters divided by
four, rounded up per hit. They exclude envelope metadata and are not a provider's
token count. A budget too small for hit metadata returns no hit.

## Connect a coding agent

Preview and install integrations for Claude Code, Codex, OpenCode and Cursor:

```console
ctx install --dry-run
ctx install --target all
ctx update --dry-run
ctx update
```

Individual targets are `claude`, `codex`, `opencode` and `cursor`. Installation
merges existing JSON/TOML configuration and installs exploration skills and
agents. Codex project trust remains a user-controlled setting.

For a manual MCP configuration, use `ctx` as the command and `mcp` as its argument.
The stdio server exposes `ctx_search`, `ctx_graph`, `ctx_pack` and `ctx_file`.
`ctx mcp --compact` exposes only `ctx_pack`, as used by the generated Codex setup.

Full `ctx_search` sends structured evidence once to protocol 2025-06-18+ clients;
older clients also receive the text fallback. Compact pack output is unchanged.

## Live indexing and freshness

`ctx mcp` watches the repository for its session lifetime. To watch directly:

```console
ctx index . --watch
ctx status --json
```

One OS-held lock permits one index writer. Other sessions observe publications
and can take over when the owner exits. Manual indexing reports an active-writer
error while a watcher owns the index. No permanent daemon is installed, and
session shutdown stops and joins its worker.

Updates publish immutable trigram generations alongside SQLite transactions.
Readers retain leases on generations in use. Unchanged reindexing preserves the
current generation; changed files contribute incremental postings. MCP sessions
reuse generation readers and a bounded 16-entry matcher cache.

Exact search still traverses live files and checks versions before excluding
non-candidates. New or changed files bypass pruning; missing or corrupt indexes
fall back to scanning. Queries without safe trigram constraints scan directly.
Budgeted searches avoid constructing further excerpts after the evidence budget
closes while preserving omission and read-error reporting.

The watcher coalesces events after 200 ms of quiet, with a two-second maximum
burst window. Startup, queue overflow, ignore-rule changes and hourly
reconciliation repair drift. Linux watches admitted directories and relevant Git
metadata; other backends watch recursively and filter artifact events.

This is not an atomic filesystem snapshot. Pending structural updates and detected
read races produce partial coverage. Changes preserving every observable file
version attribute require `ctx index . --force` or forced watcher reconciliation.
`ctx status --json` reports generation, watcher, pending-work and exclusion data.

## Languages and file limits

Tree-sitter analysis supports Python, JavaScript/TypeScript, Go, Rust and PHP,
including supported frontend component formats and framework route heuristics.
Dynamic calls, generated code and framework detection remain best-effort.
Text files without a supported grammar can still provide search evidence.

```toml
# .ctx/config.toml
[index]
max_file_mb = 64
buffer_mb = 64
```

Text admission defaults to 64 MiB per file; structural analysis is limited to
1 MiB. Hidden-file and ignore policies still apply. Coverage concerns admitted
files, not ignored or oversized content. The buffer setting limits the trigram
external-sort arena, not total process memory.

## Optional capabilities

`ctx run` invokes Codex, Claude, OpenCode or Cursor in non-interactive read-only
mode. Exact verified answers can be reused on clean repositories without another
harness call. Dirty working trees disable reuse. Cache keys include Git state,
question, model, effort, harness fingerprint and evidence digest.

```console
ctx run 'where is login defined?' --harness codex --json
ctx cache status --json
ctx cache prune --max-age-days 30 --max-size-mb 256 --json
```

OpenCode and Cursor adapters require their project integration to be installed.
Harness compatibility depends on supported CLI options; unsupported versions
fail rather than silently broadening tool access.

LSP reference enrichment is explicit and outside the search hot path:

```console
ctx lsp sources --json
ctx lsp fetch --dry-run
ctx lsp enrich . --language rust --background
```

Language servers may execute project tools; use them only with trusted projects
or appropriate isolation. Embedding configuration is reserved for future work:
embeddings remain disabled, and no embedding model is downloaded or invoked.

## Measurements and validation

The latest engine validation passed **298 workspace/all-target tests**. A separate
140-case comparison against the preceding release matched complete search JSON
except elapsed time. These checks cover correctness, not universal speed gains.

[Benchmark methodology and raw results](benchmarks/README.md) distinguish:

- Historical synthetic retrieval and agent-workflow measurements.
- Paired indexed/scan searches on pinned FastAPI source code.
- Persistent MCP timings, output sizes, watcher visibility and shutdown checks.
- The latest optimization batch measured with an existing debug/test binary.
- A new ctx-versus-Zoekt CLI benchmark whose actual Zoekt run is still pending.

The latest batch reduced historical debug MCP medians by 9–37% in seven of eight
FastAPI scenarios; the absent query regressed 2%. Runs were separate, budgets
were unlimited, and these are **not release performance claims**. Earlier
release measurements predate that batch. There is no verified Zoekt speed ratio.

Benchmark scripts use existing executables and never compile or install tools.
Python 3 is needed only for the Python benchmark scripts, not for ctx itself.
See also the [pinned multi-repository comparison suite](benchmarks/competitive/README.md).

## Development and project status

`ctx` is an early project. Static relationships and framework heuristics are
best-effort; full-text evidence does not imply complete structural understanding.
See [PRD.md](PRD.md) for the product contract.

```console
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
```

The trigram engine is adapted from Microsoft tgrep under its MIT license.
[Vendored provenance](vendor/tgrep-core/PROVENANCE.md) records the pinned revision,
local changes and validation history. Maintainers should read
[the integration maintenance memory](docs/TGREP_MAINTENANCE.md) before changing
search, indexing or watchers. Further ideas are tracked in
[the optimization research](docs/SEARCH_OPTIMIZATION_RESEARCH.md).
