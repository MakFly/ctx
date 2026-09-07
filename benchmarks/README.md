# ctx benchmarks

For the pinned ten-repository comparison against codebase-memory-mcp,
codesearch, Serena, and jCodeMunch, see the
[`competitive/` benchmark](competitive/README.md). Its corpus, security review,
ground truth, runner, and intermediate raw results are kept separate from the
synthetic microbenchmark below.

The benchmark is implemented in Rust and calls the same library used by the
`ctx` CLI and persistent MCP server. It creates a temporary mixed-language
repository, then records:

- cold indexing throughput;
- unchanged reindexing;
- exact symbol and natural-text search;
- definition and caller graph queries;
- ranked context packing.

Run from the repository root:

```console
cargo run --release --example benchmark -- \
  --files 1000 \
  --iterations 100 \
  --warmups 10 \
  --output benchmarks/results/latest.json
```

The generated corpus contains equal proportions of Python, TypeScript, Go,
Rust, and PHP. Retrieval percentiles are measured in one warm process, matching
the long-lived MCP server use case.

Committed results are reference snapshots, not universal performance claims.
CPU, filesystem, Rust and SQLite versions, repository structure, average file
size, and symbol density all affect timings. Compare revisions on the same host
with identical arguments.

## Latest measured result

Measured on 2026-09-04 after rebuilding `ctx` 0.3.0. Compilation time is not
included. The benchmark was executed three times; the committed snapshot is
the middle cold-index run. Each retrieval row contains 10 warmups and 100
measured iterations. The generated corpus contained 1,000 code files split
evenly across the five languages, plus a README and a call site.

| Environment | Value |
|---|---|
| CPU | AMD Ryzen 7 3700X, 8 cores / 16 logical CPUs |
| Memory | 30.3 GiB |
| Platform | Linux x86_64 |
| Rust | 1.98.0 |
| SQLite | 3.50.2 |
| Profile | Cargo release, thin LTO |
| Indexed corpus | 1,002 files, 573.7 KiB |
| Index contents | 2,001 symbols, 2,601 edges, 3.17 MiB SQLite DB |

Indexing measurements are end-to-end wall-clock durations:

| Operation | Duration | Throughput / changes |
|---|---:|---:|
| Cold index | 190.275 ms | 5,266.062 files/s |
| Unchanged reindex | 36.254 ms | 0 changed files |

Retrieval results use 10 warmups followed by 100 measured iterations in the
same process:

| Operation | Min | p50 | p95 | p99 | Max | Mean |
|---|---:|---:|---:|---:|---:|---:|
| Symbol search | 1.509 ms | 1.638 ms | 1.722 ms | 1.764 ms | 1.828 ms | 1.627 ms |
| Text search | 1.163 ms | 1.188 ms | 1.249 ms | 1.293 ms | 1.390 ms | 1.195 ms |
| Definition graph | 0.525 ms | 0.533 ms | 0.559 ms | 0.589 ms | 0.653 ms | 0.537 ms |
| Callers graph | 0.573 ms | 0.599 ms | 0.711 ms | 0.744 ms | 0.744 ms | 0.615 ms |
| Context pack | 6.256 ms | 6.380 ms | 6.609 ms | 6.656 ms | 6.751 ms | 6.407 ms |

Against the previous same-host snapshot, cold indexing improved 3.4%, unchanged
reindexing improved 5.6%, symbol/text/definition p50 improved 1.5-3.1%, and
callers p50 was effectively flat (+0.5%). Context-pack p50 regressed from
4.724 ms to 6.380 ms (+35.1%, or +1.656 ms). That regression is retained here
rather than hidden; end-to-end agent latency is still dominated by the harness.

The machine-readable source for these tables is
[`results/latest.json`](results/latest.json). This is a real execution against
a reproducible synthetic corpus; it is not a measurement of an arbitrary
production repository.

## Codex exec: ctx MCP versus shell baseline

