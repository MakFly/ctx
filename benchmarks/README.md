# ctx benchmarks

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

Measured on 2026-09-04 by running the command above. Compilation time is not
included. The generated corpus contained 1,000 code files split evenly across
the five languages, plus a README and a call site.

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
| Cold index | 197.069 ms | 5,084.511 files/s |
| Unchanged reindex | 38.421 ms | 0 changed files |

Retrieval results use 10 warmups followed by 100 measured iterations in the
same process:

| Operation | Min | p50 | p95 | p99 | Max | Mean |
|---|---:|---:|---:|---:|---:|---:|
| Symbol search | 1.536 ms | 1.683 ms | 1.970 ms | 2.258 ms | 2.381 ms | 1.708 ms |
| Text search | 1.171 ms | 1.206 ms | 1.339 ms | 1.360 ms | 1.398 ms | 1.224 ms |
| Definition graph | 0.530 ms | 0.550 ms | 0.772 ms | 0.817 ms | 0.832 ms | 0.592 ms |
| Callers graph | 0.577 ms | 0.596 ms | 0.633 ms | 0.668 ms | 0.701 ms | 0.602 ms |
| Context pack | 4.589 ms | 4.724 ms | 5.158 ms | 5.260 ms | 5.481 ms | 4.778 ms |

The machine-readable source for these tables is
[`results/latest.json`](results/latest.json). This is a real execution against
a reproducible synthetic corpus; it is not a measurement of an arbitrary
production repository.

## Codex exec: ctx MCP versus shell baseline

Three runs per variant used Codex CLI 0.153.1 with `gpt-5.6-luna`, reasoning
effort `high`, an ephemeral read-only session, the same four code-navigation
questions, and the ten-file `mini_repo` fixture. The optimized MCP variant
exposed only `ctx_pack`; the baseline could use normal shell search and
targeted reads but no MCP. Every run answered all four checked facts correctly.

| Median of 3 runs | MCP before | One-shot structured | Compact MCP | Shell baseline | Compact vs shell |
|---|---:|---:|---:|---:|---:|
| Wall time | 31.72 s | 17.26 s | 16.63 s | 20.10 s | -17.3% |
| Input tokens, including cached | 119,470 | 54,844 | 43,999 | 44,080 | -0.2% |
| Cached input tokens | 97,024 | 42,240 | 37,120 | 32,000 | +16.0% |
| Uncached input tokens | 22,446 | 12,604 | 6,879 | 11,817 | -41.8% |
| Output tokens | 968 | 438 | 411 | 546 | -24.7% |
| Reasoning output tokens | 522 | 163 | 147 | 207 | -29.0% |
| Tool calls | 5 | 1 | 1 | 2 | -50.0% |
| Tool payload tokens | 1,733 | 459 | 459 | n/a | n/a |
| Accuracy | 4/4 | 4/4 | 4/4 | 4/4 | equal |

The improvement comes from making the pack answer-ready: one request returns
exact symbol definitions, callers, callees, and explicit citation spans. The
Codex installer runs `ctx mcp --compact`, which physically exposes only a
query-only `ctx_pack` tool and avoids returning the same envelope in both text
and `structuredContent`. This removes repeated model/tool round trips and
reduces MCP schema/result overhead. On this small fixture the compact path is
faster and uses fewer tokens than shell search. The `ctx` engine itself reports
7-16 ms per compact pack in these runs. Larger repository evaluation is still
required before generalizing the result.

Raw per-run measurements and the exact methodology are in
[`results/codex-exec-luna-high.json`](results/codex-exec-luna-high.json).
