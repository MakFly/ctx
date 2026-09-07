# Lossless code-search optimization research

Research date: 2026-09-07. Scope: the current ctx implementation, its pinned
Microsoft tgrep integration, persistent MCP search and live source updates.
This began as a proposal inventory; the research itself changed no engine code.
The subsequent authorized implementation covers digest omission in exact search,
progressive evidence budgeting and a bounded session matcher cache. The other
proposals remain unimplemented. Performance gains for this batch are unmeasured.

## Decision

There is meaningful headroom, but no evidence supporting a universal 10x gain.
Start by removing work the current caller demonstrably does not need: digest
calculation, temporary serialization, repeated query compilation and eager
construction of discarded evidence. Then improve candidate execution and bounded
parallelism. Treat a watcher-only search path as a separate consistency problem,
not a free optimization. Prototype a new index only after measuring false-candidate
cost on large real repositories.

Evidence labels used below:

- **Observed**: a cost or behavior is visible in current code or recorded measurements.
- **Proposal**: a mechanism with a plausible benefit; its gain in ctx is unmeasured.
- **Conditional**: cannot preserve the current contract without an additional proof,
  snapshot or protocol. It must not silently become the default live search path.

## What “lossless” must preserve

At an identical admitted source snapshot and with identical query parameters,
compare full ordered hits, start/end lines, snippets, scores, truncation flags,
coverage, hints and token estimates. Elapsed time is expected to differ. Preserve
Rust regex match semantics, Unicode handling, UTF-8-lossy decoding, CRLF behavior,
multiline matches, empty matches and two-line context merging. Keep admission,
ignore rules, unknown extensions and file-size limits identical.

“No additional false negatives relative to the scanner” is essential, but weaker
than complete behavioral equivalence: changing ordering can change which hits
survive a budget, and returning more context changes both evidence and tokens.
The direct scanner shares matching helpers with indexed search, so an optimization
to those helpers also needs an independent reference implementation or frozen
expected results. Two optimized paths agreeing with each other is insufficient.

Live filesystem search is not an atomic snapshot today. ctx detects many changes
using traversal and file-version checks, and reports read races as partial. It
already documents the force-reconciliation requirement for changes that preserve
all observable version attributes. A new optimization must not weaken those
existing guarantees; absolute consistency under arbitrary concurrent writes would
require a snapshot or cooperation from writers.