Three fresh runs per variant used Codex CLI 0.153.1 with `gpt-5.6-luna`,
reasoning effort `high`, an ephemeral read-only session, the same four
code-navigation questions, and the same clean ten-file `mini_repo` fixture.
User configuration and rules were disabled. The MCP variant could call only
`ctx_pack`; the baseline could use shell search and targeted reads but no MCP.
Every run answered all four checked facts correctly.

| Median of 3 fresh runs | Shell baseline | Compact MCP | MCP vs shell |
|---|---:|---:|---:|
| Wall time | 22.91 s | 17.90 s | -21.9% |
| Input tokens, including cached | 61,192 | 45,715 | -25.3% |
| Cached input tokens | 46,080 | 37,120 | -19.4% |
| Uncached input tokens | 8,628 | 8,595 | -0.4% |
| Output tokens | 568 | 466 | -18.0% |
| Reasoning output tokens | 167 | 160 | -4.2% |
| Tool calls | 3 | 1 | -66.7% |
| MCP tool payload tokens | n/a | 459 | n/a |
| Accuracy | 4/4 | 4/4 | equal |

Compared with the original pre-optimization MCP snapshot, the current compact
MCP median is 43.6% faster (31.72 s -> 17.90 s), uses 61.7% fewer input tokens
(119,470 -> 45,715), 51.9% fewer output tokens, and one tool call instead of
five. The paired fresh shell comparison above is the fairer measure of present
behavior; the historical row shows the gain from the implementation work.

The improvement comes from making the pack answer-ready: one request returns
exact symbol definitions, callers, callees, and explicit citation spans. The
Codex installer runs `ctx mcp --compact`, which physically exposes only a
query-only `ctx_pack` tool and avoids returning the same envelope in both text
and `structuredContent`. This removes repeated model/tool round trips and
reduces MCP schema/result overhead. On this small fixture the compact path is
faster and uses fewer total tokens than shell search. Uncached input was nearly
equal in this particular run, so the total-input gain partly reflects
provider-side prompt caching. Larger and real-repository evaluation is still
required before generalizing the result.

Raw per-run measurements and the exact methodology are in
[`results/codex-exec-luna-high.json`](results/codex-exec-luna-high.json).

## Exact non-interactive response cache

A real `ctx run` benchmark used the same question, model, effort, and clean
fixture. Three forced refreshes launched Codex; three following exact requests
were validated and returned from `.ctx/cache.sqlite` without starting a
harness.

| Median of 3 runs | Wall time | ctx duration | Input tokens | Output tokens | Accuracy |
|---|---:|---:|---:|---:|---:|
| Clean cache miss | 15.58 s | 15,574 ms | 46,551 | 506 | 4/4 |
| Exact cache hit | 0.01 s | 15 ms | 0 | 0 | 4/4 |

The exact hit reduced observed wall time by more than 99.9% and started no
harness process. The machine-readable per-run record is
[`results/codex-run-cache.json`](results/codex-run-cache.json).

## Full-content/trigram comparison

The benchmark now adds paired `literal_scan` / `literal_trigram` and
`regex_scan` / `regex_trigram` measurements on the identical generated corpus.
Both variants use the same Rust matcher, admission policy, current-file checks,
context extraction and unlimited result/token budgets. The indexed variant
includes opening/validating its mmap generation and metadata traversal; these
are end-to-end library measurements, not isolated posting-list timings.

Index output includes total trigram-generation bytes alongside SQLite bytes.
Cold and unchanged indexing include `peak_process_rss_bytes`, read from Linux's
process-lifetime `VmHWM`. The unchanged phase inherits the cold high-water mark;
these values must not be described as isolated phase allocations. Other
platforms report null. Use the existing isolated competitive index runner for
per-process peaks. Historical `latest.json` results predate these additions. The separate CLI
scenario measurements below exercise the existing release binary.


## Paired CLI search scenarios (2026-09-07)

`search_speed.py` uses an existing executable and never compiles or launches a
server. It creates disposable text repositories and compares a valid tgrep
generation with ctx's full-scan fallback, obtained by temporarily clearing the
SQLite generation reference. Both variants retain the same SQLite database,
files, matcher and output limits. The reference change is outside the timer.
This measures the deployed CLI fallback, not a direct `scan_search` library call.

