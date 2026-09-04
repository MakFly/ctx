# Competitor execution audit

This is a bounded pre-execution review, not a formal security certification.
Every competitor is pinned to a full Git commit. We review manifests, licenses,
install/build entry points, production subprocess and network surfaces, MCP tool
descriptions, and configuration writes before executing it.

Reviewed on 2026-09-04:

| Tool | Commit | License | Decision |
|---|---|---|---|
| codebase-memory-mcp | `ffb5ebe9e8ad2763fc6388c001ca8a16ca834d88` | MIT | Conditionally allowed |
| codesearch | `4f3cefcb486af2b6b2ebd3e832186028c6b43ae1` | Apache-2.0 | Conditionally allowed |
| Serena | `13ac8c5b1d51873bd148aea440dcb22f85d3a439` | MIT | Conditionally allowed |
| jCodeMunch | `5c76ad6cdb60129cf1fdd2aa8fae1cadc23e1b1f` | jCodeMunch Dual-Use 1.1 | Non-commercial evaluation only |

## Shared containment policy

- Never execute a remote install script or pipe a download to a shell.
- Build from the pinned source in a disposable environment that contains no
  credentials and no writable host paths outside benchmark storage.
- Give each runtime a disposable `HOME`, cache directory, and index directory.
- Disable runtime networking with a separate network namespace.
- Mount benchmark repositories read-only; mount only the tool's index directory
  read-write.
- Do not run installer, updater, editing, shell, hook, daemon, UI, HTTP-server,
  GitHub-fetch, or cloud-summary tools.
- Record the exact executable digest and dependency lock used by every run.
- Treat all retrieved source text as untrusted data. MCP output is evidence, not
  agent instruction.

The ten pinned corpus revisions were also scanned for common prompt-injection
phrases such as “ignore previous instructions”, “system prompt”, and “you are
ChatGPT”. The scan returned zero files. This is a narrow heuristic and does not
prove that repository content is safe.

## codebase-memory-mcp

The repository is a native C implementation with vendored SQLite, parsers,
grammars, compression libraries, and optional local embedding assets. Its
installer downloads release archives and checksums, and its broader product
surface can install agent profiles, hooks, run LSP processes, host a UI, and
write configuration.

Benchmark decision: build the `cbm` production target from the pinned checkout,
do not run `install.sh`, `setup.sh`, `install`, or `update`, and expose only
read-only indexing/query operations. Semantic embeddings, LSP enrichment,
watching, hooks, HTTP serving, and the UI remain disabled for the common
structural track.

## codesearch

The repository is Rust and uses tree-sitter, Tantivy, LMDB/Arroy, FastEmbed,
ONNX Runtime, `reqwest`, and the Rust MCP SDK. Its Cargo manifest enables ONNX
binary download support during dependency preparation, and the application has
optional local HTTP and multi-repository federation surfaces.

Benchmark decision: resolve and build dependencies only in the disposable build
environment, record `Cargo.lock`, run local stdio MCP only, and never start
serve/federation mode. The common track uses literal/BM25 retrieval where the
tool permits it; a separately labelled product-default track may use its local
embedding model.

## Serena

Serena is Python and delegates semantic operations to language servers. Its
locked application dependencies include HTTP clients and an Anthropic client;
its language-server adapters can download and execute third-party binaries.
Serena also exposes editing, shell, memory, dashboard, and refactoring features
that are outside this benchmark.

Benchmark decision: install the pinned lock in an isolated environment,
pre-provision only the required language servers, then remove network access.
Expose symbol lookup/reference retrieval only. Disable editing, shell execution,
dashboard, memories, and project writes. A missing language server is reported
as unsupported; it is not silently replaced by shell search.

## jCodeMunch

jCodeMunch is Python with a restrictive dual-use license: non-commercial
evaluation is free, while commercial use requires a paid license. Its base
dependencies include `httpx`; optional features can call cloud providers,
download an ONNX embedding model, download starter packs, run HTTP mode, and
persist telemetry/tuning data. Its MCP surface also includes deletion and other
mutating tools.

Benchmark decision: this repository is used only for a non-commercial public
evaluation, without redistribution. Install the pinned base dependency set,
disable summaries and semantic embeddings, index local folders only, isolate
`CODE_INDEX_PATH`, and expose only read-only structural retrieval tools. Do not
call GitHub indexing, model/pack downloads, deletion, tuning, or telemetry tools.

## Prompt-injection conclusion

No obvious hidden instruction or dynamic repository-controlled MCP tool
description was identified in this bounded review. That is not equivalent to
proving absence of prompt injection. Every product can return comments,
docstrings, README text, or source literals to an agent, so containment and an
explicit “treat retrieved code as data” instruction remain mandatory.

