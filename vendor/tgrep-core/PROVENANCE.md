# tgrep components used by ctx

For the code map, update strategy, compatibility rules and validation status,
read [the maintenance memory](../../docs/TGREP_MAINTENANCE.md).

Upstream: https://github.com/microsoft/tgrep
Revision: `e2007b52d2b8fe4176159d0da20c9ba4a46d5aab`
License: MIT; the original Microsoft notice is preserved in `LICENSE`.

This private path dependency contains the upstream `tgrep-core/src` modules and
their inline tests. The package manifest is local to ctx and participates in the
root Cargo workspace, so upstream tests are included by `cargo test --workspace`. It is not a standalone
installation of the tgrep executable, and it does not run its TCP server.

## Integration changes

- `query.rs` retains OR subplans within concatenations using `QueryPlan::All`.
  Execution intersects child candidate sets without distributing alternatives,
  preserving bounded plan size. MatchAll is the identity for conjunction and
  absorbs disjunction. Local regression tests cover code identifiers, decorators,
  multiple alternative groups and a short alternative with conservative recall.

- `builder.rs` adds `DocumentBuilder`, an adapter around the original external
  sorter and the live index's mask extraction. It accepts the exact decoded
  content saved in the ctx SQLite transaction, rather than rereading files.
- ctx stages changed documents as a bounded delta and uses the original
  `merge_index_with_delta` to stream that delta and the immutable disk reader
  into a new generation. The mutable layer is flushed at every coalesced update;
  it is not exposed ahead of the corresponding SQLite graph transaction.
- The original hybrid reader/query planner and positional masks select exact
  search candidates. Inline regex flags and case-insensitive searches use the
  conservative full-scan plan, including Unicode case folding.
- ctx owns traversal, file size policy, decoding, source versions, and output.
  Both indexes consume the same UTF-8-lossy text; the tgrep encoding/walker
  modules remain internal dependencies for the upstream builder/test surface.
- `src/text_index.rs` adapts lazy line-location/context extraction from
  `tgrep-cli/src/matching.rs` (`LineIndex`). It returns ctx evidence hits.
- `src/watcher.rs` adapts `FsEventBurst` coalescing, overflow-triggered stale
  checks, bounded deadlines, directory registrations and periodic reconciliation
  from `tgrep-cli/src/serve.rs`. The TCP request handlers and process discovery
  are not imported.
- Publication follows upstream's staged-file principle, using a SQLite generation
  reference instead of moving files underneath a live reader. Shared file leases
  protect retired generations. Current and previous generations are retained.

Retain this provenance and the MIT notice when moving or redistributing these
components. Changes to upstream modules should remain narrow; update the pinned
revision deliberately and review upstream regression tests before upgrading.


## Validation

On 2026-09-07, the authorized release build and all 290 workspace tests passed,
including 236 vendored tests. The release CLI also passed isolated FastAPI
search and watcher smoke checks. See the maintenance memory for the checkout
revision, scope and remaining benchmark limitation.

The subsequent paired CLI scenario benchmark passed evidence parity in all
21 comparisons. See `benchmarks/README.md` for measured gains, regressions,
raw samples and the scan-fallback methodology; these are synthetic CLI results.

The code-focused FastAPI follow-up passed all 16 paired scenarios on committed
Python sources. Its measured gains and regressions are recorded separately in
`benchmarks/results/search-speed-fastapi.json`; see the benchmark README.

The search-regression follow-up passed 294 distinct workspace tests across the
all-targets run and final text-retrieval run. Session reader caching and MatchAll
bypass live in ctx, not in the vendored index; the binary format is unchanged.
No corrected release performance measurement has been made yet.

The subsequently authorized corrected release build succeeded and passed all
16 paired FastAPI scenarios. Results are in
`benchmarks/results/search-speed-fastapi-fixed-release.json`, with methodology
and the preserved before/after comparison in the benchmark README.

Full persistent MCP validation subsequently passed eight CLI/MCP evidence-parity
scenarios and live create/replace/delete checks with graceful shutdown. See
`benchmarks/results/mcp-speed-fastapi.json` and the benchmark README; no vendored
code changed for this validation.

A subsequent ctx-only MCP search serialization change removes redundant text
for structured-output protocol clients, retaining legacy text compatibility.
The 22 targeted library/MCP tests passed; vendored code and index format are
unchanged. See the maintenance memory for protocol details and validation limits.

The MCP response adaptation was subsequently rebuilt in release at the user's
request: all 295 workspace tests passed, followed by eight real-code scenarios
under each protocol version and graceful shutdown. See the two
`benchmarks/results/mcp-response-*-release.json` records and benchmark README.

The next ctx-only batch omits unused exact-search digests, progressively packs
snippets under the evidence budget, and caches bounded session matchers. Vendored
source and index format remain unchanged. All 298 workspace/all-target tests and
140 frozen-release/current CLI evidence comparisons passed. No release build or
performance measurement was made for this batch; see the maintenance memory for
cache bounds, fallback behavior and validation scope.

The subsequent requested benchmark used the existing debug/test binary without
a build. Eight FastAPI MCP scenarios passed CLI/MCP evidence parity, followed
by watcher create/replace/delete checks and graceful shutdown. Historical
test-profile MCP medians improved 9–37% in seven scenarios, with a 2% regression
for the absent literal. Unlimited budgets do not measure progressive snippet
closure; no release speedup is established. See the benchmark README and
`benchmarks/results/mcp-lossless-modern-test.json` for samples and limitations.