```console
python3 benchmarks/search_speed.py --output benchmarks/results/search-speed.json
python3 benchmarks/search_speed.py --files 100 --bytes-per-file 1048576 --output benchmarks/results/search-speed-large-files.json
```

Seven scenarios cover a rare literal, common literal, absent literal, selective
regex, regex without usable trigrams, two-character literal and Unicode
case-insensitive literal. Default corpora contain 100 and 1,000 files of 16 KiB;
the second command exercises 100 files of 1 MiB. Data is deterministic repetitive
text, not representative of every source repository. Common queries return all
files and substantial snippets, intentionally exercising output overhead.

Each scenario alternates scan/indexed execution order, performs two warmup pairs
and ten measured pairs, and records all samples, median, nearest-rank p95 and
median speedup (above 1 favors tgrep). Full hits, snippets, token estimates,
coverage and hints must match on every pair; expected hit counts and absence of
truncation are checked. No flaky speed threshold is used as a correctness test.

Timing includes process startup, SQLite access, traversal, matching and JSON
serialization/capture. Python JSON parsing is excluded. OS caches are warm;
indexing starts with no ctx index but does not imply a cold filesystem cache.
Cold-index and unchanged-index durations and total index size are recorded.
Ten samples are exploratory measurements, not stable tail-latency estimates.
Results include the binary SHA-256. This does not measure MCP warm-process speed,
watcher cost, RSS, LLM token savings or billing, and it is not an old/new version
comparison. The synthetic corpora are removed automatically after each run.

Measured median speedups on this host (scan fallback / trigram):

| Scenario | 100 × 16 KiB | 1,000 × 16 KiB | 100 × 1 MiB |
|---|---:|---:|---:|
| rare_literal | 1.08x | 1.75x | 3.53x |
| common_literal | 0.85x | 0.87x | 0.95x |
| absent_literal | 1.05x | 1.75x | 3.77x |
| selective_regex | 1.05x | 1.76x | 3.35x |
| unprunable_regex | 0.84x | 0.83x | 0.99x |
| short_literal | 0.87x | 0.88x | 0.97x |
| unicode_ignore_case | 0.75x | 0.71x | 0.77x |

Selective searches improved; broad/unprunable searches often regressed. The
index still walks metadata and checks file versions; candidate pruning saves
content reads only when the query can exclude files. These timings do not
isolate the contribution of each internal operation. Raw samples are in
[small/medium corpora](results/search-speed.json) and
[large files](results/search-speed-large-files.json).

## Real Python code: FastAPI (2026-09-07)

The code-focused mode reads committed `.py` blobs from a local FastAPI Git
checkout into disposable repositories. It excludes documentation files, assets,
Git metadata and uncommitted changes; docstrings inside Python remain code
content. It separately measures the library core and all tracked Python files
(including tests and examples). This is one Python project, not a cross-language
claim. Both variants search the identical extracted corpus.

```console
python3 benchmarks/search_speed.py --fastapi-repo /path/to/fastapi --output benchmarks/results/search-speed-fastapi.json
```

Revision: `50113da16fec53b66b80d75e80a89296de4fa5a5`. Two warmup pairs and ten measured pairs per scenario; all 16 scenarios passed complete evidence parity.

| Scenario | Core: 48 files | All Python: 1,138 files | All Python scan → tgrep median |
|---|---:|---:|---:|
| function_identifier | 1.05x | 1.16x | 36.13 → 31.03 ms |
| exact_signature | 0.93x | 1.23x | 34.52 → 27.96 ms |
| common_identifier | 0.89x | 1.01x | 39.53 → 39.13 ms |
| absent_literal | 1.07x | 1.21x | 36.21 → 29.91 ms |
| function_regex | 0.86x | 0.74x | 35.50 → 47.74 ms |
| decorator_regex | 0.84x | 0.74x | 41.10 → 55.44 ms |
| short_literal | 0.83x | 0.74x | 41.07 → 55.85 ms |
| identifier_ignore_case | 0.84x | 0.69x | 36.19 → 52.19 ms |

