# ctx — fast local codebase context for coding agents

`ctx` is a Rust-native, local-first codebase search and exploration tool for
Claude Code, Codex, OpenCode, Cursor, and other MCP-compatible coding agents.
It indexes a repository into SQLite FTS5, extracts symbols and relationships
with tree-sitter, and returns small, ranked, source-cited context packs.

No Python runtime, cloud API, account, API key, vector database, or embedding
model is required for indexing and retrieval. `ctx run` is opt-in and uses the
credentials of the selected external agent harness.

## Why ctx?

Coding agents often repeat broad Grep, Glob, and Read operations. `ctx` turns
that exploration into a reusable local index and returns token-bounded JSON with
explicit coverage (`complete`, `partial`, or `text_only`). It also generates
`.ctx/briefing.json` and `.ctx/briefing.md` for onboarding, changes, handoffs,
and impact analysis.

## Features

- Single native Rust binary
- Bundled SQLite with FTS5 and deterministic definition-first ranking
- Tree-sitter symbols, imports, calls, references, callers, and callees
- Token-bounded `search`, `graph`, and `pack` envelopes
- Repository maps, entrypoint and route heuristics, hubs, and PageRank
- Official Rust MCP SDK with four stdio tools: `ctx_search`, `ctx_graph`,
  `ctx_pack`, and `ctx_file`
- Skills and read-only explorer agents for Claude Code, Codex, OpenCode, and
  Cursor
- Harness detection, installation dry-runs, and idempotent updates
- Optional, explicit LSP reference enrichment outside the search hot path

## Supported languages and frameworks

Structural indexing covers:

- Python: `.py`, `.pyi`, `.pyw`; FastAPI, Flask, and Django heuristics
- JavaScript and TypeScript: JS, JSX, TS, TSX, modules, Vue, Svelte, and Astro;
  Express, NestJS, Next.js, Fastify, and Hono heuristics
- Go: Gin, Echo, Fiber, and Chi heuristics
- Rust: Axum, Actix Web, and Rocket heuristics
- PHP: Laravel, Symfony, and Slim route heuristics

Dynamic, generated, or metaprogrammed calls are resolved conservatively. CMS
frameworks do not receive dedicated heuristics.

## Requirements

- Rust 1.88 or newer to install from source
- Git, when Git-aware freshness information is wanted

SQLite is bundled into the binary with FTS5 enabled.

## Install

From a clone:

```console
cargo install --path .
ctx --help
```

## Quick start

```console
ctx init
ctx index .
ctx search login --json
ctx graph --op callers --symbol login --json
ctx pack "where is authentication handled?" --json
ctx explore --intent change --focus "authentication" --harness none
ctx run "where is authentication handled?" --harness codex --model gpt-5.6-luna --json
```

Indexes, maps, and briefings are written below `.ctx/`. `CTX_DIR` can
redirect these artifacts for isolated indexes and tests.

## MCP and agent harness setup

Preview detected harnesses and every planned change:

```console
ctx install --dry-run
```

Install the MCP configuration, `ctx-explore` skill, and explorer agent for all
supported harnesses:

```console
ctx install --target all
```

Refresh integrations already present in a project:

```console
ctx update --dry-run
ctx update
```

Individual targets are `claude`, `codex`, `opencode`, and `cursor`.
Existing JSON and TOML configuration is merged, and repeated installation is
idempotent.

Codex loads a project-local `.codex/config.toml` only after the project has
been marked trusted. This trust decision remains user-controlled; `ctx install`
does not change global Codex trust settings.

Run the MCP server directly with:

```console
ctx mcp
```

`ctx mcp --compact` exposes only the one-argument `ctx_pack` fast path used by
the generated Codex configuration. Other harnesses retain the full four-tool
server.

## Cached non-interactive runs

`ctx run` can execute Codex, Claude, OpenCode, or Cursor in non-interactive
read-only mode. On a clean repository, a verified answer is cached in
`.ctx/cache.sqlite`; the next identical request returns without launching the
harness and reports zero new token usage.

Run `ctx install --target opencode` or `ctx install --target cursor` before
using those adapters so their project MCP configuration is present. Codex and
Claude receive an isolated compact MCP configuration directly from `ctx run`.

Set a deterministic default and model in `.ctx/config.toml`:

```toml
default_harness = "codex"

[runners.codex]
model = "gpt-5.6-luna"
effort = "high"

[cache]
enabled = true
max_size_mb = 256
max_age_days = 30
```

Cache reuse is disabled whenever the working tree is dirty. Cache keys include
the full Git SHA, normalized question, exact model and effort, runner-binary
fingerprint, ctx prompt version, and evidence-pack digest.

JSON responses expose `cache_lookup_ms`, `harness_ms`, `validation_ms`, and
token usage so cache effectiveness can be measured without parsing logs.

```console
ctx run "where is login defined?" --harness auto --json
ctx run "where is login defined?" --harness codex --model gpt-5.6-luna --cache refresh --json
ctx cache status --json
ctx cache prune --max-age-days 30 --max-size-mb 256 --json
ctx cache clear --kind agent --json
```

