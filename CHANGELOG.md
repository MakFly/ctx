# Changelog

## Unreleased

### Added

- Text output for `ctx search`, `ctx graph` and `ctx pack` now groups hits by
  file, prints compact source evidence and reports the printed-token estimate.
  JSON output is compact when stdout is piped and remains pretty in a terminal.
- `ctx read <file>` reads a line range (`--lines A:B`, `--raw` byte-exact),
  one definition (`--symbol NAME`) or an outline of the file's definitions
  (`--outline`) within a token budget, and names what it elided. No index
  needed.
- `ctx metrics import-bench` records paired benchmark rows as measured
  savings.
- `ctx compact` groups `path:line:text` search output by file, names the
  function holding each group of matches, and appends the source the search
  points at (matched definitions, then the lines around the densest groups)
  within a token budget. `ctx compact --run -- ...` runs a `--and`/`--then`
  list of known-safe `rg`/`grep` searches and plain reads itself, from PATH and
  without a shell, and exits with the shell's status for it.
- `ctx hook rewrite` (PreToolUse, Claude and Codex) turns an agent's search and
  plain-read lists into `ctx compact --run`; `ctx hook compact-output`
  (PostToolUse, Claude Grep) compacts Grep content results in place.
  `ctx install --target claude` adds both hooks. The rewrite hook also reads
  Grok payloads (`--harness grok`).
- `ctx hook install` and `ctx hook uninstall` (`--harness
  claude|codex|grok|all`, `--home DIR`) add or remove exactly ctx's search
  hooks in each harness's user configuration, keeping everything else.

### Changed

- `ctx gain` no longer presents an estimate as a saving: estimates show as
  context avoided against whole cited files, without a percentage or the
  efficiency meter, and measured pairs are reported apart, negative included.

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

- Global and project metrics dashboards with ANSI terminal colors.
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