Ratios above 1 favor tgrep. These results show modest selective-query gains
and regressions for several broad/regex queries. They supersede any inference
that the larger synthetic gains necessarily apply to ordinary code files.
The same CLI, warm-cache and scan-fallback limitations above apply. Full paths,
query strings, hit counts and samples are in
[the raw result](results/search-speed-fastapi.json).

## Corrected release: FastAPI (2026-09-07)

After the explicitly authorized `cargo build --release --locked`, the same
16 scenarios ran with two warmup pairs and ten measured pairs. All evidence
matched the scan fallback. The prior release snapshot is retained unchanged.
Before/after columns compare separate runs on the same host and revision,
not an interleaved old/new binary trial. Small differences may be noise.
The scan-versus-index ratios are paired within the new run. CLI processes do
not exercise persistent MCP reader reuse.

| Query (1,138 Python files) | Previous tgrep median | Corrected tgrep median | Corrected scan median | Scan / tgrep |
|---|---:|---:|---:|---:|
| function_identifier | 31.03 ms | 23.83 ms | 35.21 ms | 1.48x |
| exact_signature | 27.96 ms | 24.46 ms | 36.13 ms | 1.48x |
| common_identifier | 39.13 ms | 32.39 ms | 41.69 ms | 1.29x |
| absent_literal | 29.91 ms | 24.78 ms | 39.19 ms | 1.58x |
| function_regex | 47.74 ms | 26.61 ms | 37.23 ms | 1.40x |
| decorator_regex | 55.44 ms | 43.97 ms | 45.75 ms | 1.04x |
| short_literal | 55.85 ms | 44.31 ms | 44.38 ms | 1.00x |
| identifier_ignore_case | 52.19 ms | 42.59 ms | 40.34 ms | 0.95x |

Selective queries now benefit in this corpus. Broad queries are close to the
scan, with a small remaining slowdown in the ignore-case scenario. This is
not evidence of a universal speedup. Raw samples and binary hash:
[corrected release](results/search-speed-fastapi-fixed-release.json).

## Persistent MCP versus fresh CLI (2026-09-07)

The existing corrected release binary was exercised through full MCP stdio
`ctx_search` on the same 1,138 committed FastAPI Python files. One MCP server
and its watcher stayed alive across all requests. Startup reconciliation finished
before measurement; each scenario used two warmup pairs and ten measured pairs,
alternating CLI/MCP order. Both paths used identical unlimited evidence budgets
and returned identical hits, token estimates, coverage and hints.

```console
python3 benchmarks/mcp_speed.py --repo /path/to/fastapi --output benchmarks/results/mcp-speed-fastapi.json
```

| Query | Fresh indexed CLI p50 | Persistent MCP p50 | CLI / MCP |
|---|---:|---:|---:|
| function_identifier | 25.91 ms | 22.27 ms | 1.16x |
| exact_signature | 27.35 ms | 20.86 ms | 1.31x |
| common_identifier | 32.52 ms | 29.37 ms | 1.11x |
| absent_literal | 23.22 ms | 19.63 ms | 1.18x |
| function_regex | 23.84 ms | 17.99 ms | 1.33x |
| decorator_regex | 47.65 ms | 49.68 ms | 0.96x |
| short_literal | 49.60 ms | 60.87 ms | 0.81x |
| identifier_ignore_case | 41.50 ms | 45.69 ms | 0.91x |

Times include transport, response receipt and Python JSON decoding. CLI time
also includes starting a new process; this comparison cannot isolate the reader
cache contribution. The MCP client uses buffered pipe reads. First per-scenario
calls are recorded but do not all represent cold readers. Broad-output cases
can remain slower over MCP; the results do not support a universal speedup.
This measures full MCP search, not compact `ctx_pack`, an LLM workflow or billed
tokens. No build was run. Raw samples and executable hash:
[MCP results](results/mcp-speed-fastapi.json).

