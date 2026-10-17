---
name: ctx-explore
description: Set up, use, and troubleshoot ctx local code search across Claude Code, Codex, Grok, OpenCode, and Cursor. Use for ctx installation or updates, repository indexing, cited code research, MCP connection problems, and ctx usage metrics.
---

# ctx: setup, code research, and usage

Make the requested repository searchable and return enough cited evidence to
answer the user's question. Use the current agent and the existing ctx route;
the skill does not require a second model session or delegated explorer.

## Choose the requested work

- **Install, connect, update, or repair:** read [setup.md](references/setup.md).
  Distinguish installing the binary from configuring a project and making this
  skill discoverable. Verify the affected commands and configuration.
- **Find or understand code:** use an available ctx MCP tool, or
  `konoha_code_context` when that is the established project integration.
  Read [retrieval.md](references/retrieval.md) when choosing a search mode,
  handling incomplete results, or investigating freshness.
- **Explain usage or savings:** use `ctx gain`; the metrics section of
  [retrieval.md](references/retrieval.md) explains scope, missing data, and
  the difference between estimates and a measured comparison.

## Repository and transport

Establish the repository root from the user's request and actual working
directory. Inspect the effective `CTX_DIR` before interpreting status or
changing an index. A repository's `.ctx/` and a KonohaOS-managed index can be
different stores for the same code.

Prefer the existing working integration. When KonohaOS already provides code
context, reuse it; do not add another direct MCP server or create a second index
merely to answer a code question. Resolve redirected paths from configuration
or the running ctx process, never by guessing a workspace hash.

Tool names can have harness prefixes. Inspect their actual schemas: compact
Codex/Grok integrations expose a query-only `ctx_pack`; full integrations also
expose search, graph, and file tools. Use the supported arguments, with CLI
`--json` as a fallback when it can access the same effective index.

## Evidence quality

Start with an exact symbol when one is known, or a focused subsystem question.
Use an existing briefing only when it is relevant and current. A pack may
contain signatures and relationships rather than complete function bodies.
Expand the cited span when the requested fact requires the body.

Answer when the evidence is sufficient. For a missing fact, use a relevant
expansion hint or a narrower query; do not repeat an empty or unchanged hint
indefinitely. If ctx is unavailable or still lacks the evidence, use targeted
file reads or native search within the user's permissions. Missing hits and
`coverage=partial` do not establish that a file, symbol, or relationship is absent.

Cite `path:line` references from inspected evidence. Read the affected source
before editing it. Report material coverage or freshness limits without turning
every response into a tool transcript.

## Boundaries and completion

Honor the requested scope and existing authorization. An installation request
permits the relevant local setup; a code question does not automatically permit
installing binaries, changing harness settings, downloading language servers,
or launching an external agent. In a read-only explorer, report required setup
to the caller. Do not use `ctx run` for ordinary code retrieval.

Do not manually index over an active writer, kill user sessions to acquire a
lock, or delete an index as a routine repair. Follow the freshness guidance in
the reference. Embeddings are currently disabled; they are not a setup dependency.

For setup, verify the selected integration and retrieve a known source symbol
when possible. Distinguish files installed, discovery confirmed, and an actual
successful MCP call. A skill may require a new session or a harness refresh to
appear. Never claim a runtime check, benchmark, build, or restart that did not run.

For ctx development, follow its repository's installation policy after changing
the executable or embedded skill assets. Existing project copies are refreshed
with `ctx update`; updating the binary alone does not rewrite them.