## Command reference

```console
ctx init
ctx index [PATH]
ctx status --json
ctx search QUERY --mode auto|text|symbol --json
ctx graph --op def|refs|callers|callees|path|impact --symbol SYMBOL --json
ctx pack QUERY --intent explore|edit|review --json
ctx map --json
ctx explore --intent onboard|change|handoff|impact --harness none
ctx mcp
ctx run QUESTION --harness auto|codex|claude|opencode|cursor --model MODEL --json
ctx cache status --json
ctx cache prune --json
ctx install --dry-run
ctx install --target all
ctx update --dry-run
ctx update
ctx embeddings status --json
```

## Optional LSP enrichment

Tree-sitter and FTS5 remain the default offline path. LSP servers are optional
and selected explicitly:

```console
ctx lsp sources --json
ctx lsp status --json
ctx lsp fetch --dry-run
ctx lsp fetch --language rust
ctx lsp enrich . --language rust --background
```

The registry supports BasedPyright, TypeScript Native Preview, `gopls`,
rust-analyzer, and Phpactor. Official Laravel and Symfony language servers are
not currently integrated.

Treat LSP execution as trusted-project functionality: a language server may
load project configuration, start external tools, or execute application code.
The Rust client consumes reference locations only, rejects paths outside the
repository, and explicitly refuses unsupported server-to-client requests. Do
not run LSP enrichment on an untrusted repository without a sandbox.

GitHub release downloads are checked against a published SHA-256 digest when
available. This is an integrity check, not an independent publisher
attestation.

## Embeddings roadmap

The configuration reserves local and API embedding providers, but embeddings
remain disabled and no model or network is used. `ctx embeddings status` shows
the inert configuration. Setup and indexing intentionally fail with a readable
message until the later opt-in hybrid retrieval release.

## Development

```console
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

The Rust acceptance suite covers indexing, every supported grammar, search,
graph traversal, context packing, maps, briefing generation, harness
installation, CLI errors, and the MCP stdio handshake and tool calls.

## Benchmark

The benchmark generates a mixed Python/TypeScript/Go/Rust/PHP repository and
measures cold indexing, unchanged reindexing, search, graph traversal, and
context packing:

```console
cargo run --release --example benchmark -- \
  --files 1000 \
  --iterations 100 \
  --warmups 10 \
  --output benchmarks/results/latest.json
```

See [benchmarks/README.md](benchmarks/README.md) for the methodology. The
command writes the complete environment and percentiles to the requested JSON
path. Results are synthetic measurements, not guarantees for every repository.

A separate [real-repository competitive benchmark](benchmarks/competitive/README.md)
pins ten public repositories and four competing MCP tools. Its corpus and
forty source facts are verified; cross-tool results will only be published once
the isolated competitor runs are complete.

Latest measured release run (2026-09-04, Ryzen 7 3700X, Linux x86_64,
1,002 indexed files):

| Operation | Result |
|---|---:|
| Cold index | 190.28 ms / 5,266 files/s |
| Unchanged reindex | 36.25 ms |
| Symbol search p50 / p95 | 1.64 / 1.72 ms |
| Text search p50 / p95 | 1.19 / 1.25 ms |
| Definition graph p50 / p95 | 0.53 / 0.56 ms |
| Callers graph p50 / p95 | 0.60 / 0.71 ms |
| Context pack p50 / p95 | 6.38 / 6.61 ms |

Raw measurements and the full percentile table are available in
[`benchmarks/results/latest.json`](benchmarks/results/latest.json).

Agent-level A/B on the ten-file fixture, using Codex CLI 0.153.1 with
`gpt-5.6-luna` at high reasoning effort (median of three runs):

| Variant | Wall time | Input tokens | Output tokens | Tool calls | Accuracy |
|---|---:|---:|---:|---:|---:|
| Shell search baseline | 22.91 s | 61,192 | 568 | 3 | 4/4 |
| Compact `ctx_pack` MCP | 17.90 s | 45,715 | 466 | 1 | 4/4 |
| `ctx run`, clean cache miss | 15.58 s | 46,551 | 506 | 1 | 4/4 |
| `ctx run`, exact cache hit | 0.01 s | 0 | 0 | 0 | 4/4 |

On this controlled fixture, compact MCP was 21.9% faster than shell search and
used 25.3% fewer total input tokens. Against the original pre-optimization MCP
snapshot, it was 43.6% faster and used 61.7% fewer input tokens. An exact clean
cache hit avoided the harness entirely and returned in 15 ms internally. The
uncached-input difference between fresh MCP and shell runs was only 0.4%, so
provider prompt caching explains part of the total-token gap.

Native retrieval improved for indexing and most queries versus the prior
same-host snapshot, but context-pack p50 regressed from 4.724 ms to 6.380 ms
(+1.656 ms). See [the complete methodology, percentiles, raw runs, and honest
before/after notes](benchmarks/README.md#codex-exec-ctx-mcp-versus-shell-baseline).

## Project status

`ctx` is an early MVP. Symbol resolution and framework detection remain
best-effort for dynamic code. See [PRD.md](PRD.md) for the product contract.
