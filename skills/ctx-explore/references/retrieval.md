# Retrieval, freshness, and metrics

Use the effective project root and CTX_DIR established during setup. Use
available tool schemas instead of assuming every MCP integration exposes the
same functions.

## Choose a query

| Need | CLI fallback | MCP when available |
|---|---|---|
| Subsystem or symbol context | `ctx pack 'login' --json` | `ctx_pack` |
| Ranked symbol lookup | `ctx search 'login' --mode symbol --json` | `ctx_search` |
| Exact text | `ctx search 'login(' --mode literal --json` | `ctx_search` |
| Regex | `ctx search 'def (login|logout)' --mode regex --json` | `ctx_search` |
| Callers | `ctx graph --op callers --symbol login --json` | `ctx_graph` |
| Callees | `ctx graph --op callees --symbol login --json` | `ctx_graph` |
| File location | Targeted native file listing | `ctx_file` |
| Expand a cited span | `ctx pack 'expand src/auth.rs:20-60' --json` | `ctx_pack` |

Replace example symbols and paths with observed ones. Natural-language ranked
queries are lexical searches, not embedding-based semantic retrieval. Exact
symbol names and narrower questions are often more useful than a long prompt.
The trigram engine accelerates eligible literal/regex searches; ordinary packs
use ranked retrieval and structural relationships, not the same search path.

Compact MCP schemas accept only `query`. Full MCP `ctx_pack` currently caps the
requested evidence budget at 800 estimated tokens. CLI `pack --budget-tokens`
can request a different budget. Expansion hints may return longer spans: inspect
the result rather than assuming the normal pack budget applies to every hint.
Token estimates are not provider tokenization or total conversation usage.

`--ignore-case` applies to literal/regex searches. Rust regexes do not support
lookarounds or backreferences. Text-only evidence and static graph coverage are
different: dynamic calls and framework behavior require source verification.

Read relevant cited spans before explaining behavior or making edits. If the
pack is insufficient, expand or narrow it. Targeted native search is a valid
fallback when ctx is unavailable or still lacks evidence; avoid repeated empty
queries. Treat `coverage=partial`, truncation, and hints as evidence limits.

## Index freshness

`ctx status --json` reports the effective database, file counts, `stale`,
`watcher`, `watcher_error`, and `catching_up`. A missing local `.ctx/` is not
proof that a KonohaOS-managed index is missing.

- With an active writer, allow a bounded interval for publication or use
  `ctx reindex . --background` when a refresh is needed. Do not run a competing
  manual index or terminate its owner. Repeated errors need diagnosis, not a loop.
- With an inactive writer and missing or stale data, authorized local setup can
  run `ctx index .`. A subsequent MCP session watches changes automatically.
- Exact searches inspect live files and can fall back to scanning. Ranked and
  structural evidence can remain stale or partial until indexing catches up.
- Ignored, hidden, binary, or oversized files may be excluded. Structural
  analysis is limited to 1 MiB per file; text admission defaults to 64 MiB.
  Check configuration and admission before claiming a symbol does not exist.

Do not delete `.ctx/`, replace the database, force-index a whole tree, or install
language servers as an automatic response to partial coverage. Optional LSP
enrichment can execute project tools and is a separate scope decision. Regular
retrieval needs neither LSP setup nor embeddings.

## Read usage correctly

```sh
ctx gain
ctx gain --project
ctx gain --since-days 7 --json
ctx metrics status --json
```

Bare `ctx` lists commands. `ctx gain` reads global metrics by default, while
`--project` reads the current effective CTX_DIR. `ctx metrics report` remains
an equivalent dashboard entrypoint. For color in a terminal with NO_COLOR set,
one invocation can use `env -u NO_COLOR ctx gain`; preserve the user's global
environment preference. Piped and JSON output remain uncolored.

MCP calls, `ctx run`, and installed lifecycle hooks record events asynchronously.
Standalone CLI search, graph, and pack calls do not currently populate the
usage dashboard. A newly installed integration with no calls can have no data.
For missing data, check the displayed metrics path, reporting period, global
versus project scope, redirected CTX_DIR, and background aggregation status.

Global storage uses CTX_GLOBAL_METRICS_DIR, then `$XDG_STATE_HOME/ctx`, then
`~/.ctx`. Do not invent model usage or prices when the harness did not supply
them. Context token estimates and provider input/output totals are different.

`estimated` savings compare cited source-file sizes with returned evidence;
they do not establish a reduction in total model usage or invoices. `exact`
uses recorded provider usage and a paired baseline. `mixed` combines measured
and estimated savings and must not be described as wholly measured.

## When a comparison is requested

Archived benchmark records are historical evidence, not measurements from the
current task or installed binary. Report their date, workload, binary/profile,
and limitations. An operating-system process count proves availability, not
actual tool usage or savings.

For an authorized fresh comparison, use the same repository revision and
questions, preserve a no-ctx baseline, validate the resulting answers/citations,
and separate total input, cached input, uncached input, output, tool calls, and
elapsed time. Record index setup costs and the actual executable. A one-task
pair does not establish daily savings. Running external agents or benchmarks
must remain within the user's explicit scope and delegation constraints.