A quiet event queue is not proof of a current tree. Linux documents overflow and
filesystem-monitoring limitations, and not every mutation mechanism is observable.
A timer or periodic repair bounds some staleness; it does not make an omitted live
walk lossless. [Linux inotify manual](https://man7.org/linux/man-pages/man7/inotify.7.html)

## Current evidence and limitations

The latest release passed 295 tests. In the recorded FastAPI MCP run, selective
queries took roughly 18–27 ms and broad cases roughly 43–51 ms. See the
[release records](../benchmarks/results/mcp-response-modern-release.json).
The modern response adaptation already removed about half the wire bytes from
large responses; that work must not be counted again as future headroom.

The current benchmarks use eight repeated queries, ten measured pairs, one Python
project and extremely large output budgets. They are useful diagnostics, not a
representative agent workload or a stable p99 measurement. They do not identify
how much time each internal stage consumes.

Observed remaining work:

- [read_document](../src/indexer.rs) computes SHA-1 and formats a hex digest;
  [search_full_content](../src/text_index.rs) discards that digest.
- The reader also copies valid UTF-8 through `from_utf8_lossy(...).into_owned()`.
- Every query builds a new matcher and trigram plan.
- Every query walks admitted paths, sorts them and reads metadata; excluded
  candidates require version SQL. The statement is now reused within a request.
- Candidate IDs become owned strings in a `HashSet`, followed by relative-path
  conversion during traversal.
- `matching_spans` constructs newline offsets; `content_hits` then builds a second
  line collection and joins snippets. This only happens after a match, not on all files.
- `apply_budget` runs after evidence generation. `estimate_tokens` serializes each
  hit to temporary JSON to count characters; `fit_snippet` repeats work while fitting.
- The query executor obtains all child posting lists before it can exploit an
  empty or very selective first intersection.
- The watcher registers directories, indexes and registers again around updates;
  those paths still perform whole-tree metadata walks. Incremental document parsing
  is not the same as an O(changed-files) update path.

## Near-term changes with direct code evidence

| ID | Proposal and ownership | Expected beneficiary | Preservation gate and cost |
|---|---|---|---|
| 1 | Separate validated document reading from digest calculation in `src/indexer.rs`; search requests no digest, indexing still does. | Broad scans and large admitted files. | Keep every size, binary, symlink and before/after version check. Reading and indexing must still decode identically. Low implementation cost; magnitude unmeasured. |
| 2 | Reuse the owned byte buffer for valid UTF-8 instead of copying it; fall back to precisely the existing lossy conversion. | Large files and allocation-heavy scans. | Byte-for-byte decoded-text parity including invalid UTF-8 and boundary errors. No ASCII-only shortcut. |
| 3 | Cache compiled matchers and query plans in the MCP session with a byte/entry cap. | Repeated or complex patterns; no repeated compiler work. | Key all relevant flags and syntax options; bound memory and drop with session. Plans are reusable independently of candidates. Avoid caching a generation's candidate set under a query-only key. |
| 4 | Generate exact-search spans/evidence lazily and apply the existing budget as the ordered stream advances. | Normal agent budgets, where most possible hits will not be returned. | Same first-hit shortening, context merges, omission/partial hints and ordering. Confirm whether later errors affect the returned envelope before stopping early. Ranked modes need separate ranking proofs. |
| 5 | Use one line-offset representation for span location and excerpt extraction. | Broad match sets and multiline files. | Preserve CRLF stripping, final newline, empty text, UTF-8 offsets and merged context exactly. A raw slice is not automatically equivalent to `lines().join("\n")`. |
| 6 | Replace temporary JSON strings used only for token estimation with a counting serializer or equivalent exact encoded-character accounting. | Large or heavily escaped snippets; tight budgets. | Count the same serialized Unicode characters, escapes and metadata as today, not bytes/4. Differential-test every budget boundary. |
| 7 | Reuse bounded SQLite read connections and prepared statements per session/worker. | Fixed per-request overhead and repeated ranked/graph requests. | New read transaction per request; release snapshots promptly, handle database replacement/schema changes. Do not pin a transaction for the whole session: long readers can delay WAL checkpoints. |
| 8 | Compute normalized relative sort keys once per walk; use IDs/borrowed paths through candidate processing. | Repositories containing many small files. | Preserve stable path order, path-filter semantics, unknown/new paths and platform behavior. Keep snapshots owning borrowed data alive. |

The regex crate documents compilation reuse and potential contention when sharing
one regex between threads. A bounded plan cache is a direct fit; it is not a reason
to replace an already optimized regex engine blindly.
[Regex performance documentation](https://docs.rs/regex/latest/regex/#performance)

For connection reuse, SQLite's snapshot and checkpoint lifetime rules matter more
than simply retaining a handle. [SQLite WAL](https://sqlite.org/wal.html)

## Candidate planning and exact matching

| ID | Proposal | Expected beneficiary | Preservation gate and cost |
|---|---|---|---|
| 9 | Estimate posting cardinalities cheaply; evaluate rare conjunctions first and decode other lists lazily. | Absent patterns, long identifiers, nested AND/OR plans. | Never skip an OR branch. An empty required conjunct can safely terminate. Reordering must not reorder final evidence. |
| 10 | Select a cheaper subset of necessary trigrams, then verify candidates with the complete matcher. | Patterns where decoding all postings costs more than reading a few extra files. | Omitting a necessary prefilter only widens candidates; preserve final verification. Do not confuse this with limiting candidate count. Use measured costs, not fixed “long query” rules. |
| 11 | Choose scan versus index from estimated candidate density and content size. | Frequent literals and broad regexes. | Both routes consume the same current corpus and matcher. Avoid evaluating the entire expensive plan merely to decide not to use it. |
| 12 | Use bounded HIR prefix/suffix extraction to recover constraints across small classes, repetitions and scoped flags. | Regexes unnecessarily sent to MatchAll today. | Derive constraints from parsed syntax with flags applied. Infinite/empty/overlarge literal sets fall back safely. Never keep only the first N alternatives. |
| 13 | Introduce provably safe Unicode case-insensitive candidate generation. | The current full-scan ignore-case path. | Cover the regex engine's case-equivalence classes, including Kelvin sign, long s and mixed scripts; lowercasing raw UTF-8 is not a proof. Preserve original text for exact matching/citations. Fall back on expansion limits. |
| 14 | Test a specialized literal finder and SIMD newline scanning. | Simple literals, line tables, large scans. | Rust regex already accelerates literals. Measure incremental benefit. Empty needles have different byte/UTF-8-boundary semantics and need the existing path or a proven equivalent. |
| 15 | Match independent candidate files in bounded parallel batches, with per-worker matcher scratch state. | Broad CPU-bound searches across many files. | Collect by original path index and merge deterministically before applying global limits. Cap workers, bytes in flight and open files. Small workloads may remain faster serially. |

HIR extraction has explicit size limits and conservative fallback behavior; even
“exact” extracted literals may still need regex verification for assertions.
[regex-syntax Extractor](https://docs.rs/regex-syntax/latest/regex_syntax/hir/literal/struct.Extractor.html)

`memmem` provides reusable literal finders, but documents the empty-needle boundary
difference. [memchr documentation](https://docs.rs/memchr/latest/memchr/memmem/index.html)
Indexed parallel iteration provides building blocks for ordered collection; memory
bounds and deterministic error handling remain ctx responsibilities.
[Rayon IndexedParallelIterator](https://docs.rs/rayon/latest/rayon/iter/trait.IndexedParallelIterator.html)

## Session state, freshness and update architecture

| ID | Proposal | Potential benefit | Preservation gate and cost |
|---|---|---|---|
| 16 | Cache immutable decoded content plus line offsets by verified file version or content identity. | Repeated broad searches; avoids rereading and rebuilding line tables. | Byte cap, eviction and update invalidation. A cache miss must execute the canonical validated read. A TTL alone is insufficient. Replacing mmap readers does not itself validate cached live files. |
| 17 | Keep a per-generation file-ID/version table in memory, avoiding per-excluded-path SQL. | Selective searches where most files are excluded. | Load from the same SQLite generation snapshot; current-file checks remain necessary. Retired tables live only as long as in-flight readers. Memory grows with file count. |
| 18 | Maintain an event-driven file inventory and dirty overlay, searching index candidates plus all dirty/new files. | Large trees: potentially removes whole-tree work from most queries. | **Conditional.** Requires an authoritative freshness boundary or explicit snapshot contract; queue emptiness/quiet time is insufficient. Overflow, unknown state, rename/ignore changes and unsupported filesystems require reconciliation. Never silently downgrade live semantics. |
| 19 | Make watcher updates genuinely proportional to touched paths/subtrees; avoid repeated whole-tree registration scans and repeated pending-state writes. | Edit-to-search latency and background CPU. | Watch new directories before relying on their events, retain deletion/rename semantics and full reconciliation on lost events. Publish SQLite and text generations atomically. |
| 20 | Cache exact results or decoded candidate lists by query, generation, policies and verified live state. | Repeated identical requests, including negative results. | **Conditional for live results.** A new matching file outside the previous hits invalidates a negative cache. Generation-only keys are insufficient before watcher publication. Plan caching is much easier to make safe. |
| 21 | Deduplicate concurrent identical requests, share immutable snapshots and batch explicitly requested searches. | Several agents/tools exploring the same state. | Per-request cancellation, budgets and freshness remain correct. Do not turn canceled/shared work into truncated success. A new batch API is a separate interface change. |

Git's fsmonitor design is useful reference material for an inventory of changed
paths and explicit resynchronization. Its platform support and Git-specific scope
must be checked; it is not a replacement for ctx's admitted untracked files.
[Git fsmonitor daemon documentation](https://git-scm.com/docs/git-fsmonitor--daemon)

Upstream tgrep keeps a persistent index, caches decoded candidate contents and
parallelizes matching. ctx already reuses readers, but not that entire content
pipeline. Reuse the applicable mechanisms with ctx's stronger live-check contract,
not the server wholesale.
[Pinned upstream search implementation](https://github.com/microsoft/tgrep/blob/e2007b52d2b8fe4176159d0da20c9ba4a46d5aab/tgrep-cli/src/serve.rs#L1551)

A proposed fast lane should distinguish immutable published-snapshot queries from
live queries. A cooperative editor version stream can help for buffers it owns;
it cannot certify shell edits or external generators. An OS journal may help on
some platforms, but requires a proven barrier, overflow handling and admission
tracking. Until that exists, keep the live fallback. This qualifies the earlier
conversation's suggestion to simply remove traversal when the watcher is active.

## Larger index and storage experiments

| ID | Experiment | Where a major gain is plausible | Cost and correctness gate |
|---|---|---|---|
| 22 | Positional trigrams, following Zoekt's design. | Many false candidate files; long literals whose trigrams occur in unrelated positions. | Store positions and test relative offsets before content matching. Larger index and migration; do not assume fixed offsets across regex wildcards. |
| 23 | Sparse variable-length grams, inspired by GitHub Blackbird. | Large code corpora with common trigrams and saturated follow masks. | Same deterministic gram extraction on indexing and query paths, plus exact verification. New format, builder, update logic and a full recall oracle are required. |
| 24 | Hybrid postings: sorted arrays for sparse IDs, compressed bitmaps for dense sets, plus optional SIMD intersections. | Large posting sets and Boolean combinations. | Measure container conversion/decoding overhead. A small list may beat a bitmap. Preserve masks or weaken pruning safely; changing encoding requires a format migration. |
| 25 | Immutable content shards with block metadata/Bloom filters and a small mutable overlay. | Broad scans with many small filesystem reads; improved locality and snapshot reuse. | Live files not represented in the snapshot must remain visible. Bloom filters may only reject definite negatives; saturation hurts speed, not recall. Cross-block matches need conservative handling. |
| 26 | Multiple immutable delta segments with controlled compaction instead of merging the full text index on every update. | Large repositories with frequent small edits. | Tombstones, per-file latest version, atomic generation manifest and bounded segment count. Faster writes can make reads more expensive; measure both. |
| 27 | Suffix-array or FM-index prototype beside the current engine. | Stable, very large corpora and diverse substring queries. | High construction/update complexity; exact query support and memory footprint must be evaluated. Treat as an alternative backend experiment, not an immediate replacement. |
| 28 | Deduplicate identical immutable content while retaining every original path. | Vendored/generated copies, branches or related worktrees. | Identical bytes can share storage/matching, but every path must produce its own ordered evidence. Publication and admission differ by repository. |

Zoekt describes positional trigrams, rare-pair selection and mmap-able shards.
Its storage costs are a tradeoff, not a performance prediction for ctx. Borrowing
these ideas does not imply importing its ranking or output semantics.
[Zoekt design](https://github.com/sourcegraph/zoekt/blob/main/doc/design.md)

GitHub describes false positives and saturation of follow masks as motivations
for sparse grams. This is particularly relevant to tgrep's per-file trigram/mask
filter. The benefit in ctx must be established independently, especially on small
repositories where traversal dominates.
[Blackbird engineering article](https://github.blog/engineering/the-technology-behind-githubs-new-code-search/)

Roaring offers compressed integer sets and SIMD implementations; it is a candidate
representation, not a universal win for every posting density.
[CRoaring](https://github.com/RoaringBitmap/CRoaring)

livegrep demonstrates regex search using suffix arrays, while describing difficult
query shapes as well. That is evidence of feasibility, not of superiority to the
current engine on changing local worktrees.
[livegrep author's design article](https://blog.nelhage.com/2015/02/regular-expression-search-with-suffix-arrays/)

## Lower-priority and potentially incompatible approaches

- **Hyperscan or another regex engine:** potentially useful for batching many
  patterns, but not a drop-in match enumerator. Start/end, greedy/overlap and
  Unicode behavior differ. Use only a proven conservative prefilter or preserve
  the current matcher on unsupported cases.
  [Hyperscan semantics](https://intel.github.io/hyperscan/dev-reference/compilation.html)
- **mmap of mutable source files:** does not make concurrent changes safe and can
  violate Rust mapping safety assumptions. mmap remains appropriate for our
  immutable leased generations. Prefer validated buffered reads for live sources.
  [memmap2 safety](https://docs.rs/memmap2/latest/memmap2/struct.MmapOptions.html)
- **Async I/O, read-ahead, prefetch, alternate allocators, NUMA tuning:** investigate
  only after profiles show those bottlenecks. They add platforms, memory or resource
  costs and are unlikely to remove the current shared metadata floor by themselves.
- **PGO and CPU-specific dispatch:** later, train on diverse actual workloads and
  retain portable CPU fallbacks. Release already uses thin LTO; enabling LTO again
  is not a new optimization.
  [Rust PGO documentation](https://doc.rust-lang.org/rustc/profile-guided-optimization.html)
- **GPU/distributed search:** exploratory for very large batches or corpora; extra
  transport, deployment and memory costs make it a poor first direction for local
  requests measured in tens of milliseconds. No ctx gain is established.
- **Embedding/ANN-only retrieval, AST-only pruning, dropping comments/tests, silently
  narrowing paths, removing Unicode or lowering output limits:** these change the
  admitted answer set or evidence. They do not meet this task's lossless requirement.
  Structural/semantic indexes may supplement exact search, not replace its recall.
- **Compressed/binary MCP payloads:** require a cooperating client or new transport.
  Do not silently replace standard structured output. A paginated/resource-link API
  can reduce first-response cost but is not equivalent to returning complete evidence
  in one call; measure total follow-up work and preserve the original interface.

## Recommended sequence and acceptance gates

The ordering below is an engineering judgment based on current code, not a ranked
list of measured savings. Work on independent interventions first so their effects
can be attributed.

1. **Measure the stages.** Record wall/CPU time, bytes read, files visited/opened,
   metadata and SQL calls, postings decoded, candidates, matches, allocated bytes,
   serialization time and response size. Separate first request, warm process,
   warm filesystem, concurrent edits and simultaneous requests. Profiling overhead
   must not contaminate the final latency table.
2. **Remove demonstrably wasted work (1–8).** Digest omission, buffer ownership,
   query caching, exact lazy budgeting, line-table reuse and temporary JSON removal.
   Then address connection and path-processing overhead where measured.
3. **Improve selection and CPU use (9–15, 17).** Cardinality-aware lazy postings,
   adaptive dispatch, bounded HIR extraction, safe folding, parallel ordered batches.
4. **Improve updates and session reuse (16, 19, 21).** Make memory limits explicit.
   Keep the live walk until the consistency requirements of 18/20 are proven.
5. **Prototype a new index (22–28) only if profiles justify it.** Compare sparse
   grams and positional trigrams on false-candidate rate, read bytes, total RSS,
   cold index time, edit publication time and disk size, not lookup time alone.

Validation must cover fixed real revisions in Python, TS/JS, Rust, Go and PHP;
small repositories, many tiny files, large files and large mixed worktrees;
rare/common/absent literals, short strings, Unicode case folding, scoped flags,
multiline/empty matches, alternatives, escaped punctuation and malformed queries.
Use real default budgets as well as unlimited output. Add changing query sets so
repeated-query caching does not dominate the benchmark artificially.

Keep the existing direct-scan oracle and independent matcher/span fixtures. Test
atomic-save renames, create/delete bursts, ignore-rule updates, concurrent requests,
publication rollback, corrupt/missing generations, queue overflow and restart.
Every extra candidate is acceptable internally; a missing hit, changed citation or
unexplained partial result is not. Returning identical snippets with different
ordering is also a failure if it changes the budgeted response.

For each accepted intervention, retain an interleaved same-profile A/B experiment,
raw samples, binary hashes and output fingerprints on identical corpora. Repeat
batches and report uncertainty; do not label the maximum of ten measurements a
stable p99. Measure process RSS, index bytes, indexing time and idle/edit CPU as
well as latency. Gate correctness absolutely; evaluate performance by workload
instead of demanding an impossible universal speedup. No build is authorized by
this document; follow the user's current launch instructions.

A 10x total gain requires removing or accelerating roughly 90% of current elapsed
work. If a stage is 10% of latency, eliminating it entirely gives only about 1.11x.
Likewise, returning hundreds of kilobytes has a nonzero output cost regardless of
how good the index becomes. Major selective-query gains are plausible if whole-tree
work and unnecessary reads dominate; broad full-output queries face a different
limit. No numeric future gain is established by this research.
