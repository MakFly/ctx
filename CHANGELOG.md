# Changelog

## Unreleased

### Security

- `ctx_pack` expansion (`expand <path>:<start>-<end>`) now accepts only indexed
  files that resolve inside the repository root. Absolute paths, `..` segments
  and symlinks leaving the root are refused, and the expanded span obeys the
  requested token budget.
- `ctx lsp fetch` now refuses a release asset without a published SHA-256
  digest, an asset name that is not a plain file name, a download URL outside
  the server's own GitHub releases, and assets above 256 MiB. Downloads stream
  to disk and the Python wheel must be a `basedpyright-*.whl`.
- `ctx install` and `ctx update` refuse destinations whose canonical target
  leaves the project (symlinked files or directories), write atomically, and
  keep in-project symlinks and file permissions.

### Fixed

- Indexing no longer fills the disk: below `index.min_free_mb` (default
  512 MiB, `0` disables) free space on the index disk, it aborts without
  publishing, keeps the previous index and truncates the WAL.
- MCP server instructions again tell agents to answer from the hits.
- `ctx status` reports new and deleted admitted files: `stale` now uses the
  watcher's drift check and a `drift` object gives added, modified and
  removed counts.
- The static graph follows renamed imports (`import { foo as bar }`,
  `from m import foo as bar`, `use a::foo as bar`), prefers same-file then
  imported definitions for homonyms, keeps same-name methods of one file
  apart, and reports `partial` coverage for unresolved homonyms. Schema 5;
  existing indexes migrate and refresh their graph once.
- LSP enrichment stores each language atomically and reports `stale_index`
  instead of writing over a newer index publication.
- Reindex requests and metrics events are no longer lost when queued during
  a drain, and snapshots left by a crashed worker are replayed.

## 0.3.0 - 2026-09-15

### Added

- RTK-style global and project metrics dashboards with ANSI terminal colors.
- Per-operation counts, context tokens, duration, efficiency and savings.
- Exact no-ctx baseline pairing for `ctx run` measurements.
- Estimated MCP savings based on cited source files.
- Configurable model pricing and estimated saved cost reporting.
- Asynchronous metrics aggregation and background reindex hooks for harnesses.
- Incremental indexing progress reporting and harness MCP integrations.

### Validation

- `cargo check --locked`
- `cargo test --lib metrics::tests --locked`
- `cargo test --lib --test acceptance --test harness --test mcp --locked`
- `cargo build --release --locked`

### Limitations

- Provider input and output tokens remain unknown for MCP-only events when the
  harness does not expose model usage.
- Model-less MCP cost estimates require one configured pricing model.
- The full workspace test suite still has a pre-existing CLI acceptance failure
  in `tests/cli.rs::explore_refreshes_git_and_non_git_sources_before_reusing_briefings`.
