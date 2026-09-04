# ctx — local codebase context for coding agents

`ctx` is a local-first codebase search and exploration tool for Claude Code,
Codex, OpenCode, Cursor, and other MCP-compatible coding agents. It indexes a
repository into SQLite FTS5, extracts symbols and relationships with
tree-sitter, then returns small, ranked, source-cited context packs instead of
making an agent repeatedly scan the whole repository.

No cloud API, account, API key, vector database, or embedding model is required.

## Why ctx?

Coding agents often spend many tool calls repeating broad Grep, Glob, and Read
operations. `ctx` turns that exploration into a reusable local index and emits
token-bounded JSON results with explicit coverage (`complete`, `partial`, or
`text_only`). It can also generate `.ctx/briefing.json` and a human-readable
`.ctx/briefing.md` for onboarding, changes, handoffs, and impact analysis.

## Features

- Local SQLite FTS5 search with deterministic, definition-first ranking
- Tree-sitter symbols, imports, calls, references, callers, and callees
- Token-bounded `search`, `graph`, and `pack` JSON envelopes
- Repository maps, entrypoint heuristics, hubs, and lightweight PageRank
- MCP stdio server with four tools: `ctx_search`, `ctx_graph`, `ctx_pack`, and
  `ctx_file`
- Project skills and read-only explorer agents for Claude Code, Codex,
  OpenCode, and Cursor
- Harness auto-detection, installation dry-runs, and idempotent updates
- Optional LSP reference enrichment outside the default search hot path
- Fully local operation for indexing and retrieval

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

- Python 3.12+
- A Python build with SQLite FTS5 enabled

## Install

From a clone:

```console
python -m venv .venv
.venv/bin/pip install -e '.[test]'
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
```

The local index, repository map, and briefings are written below `.ctx/`.
`CTX_DIR` can redirect these artifacts, which is useful for tests and isolated
indexes.

## MCP and agent harness setup

Preview detected harnesses and every planned file change:

```console
ctx install --dry-run
```

Install the MCP configuration, `ctx-explore` skill, and explorer agent for all
supported harnesses:

```console
ctx install --target all
```

Refresh integrations already installed in the project:

```console
ctx update --dry-run
ctx update
```

Individual targets are `claude`, `codex`, `opencode`, and `cursor`. The
`both` alias installs Claude Code and Codex integrations. Existing project
configuration is merged, and repeated installation is idempotent.

Run the MCP server directly with:

```console
ctx mcp
```

## Command reference

```console
ctx init
ctx index [PATH]
ctx status
ctx search QUERY --json
ctx graph --op def|refs|callers|callees|path|impact --symbol SYMBOL --json
ctx pack QUERY --json
ctx map --json
ctx explore --intent onboard|change|handoff|impact --harness none
ctx mcp
ctx install --dry-run
ctx install --target all
ctx update --dry-run
ctx update
```

## Optional LSP enrichment

Tree-sitter and FTS5 remain the default offline path. LSP servers are optional
and must be fetched or selected explicitly:

```console
ctx lsp sources --json
ctx lsp status --json
ctx lsp fetch --dry-run
ctx lsp fetch --language rust
ctx lsp enrich . --language rust --background
```

The current registry supports BasedPyright, TypeScript Native Preview,
`gopls`, rust-analyzer, and Phpactor. The official Laravel and Symfony language
servers are not currently integrated. Treat LSP execution as trusted-project
functionality: a language server may load project configuration, start external
tools, or execute application code. Do not run LSP enrichment on an untrusted
repository without an appropriate sandbox.

GitHub release downloads are pinned in the local manifest and checked against a
published SHA-256 digest when one is available. This is an integrity check, not
an independent security audit or publisher attestation.

## Development

```console
.venv/bin/python -m pytest
```

The acceptance fixture covers Python, JavaScript/TypeScript, Go, Rust, and PHP,
including indexing, search, graph traversal, context packing, MCP tools,
briefing generation, harness installation, and optional LSP orchestration.

## Project status

`ctx` is an early MVP. Symbol resolution and framework detection are
best-effort, especially for dynamic code. See [PRD.md](PRD.md) for the product
contract and scope.
