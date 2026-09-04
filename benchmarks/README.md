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
