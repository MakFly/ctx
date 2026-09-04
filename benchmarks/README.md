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
