# tgrep integration maintenance memory

This is the durable engineering reference for updating ctx's full-content search
and live indexing. Read it alongside the code before making changes, and update
it when the integration changes. It is project documentation, not runtime state.

## Upstream and ownership

- Upstream: <https://github.com/microsoft/tgrep>.
- Imported revision: `e2007b52d2b8fe4176159d0da20c9ba4a46d5aab`.
- Local package: `ctx-tgrep`, a private path dependency in the root Cargo
  workspace. See [its manifest](../vendor/tgrep-core/Cargo.toml).
- [Provenance](../vendor/tgrep-core/PROVENANCE.md) records copied components and
  adaptations. Preserve [Microsoft's MIT notice](../vendor/tgrep-core/LICENSE).
- ctx owns traversal, decoding, SQLite, structural analysis, evidence packing,
  session lifetime and publication. No tgrep executable or TCP server is needed.

The pinned revision is the comparison base. Do not substitute a moving `main`
branch for it when determining what has changed locally.

## Where to look

| Responsibility | Local source | Upstream comparison target |
|---|---|---|
| Byte trigrams, masks, regex candidate planning | `vendor/tgrep-core/src/trigram.rs`, `query.rs` | Same files under `tgrep-core/src/` |
| External sorting and binary index construction | `vendor/tgrep-core/src/builder.rs`, `external.rs`, `ondisk.rs` | Same upstream modules; retain the local `DocumentBuilder` adapter |
| mmap readers and mutable index algorithms | `vendor/tgrep-core/src/reader.rs`, `hybrid.rs`, `live.rs`, `meta.rs` | Same upstream modules and their inline regression tests |
| Generation staging, reader leases, exact search, direct-scan oracle, result spans | [src/text_index.rs](../src/text_index.rs) | `tgrep-cli/src/matching.rs` for line/context logic; `serve.rs` for staged publication concepts |
| File admission, source-version checks, structural indexing, incremental updates | [src/indexer.rs](../src/indexer.rs), [src/config.rs](../src/config.rs) | Builder/walker changes are reference material; ctx's admission policy remains authoritative |
| SQLite schema, full-content storage and FTS triggers | [src/db.rs](../src/db.rs) | ctx-owned integration |
| Event coalescing, overflow recovery, ownership and shutdown | [src/watcher.rs](../src/watcher.rs) | `tgrep-cli/src/serve.rs`, especially `FsEventBurst`, watcher registration and stale checks |
| Ranked retrieval, graph consistency and packs | [src/search.rs](../src/search.rs), [src/graph.rs](../src/graph.rs), [src/pack.rs](../src/pack.rs) | ctx-owned integration |
| CLI/MCP contracts and session activation | [src/main.rs](../src/main.rs), [src/mcp.rs](../src/mcp.rs) | ctx-owned interfaces; do not copy tgrep's CLI wholesale |
| Answer-cache compatibility | [src/runner.rs](../src/runner.rs), [src/cache.rs](../src/cache.rs) | `PROMPT_VERSION` and evidence-digest inputs |

The `skills/CLAUDE.snippet.md` and `skills/AGENTS.snippet.md` files are templates
installed into consumer repositories. They are not this repository's maintenance
instructions; do not inject links to this internal document into other projects.

## Current contracts and invariants

**Retrieval.** Existing `auto`, `text`, and `symbol` modes remain ranked searches.
Only explicit `literal` and `regex` modes use the exact matcher. Their
`ignore_case` option defaults to false; CLI spelling is `--ignore-case`.
Rust regex syntax is supported, including inline flags, but PCRE lookarounds
and backreferences are rejected. Queries without safe trigram constraints,
case-insensitive queries and regexes containing inline-group syntax use the
conservative scan plan, without opening a trigram generation. Any future pruning
optimization must preserve every match, including Unicode matches. Concatenated
regex alternatives retain conjunctive child plans (`QueryPlan::All`) instead of
losing their OR constraints; this is a runtime planner change, not an index
format change.

Both indexes consume the same UTF-8-lossy document returned by `read_document`.
Do not reread source files independently for SQLite and trigram extraction.
Exact searches use `read_search_document`, sharing the same validated byte reader
and decoding but omitting the SHA1 digest that only indexing needs.
Exact search checks current admission and file versions; newly added or changed
files bypass pruning, and unusable indexes fall back to scanning. Version checks
are needed only for paths absent from the candidate set; candidate files are
always read. The SQL statement is prepared once and reused within the request.
Context spans
include two surrounding lines, merge adjacent/overlapping ranges, and respect
the existing result/token limits. Missing evidence and shortened output must
remain explicit. Full-content FTS storage is separate from display excerpts.
Exact search packs evidence progressively and stops constructing snippets after
the token budget closes. It still inspects subsequent matches/files as needed
to preserve result-limit and unreadable-file reporting; this is not a scan cutoff.

**Bounds and admission.** `[index].max_file_mb` defaults to 64;
`[index].buffer_mb` defaults to 64 and bounds the external-sort arena, not total
RSS. Tree-sitter remains limited to 1 MiB per file. Unknown extensions can yield
text evidence. Indexing processes one prepared document at a time. Preserve
shared hidden-file/ignore behavior and exclude generated ctx artifacts,
including a redirected `CTX_DIR`. Do not equate text coverage with structural
coverage, or with coverage of ignored/oversized files.

**Publication.** SQLite schema version and `text_index::FORMAT` currently are
both `4`, but they describe separate compatibility boundaries. SQLite's
`text_generation` points to immutable files under the effective `CTX_DIR/text/`.
`stage_generation` reads the same SQLite transaction as the graph update, writes
and synchronizes a new generation, then records its reference in that transaction.
The reference becomes visible only when the transaction commits. Preserve
rollback cleanup and the previous usable generation. Never overwrite files
underneath an existing mmap reader.

Changed documents become a bounded delta merged with the old disk index. The
mutable layer is flushed at each coalesced update; it is not exposed ahead of
the SQLite commit. Unchanged reindexing retains the generation. Shared reader
leases protect retired generations; cleanup retains current and previous
generations and skips older generations still leased by readers.

**Sessions.** `ctx mcp`, including compact mode, and `ctx index --watch` own
session-scoped watcher workers. One OS-held writer lock governs each index;
observers retry ownership and MCP requests also retry at their boundary.
There is no permanent daemon. MCP sessions also own a `ReaderSession`: one
cached immutable generation per effective index directory, shared by overlapping
sessions. Each request chooses the generation from its SQLite snapshot. Replacing
the cached entry releases its lease after any in-flight readers finish; ending
the last session drops the cached reader. The registry holds only weak references.
Publication/indexing still uses uncached readers. Drop/shutdown must stop and join
workers and release watcher registrations and locks. Preserve the user's unrelated
services.

The same session lifetime owns a 16-entry LRU of compiled matchers, keyed by
query, literal/regex mode and case folding. Cached patterns are at most 4,096
bytes; compilation uses a 256 KiB compiled-size limit and 64 KiB DFA-cache limit.
These are library resource controls, not a total RSS guarantee. Oversized queries
or bounded-compilation failures use the original uncached compiler, preserving
accepted syntax. The last session releases the cache; in-flight Arc users may
finish normally. Query plans are not cached.

Coalescing uses 200 ms quiet time, a two-second maximum burst window and a
16,384-event queue. Lost events/overflow and changed ignore rules trigger full
reconciliation; startup and hourly reconciliation repair drift. Linux registers
admitted directories plus relevant Git metadata. Other backends watch the root
recursively and filter excluded artifact events. Do not claim ignored trees
are unsubscribed on every platform. Pending updates, stale evidence or a
generation change during retrieval must not silently yield a consistent graph.

## How to approach an update

Compare the pinned upstream revision with a specific proposed replacement
revision in a separate checkout. Inspect upstream release notes and fixes to
candidate correctness, index format, mmap lifetime, memory bounds, watcher
overflow and publication. Select relevant changes; never overwrite the vendored
directory blindly. The local manifest, `DocumentBuilder`, ctx publication layer
and session coordinator must survive unless their replacements are explicit.

For each selected change, compare three versions: the old upstream file, our
adapted file, and the new upstream file. Carry over relevant upstream regression
tests. Prefer the existing vendored algorithms over an independent rewrite, but
keep ctx's file admission and SQL transaction boundaries intact. Watcher and
matching fixes from the upstream CLI need an adaptation in ctx, not a second
server or CLI dependency.

If SQLite layout changes, update `SCHEMA_VERSION` and transactional migration
logic. If persisted retrieval semantics or the binary format changes, update
`text_index::FORMAT` and implement forced rebuilding or an explicit compatible
migration; merely changing the marker is not proof that old generations are
safe. Revisit `PROMPT_VERSION` (currently `ctx-run-v4-fulltext`) and evidence
digests when answer-cache assumptions change. Update the root `Cargo.lock`
without unrelated dependency upgrades and retain the Rust 1.88 requirement
unless a deliberate compatibility change is authorized.

Update the provenance, this memory, user-facing README and changed CLI/MCP
contracts together. Record the new revision, migration choice, actual validation
and remaining limitations. Never present old benchmark tables as new results.

## Verification and evidence

Non-compiling checks include `git diff --check`, targeted `rustfmt --check`,
and `cargo metadata --locked --offline --format-version 1`. An offline metadata
check requires dependencies to be cached. Formatting and dependency resolution
do not prove that Rust code typechecks or that behavior is correct.

When compilation-based validation is authorized by the current user instructions,
use the root workspace tests so the vendored inline tests are included:
`cargo test --workspace --all-targets`. Focus on these existing suites:

- [tests/text_retrieval.rs](../tests/text_retrieval.rs): tail content, large
  unparsed files, indexed/direct-scan parity, Unicode, multiline, incremental
  deletion, corrupt generations, rollback, reader leases and output bounds.
- [tests/watch.rs](../tests/watch.rs) and watcher inline tests: creations,
  renames, exclusions, ownership takeover, process death, coalescing and rescan.
- [tests/cli.rs](../tests/cli.rs), [tests/mcp.rs](../tests/mcp.rs) and
  [tests/acceptance.rs](../tests/acceptance.rs): public contracts, migrations
  and structural retrieval compatibility.
- [examples/benchmark.rs](../examples/benchmark.rs) and
  [benchmarks/README.md](../benchmarks/README.md): paired direct-scan/trigram
  timings, index sizes and RSS methodology. Compare identical admitted files,
  queries and machines; include indexing costs. Linux `VmHWM` values are
  process-lifetime peaks, not isolated per-phase allocations.

Keep “no missing exact matches relative to the direct scanner” as a correctness
gate for pruning changes. Exercise interrupted publication and concurrent
readers when touching generations; exercise shutdown and lost notifications
when touching the coordinator. Check case folding, decoding and ignore policy
together rather than treating each as an isolated optimization.

**Recorded integration status (2026-09-07):** targeted Rust formatting/syntax
checks, locked Cargo metadata resolution and `git diff --check` passed. A scratch
SQLite check exercised full-content highlighting, metadata-only updates,
content replacement and cascade deletion. Rust compilation, the Rust test suites
and new benchmarks were not executed because compilation was prohibited in
that session. They remain unverified; this document does not authorize running
them or starting a development server. No service was launched for validation.

**Debugging follow-up (2026-09-07):** reproduced an FTS citation defect with
SQLite: a literal SOH character in source was mistaken for an inserted highlight
marker, reporting line 1 instead of line 302. `append_file_hits` now selects a
marker absent from that document when the normal marker collides. The regression
test also covers a collision with the first fallback marker. The SQLite
reproduction and corrected query were exercised without compiling Rust; the
new Rust regression test remains unexecuted under the same compilation constraint.


**Authorized release validation (2026-09-07):** the user subsequently authorized
building and retesting. `cargo build --release --locked` succeeded, and
`cargo test --workspace --all-targets --locked` passed all 290 tests (including
236 vendored tests), with no failures or ignored tests. This executes the FTS
marker-collision regression described above and supersedes the earlier
unverified compilation/test status.

The freshly built release CLI was also exercised on an isolated FastAPI checkout
at `50113da16fec53b66b80d75e80a89296de4fa5a5`: 2,952 files, 5,755 symbols and
20,687 edges indexed; a second index reported zero changed files. Literal, regex,
symbol and ranked text searches located `solve_dependencies` in
`fastapi/dependencies/utils.py`. A release `index --watch` process correctly
published creation, replacement and deletion of a scratch text file; searches
returned the new content and stopped returning the replaced/deleted content.
The validation watcher was terminated and reaped. No development server was
started. No new performance benchmark was run; existing timing tables remain
historical results.


**Paired CLI speed validation (2026-09-07):** `benchmarks/search_speed.py`
executed the existing release binary without rebuilding. Seven query scenarios
across three synthetic corpora (100 and 1,000 files of 16 KiB, and 100 files of
1 MiB) produced 21 comparisons, each with two warmup and ten measured pairs.
All paired evidence matched. The baseline is the deployed scan fallback with
an empty generation reference, not a direct library oracle invocation. Selective
queries benefited; common/unprunable cases frequently regressed. Raw samples,
binary hashes, timings, index sizes and limitations are recorded in the two
`benchmarks/results/search-speed*.json` files and the benchmark README. This
supersedes the earlier statement that no new performance benchmark was run;
MCP/LLM costs and watcher RSS remain unmeasured. Temporary corpora were removed
and all benchmark child processes exited.


**Code-focused speed validation (2026-09-07):** the benchmark now also accepts
`--fastapi-repo`, extracts committed Python blobs without changing the source
checkout, and measures eight code queries on the core (48 files) and all Python
sources (1,138 files). All 16 paired scenarios passed evidence parity at commit
`50113da16fec53b66b80d75e80a89296de4fa5a5`. Results and raw samples are documented
in `benchmarks/README.md` and `benchmarks/results/search-speed-fastapi.json`.
Selective gains were more modest than on repetitive synthetic text, and several
queries regressed. Do not generalize the synthetic speedups to code search.
No build or persistent service was started; disposable corpora were cleaned up.


**Search regression corrections (2026-09-07):** implemented direct-scan dispatch
for MatchAll plans, candidate-only avoidance of unnecessary version SQL (with a
cached prepared statement for excluded paths), session-owned generation readers,
and nested conjunctive/alternative query plans without distributive expansion.
The SQLite schema, binary generation format and public query semantics are
unchanged, so no schema/FORMAT/prompt-version bump or rebuild migration is needed.
The pinned upstream revision remains unchanged; `query.rs` now contains a local
planner adaptation in addition to the existing builder adapter.

The workspace/all-targets test run passed 293 tests, followed by all eight
text-retrieval tests after adding a further live-change/session regression
(294 distinct passing tests in total). New coverage checks MatchAll avoids
opening readers, cache reuse/publication snapshot selection/lease release,
nested regex pruning with masked and plain postings, and changed/new/deleted
code through cached sessions. Existing Unicode, rollback, corruption, CLI, MCP
and watcher tests passed. The release binary was not rebuilt; historical release
speed tables must not be presented as measurements of these corrections.

The corrected test-profile CLI also passed all 16 FastAPI benchmark scenarios
with identical evidence against its own scan fallback. A separate `strace`
check on the same 1,138 Python files confirmed that the function-alternative
regex opens 3 source files (previously 915), and the decorator regex opens 800
(previously 1,138), with the same hit counts. Short and ignore-case queries no
longer open `lookup.bin` or `index.bin`. These are work-count/correctness checks,
not release latency comparisons. Formatting and `git diff --check` passed;
all validation processes exited and temporary source corpora were removed.


**Corrected release validation (2026-09-07):** following explicit user go-ahead,
`cargo build --release --locked` succeeded. The corrected release passed all 16
FastAPI paired scenarios with identical evidence. The raw result is
`benchmarks/results/search-speed-fastapi-fixed-release.json`; the benchmark
README compares it with the preserved previous release snapshot and records
remaining regressions and measurement limits. This supersedes the earlier
unmeasured-release status. No code changed after the 294 passing tests.
No persistent service was started; benchmark processes exited and disposable
corpora were removed.


**Persistent MCP validation (2026-09-07):** `benchmarks/mcp_speed.py` ran the
existing corrected release through full stdio `ctx_search` on FastAPI's 1,138
Python files. Eight scenarios, two warmup pairs and ten measured pairs each,
matched fresh indexed CLI evidence exactly. Measurements include buffered
transport and JSON decoding on both paths; the watcher completed startup before
sampling. Selective queries benefited, while several broad queries remained
slower. This compares whole request paths, not isolated cache savings or LLM
cost. The same cached session passed watcher create/replace/delete visibility
checks and shut down gracefully on stdin close. Raw samples and methodology are
in `benchmarks/results/mcp-speed-fastapi.json` and the benchmark README. No build
or persistent service was left running.


**MCP search response adaptation (2026-09-07):** `ctx_search` now explicitly
retains its Envelope output schema and emits one complete `structuredContent`
object without a redundant text copy for MCP clients using protocol 2025-06-18
or newer. Older/unknown clients retain the previous full text-plus-structured
response. This deliberately avoids the SDK's `CallToolResult::structured`
constructor on the modern path, which otherwise serializes both copies. No hit,
citation, budget or search semantics changed; other full tools and compact pack
retain their contracts. No index/schema/prompt-version migration is needed.
The MCP specification recommends a text copy for backward compatibility; ctx
keeps that copy for pre-structured-output protocol versions.

`cargo test --locked --lib --test mcp` passed 22 tests, including real stdio
search/schema checks and a regression verifying complete modern/legacy evidence
and reduced serialization size. The release executable has not been rebuilt for
this response adaptation; prior release latency tables remain historical.

Both actual protocol paths subsequently passed all eight FastAPI MCP/CLI
scenarios and watcher create/replace/delete checks using the debug test binary.
Raw diagnostics are in `benchmarks/results/mcp-response-{modern,legacy}-test.json`.
Wire sizes drop by roughly half for non-empty results without changing evidence;
see the benchmark README for exact sizes and protocol compatibility. Both servers
exited gracefully; no validation process remains. Formatting and diff checks
passed. Corrected release latency remains unmeasured for this response change.


**MCP response release retest (2026-09-07):** the user explicitly requested
rebuild and retest. `cargo build --release --locked` succeeded; all 295 workspace
and all-target tests passed. The corrected release passed eight FastAPI MCP/CLI
scenarios under each of protocols 2025-06-18 and 2025-03-26, with complete evidence
parity, live create/replace/delete visibility and graceful shutdown. Raw release
samples are `benchmarks/results/mcp-response-{modern,legacy}-release.json`; the
benchmark README records timings and remaining small CLI/MCP regressions.
This supersedes the prior unmeasured-release status for the response adaptation.
No validation processes remain; disposable corpora were removed.


**Further optimization research (2026-09-07):**
[Lossless search optimization research](SEARCH_OPTIMIZATION_RESEARCH.md) records
28 proposals and additional long-term alternatives, mapped to current code and
primary sources. These are not implemented or benchmarked changes. In particular,
a quiet watcher is not an authoritative freshness barrier and does not justify
removing live traversal without a stronger consistency contract. Digest omission,
lazy evidence budgeting and session query caches are proposed first experiments.

**First lossless optimization batch (2026-09-07):** those three experiments are
now implemented in ctx: exact-search digest omission, progressive snippet
packing, and bounded session matcher caching. No vendored source or index format
changed. All 298 workspace/all-target tests passed, including decode/admission
parity, cache eviction/flags/fallback/lifetime and 2,100 eager/progressive budget
comparisons. An independent frozen pre-batch release versus current debug CLI
check passed 140 complete JSON comparisons (excluding elapsed time), covering
multiple files, Unicode, invalid UTF-8, CRLF, multiline/empty regex matches,
missing queries, result limits and token budgets. Formatting passed. No release
build or release performance benchmark was run for this batch, so no speedup is
claimed. The temporary comparison corpus was removed and test processes exited.

The subsequent requested benchmark used the existing debug/test binary without
a build. Eight FastAPI MCP scenarios passed CLI/MCP evidence parity, followed
by watcher create/replace/delete checks and graceful shutdown. Historical
test-profile MCP medians improved 9–37% in seven scenarios, with a 2% regression
for the absent literal. Unlimited budgets do not measure progressive snippet
closure; no release speedup is established. See the benchmark README and
`benchmarks/results/mcp-lossless-modern-test.json` for samples and limitations.