After timing, the same session observed creation, replacement and deletion of
a Python probe via the watcher, with correct search visibility after each
publication. Publication was observed after 385, 359, 360 ms respectively
(25 ms polling granularity; three observations, not latency percentiles).
Closing stdin shut the MCP server down successfully without forced termination;
the process was reaped and the disposable corpus removed.

## MCP response size correction (2026-09-07)

Full `ctx_search` previously serialized the complete envelope twice. It now
retains the advertised output schema and sends only `structuredContent` to
protocol 2025-06-18+ clients. Older/unknown clients keep the text fallback.
The raw evidence and query budgets are unchanged. Other tools are unchanged.
This uses structured output introduced by the
[MCP specification](https://modelcontextprotocol.io/specification/2025-06-18/server/tools#structured-content);
the compatibility text copy is retained for older protocol clients.

The same test-profile executable passed all eight FastAPI CLI/MCP scenarios
under both protocol versions, plus watcher create/replace/delete checks and
graceful shutdown. Both paths include the complete evidence. Median wire bytes:

| Query | Legacy duplicate response | Modern response | Reduction |
|---|---:|---:|---:|
| function_identifier | 5568 | 2674 | 52.0% |
| exact_signature | 764 | 391 | 48.8% |
| common_identifier | 345064 | 163258 | 52.7% |
| absent_literal | 274 | 162 | 40.9% |
| function_regex | 1196 | 591 | 50.6% |
| decorator_regex | 686574 | 322564 | 53.0% |
| short_literal | 984870 | 470023 | 52.3% |
| identifier_ignore_case | 114309 | 54448 | 52.4% |

The 22 targeted library/MCP tests passed, including schema retention, actual
modern stdio responses and legacy/modern envelope equality. These new runs use
the debug test binary. Their latency is not comparable to the prior release
tables; the release has not been rebuilt for this change. Size reduction is
measured directly and is not a claim about billed tokens or equal latency gains.
The benchmark accepts `--protocol 2025-03-26` or `--protocol 2025-06-18`
(new default) and records wire sizes. Raw diagnostics:
[legacy](results/mcp-response-legacy-test.json),
[modern](results/mcp-response-modern-test.json).

## Release validation of MCP response correction (2026-09-07)

The explicitly authorized `cargo build --release --locked` succeeded and
`cargo test --workspace --all-targets --locked` passed all 295 tests. The same
release executable then passed eight FastAPI scenarios under each protocol
version, including evidence parity, watcher create/replace/delete visibility
and graceful shutdown. Two warmup pairs and ten measured pairs per scenario.

| Query | Modern CLI p50 | Modern MCP p50 | Legacy MCP p50 | Modern response bytes |
|---|---:|---:|---:|---:|
| function_identifier | 25.22 ms | 19.98 ms | 18.79 ms | 2674 |
| exact_signature | 24.14 ms | 19.43 ms | 20.28 ms | 391 |
| common_identifier | 32.62 ms | 27.24 ms | 32.41 ms | 163258 |
| absent_literal | 22.82 ms | 17.60 ms | 17.54 ms | 162 |
| function_regex | 22.99 ms | 18.45 ms | 19.14 ms | 591 |
| decorator_regex | 47.51 ms | 45.60 ms | 47.39 ms | 322563 |
| short_literal | 49.53 ms | 51.26 ms | 56.00 ms | 470022 |
| identifier_ignore_case | 40.77 ms | 43.06 ms | 44.28 ms | 54447 |

Modern/legacy protocols ran separately, not interleaved; small latency changes
may be noise. CLI/MCP comparisons are paired within each run. Wire bytes remain
about 52–53% lower for large modern responses. Modern MCP is faster for selective
queries in this run; short-literal and ignore-case requests remain slightly
slower than their paired CLI. No claim of universal acceleration or equivalent
billing savings is made. Raw samples and binary hashes:
[modern release](results/mcp-response-modern-release.json),
[legacy release](results/mcp-response-legacy-release.json).

## First lossless batch: test-profile measurement (2026-09-07)

The existing current debug/test executable was benchmarked without rebuilding.
Same FastAPI revision and 1,138 Python files, modern MCP protocol, two warmup
pairs and ten measured pairs per scenario. Comparison below is against the
previous modern **test-profile** run, not the release measurements. Historical
before/after runs were not interleaved; host noise can affect the differences.

| Query | Previous MCP p50 | Current MCP p50 | Latency reduction |
|---|---:|---:|---:|
| function_identifier | 51.96 ms | 38.30 ms | +26.3% |
| exact_signature | 42.80 ms | 38.97 ms | +9.0% |
| common_identifier | 99.71 ms | 74.93 ms | +24.9% |
| absent_literal | 52.63 ms | 53.67 ms | -2.0% |
| function_regex | 53.46 ms | 47.43 ms | +11.3% |
| decorator_regex | 185.10 ms | 116.30 ms | +37.2% |
| short_literal | 206.90 ms | 137.05 ms | +33.8% |
| identifier_ignore_case | 203.22 ms | 132.16 ms | +35.0% |

All eight scenarios passed complete CLI/MCP evidence parity. Watcher create,
replace and delete visibility passed (665, 670 and 642 ms observed publication
times); the server shut down gracefully and the disposable corpus was removed.
Unlimited evidence budgets mean this run does not exercise early snippet-budget
closure. It does not isolate digest omission from matcher caching and does not
establish release performance gains. Raw samples and executable hash:
[current test-profile results](results/mcp-lossless-modern-test.json).

## External competitor: ctx versus Zoekt

`competitor_speed.py` compares existing CLI executables on the same disposable,
synthetic Python corpus. Zoekt was chosen as an independent indexed code-search
engine; tgrep already supplies ctx's vendored index components. Alternatives
researched were [livegrep](https://github.com/livegrep/livegrep) and
[upstream tgrep](https://github.com/microsoft/tgrep).

The adapter follows Sourcegraph's current
[Zoekt CLI](https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt/main.go),
[indexer](https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-index/main.go)
and [query syntax](https://github.com/sourcegraph/zoekt/blob/main/doc/query_syntax.md),
checked on 2026-09-07. It requires `-jsonl` support; older binaries may not work.

```console
python3 benchmarks/competitor_speed.py --self-test
python3 benchmarks/competitor_speed.py --ctx /path/to/ctx --ctx-profile release --zoekt /path/to/zoekt --zoekt-index /path/to/zoekt-index --output benchmarks/results/ctx-vs-zoekt.json
```

No build, installation, download or server is launched. All indexing occurs in
an automatically removed temporary directory. Six scenarios cover rare/common/
absent literals, a selective regex, a short literal and Unicode case folding.
Default: 100 files, two warmups and ten alternating measured CLI pairs. JSON
records p50/p95, raw samples, executable hashes, source digest, index bytes and
initial indexing times. Set the actual ctx profile explicitly; comparing a debug
ctx against optimized Zoekt does not establish a product performance ranking.

A small independent Python oracle supplies expected matching lines. The Zoekt
adapter checks returned line text and reconstructs ctx's merged two-line context;
ctx's returned spans/snippets must match exactly. Ranking is normalized away.
Partial/missing evidence fails the run, including omissions from Zoekt's default
result caps. The query set deliberately uses shared single-line regex syntax;
it does not establish equivalence for arbitrary Rust/Go regexes or query languages.

Search timing includes process startup and native output capture, excluding
Python normalization. Native output workloads differ: ctx emits context and
metadata, Zoekt emits matching lines. Zoekt ctags is disabled, whereas ctx still
performs its normal structural indexing: initial-index costs describe those
configurations, not isolated text-index algorithms. This is a synthetic CLI
comparison, not a persistent MCP, live-update, real-code or semantic-search test.

Validation here: adapter self-test passed; all six scenarios matched the oracle
using the existing ctx debug executable on ten files. Zoekt and zoekt-index are
not installed, so actual Zoekt integration and comparative timings remain
unexecuted. No benchmark result file or performance claim has been fabricated.
