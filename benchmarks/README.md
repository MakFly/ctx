# ctx benchmarks

This directory contains a reproducible, dependency-free benchmark driver for
the core ctx operations. It generates a temporary mixed-language repository,
indexes it, and records cold indexing, unchanged reindexing, symbol and text
search, graph queries, and context packing.

Run from the repository root after installing the test environment:

```console
uv run python benchmarks/benchmark.py \
  --files 1000 \
  --iterations 100 \
  --warmups 10 \
  --output benchmarks/results/latest.json
```

The generated repository contains equal proportions of Python, TypeScript, Go,
Rust, and PHP. Retrieval percentiles are measured in one warm Python process.
If `rg` is installed, the report includes an exact-file-match ripgrep baseline.
That baseline only measures raw literal lookup: it does not provide ranked
symbols, graph relationships, token budgets, coverage, or context packs.

Committed results are snapshots, not universal performance claims. CPU,
filesystem, Python, SQLite, repository structure, average file size, and symbol
density all affect timings. Compare changes on the same host with the same
arguments.

The latest committed 1,000-code-file run indexed 1,002 files in 178.7 ms.
Warm p50 latency was 1.03 ms for symbol search, 1.49 ms for text search,
0.79 ms for callers, and 5.58 ms for a context pack. See
[`results/latest.json`](results/latest.json) for p95/p99 values and the full
environment.
