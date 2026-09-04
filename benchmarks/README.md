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
