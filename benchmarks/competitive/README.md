# Competitive real-repository benchmark

This suite compares `ctx` with codebase-memory-mcp, codesearch, Serena, and
jCodeMunch on ten pinned public repositories. It measures product behavior, not
marketing claims or benchmarks copied from competitor documentation.

The corpus contains two repositories per primary `ctx` language:

| Language | Repositories |
|---|---|
| Python | Requests, FastAPI |
| JavaScript / TypeScript | Express, Vue |
| Go | Gin, Chi |
| Rust | Axum, ripgrep |
| PHP | Slim, Symfony HttpKernel |

Full URLs, commits, questions, and forty path/line facts are in
[`repos.json`](repos.json). Competitor revisions and licensing are in
[`competitors.json`](competitors.json). Read [`AUDIT.md`](AUDIT.md) before
executing any third-party code.

## Protocol

1. Clone exactly the commits in `repos.json`.
2. Verify every expected source path, line, and symbol before measuring.
3. Build every tool from its pinned source in an isolated, credential-free
   environment.
4. Run each tool with a separate disposable HOME and index directory, with the
   source repository read-only and runtime networking disabled.
5. Measure cold index wall time, unchanged index wall time, peak RSS, database
   size, and indexed file/symbol counts where exposed.
6. Ask one compound question containing four independently scored facts for
   each repository.
7. Use the same Codex CLI version, `gpt-5.6-luna`, high reasoning effort,
   read-only ephemeral sessions, and no user configuration for every tool.
8. For the shell baseline, allow targeted `rg` and file reads but no MCP. For a
   tool variant, allow only that tool's read-only MCP retrieval operations.
9. Record wall time, total/cached/uncached input tokens, output/reasoning tokens,
   MCP calls, payload size, and deterministic citation accuracy.
10. Aggregate paired per-repository deltas. Do not compare one tool's best run
    with another tool's worst run.

There are two capability tracks:

- **Common structural track:** no cloud API and no embeddings. Tree-sitter,
  text/BM25, graph, and pre-provisioned LSP retrieval are allowed.
- **Documented local-default track:** local embeddings may be enabled for a
  competitor when they are part of its normal documented product, but `ctx`
  remains unchanged and embedding-free. These results are labelled separately.

The ten different repositories are the benchmark samples. We do not repeat an
identical agent request merely to inflate the sample count; provider-side prompt
caching would make those repetitions correlated. Low-level retrieval operations
may use repeated warmups and iterations.

## Reproduce the completed first phase

Use a fresh workspace outside any source checkout:

```console
python3 benchmarks/competitive/benchmark.py prepare --workspace /tmp/ctx-competitive
python3 benchmarks/competitive/benchmark.py verify --workspace /tmp/ctx-competitive
python3 benchmarks/competitive/benchmark.py ctx-index \
  --workspace /tmp/ctx-competitive \
  --ctx-binary target/release/ctx \
  --output /tmp/ctx-index.json
```

The first committed real-repository indexing run is
[`results/ctx-index.json`](results/ctx-index.json). It is an intermediate result,
not yet a competitor ranking.

Initial `ctx` 0.3.0 measurements on this host:

| Repository | Indexed files | Cold index | Unchanged index | Peak cold RSS |
|---|---:|---:|---:|---:|
| Requests | 68 | 0.127 s | 0.042 s | 32.8 MiB |
| FastAPI | 2,890 | 1.835 s | 0.352 s | 60.1 MiB |
| Express | 175 | 0.250 s | 0.097 s | 28.5 MiB |
| Vue | 644 | 3.494 s | 1.087 s | 75.1 MiB |
| Gin | 120 | 0.276 s | 0.072 s | 40.3 MiB |
| Chi | 94 | 0.138 s | 0.043 s | 29.6 MiB |
| Axum | 427 | 0.498 s | 0.168 s | 43.2 MiB |
| ripgrep | 155 | 0.562 s | 0.179 s | 66.5 MiB |
| Slim | 138 | 0.202 s | 0.059 s | 36.9 MiB |
| Symfony HttpKernel | 340 | 0.625 s | 0.163 s | 54.2 MiB |

Across the corpus, `ctx` indexed 5,051 accepted files, 37,687 symbols, and
160,590 edges in 8.008 seconds of summed cold wall time. The source checkouts
contain 5,650 files / 52.0 MiB before ignore and language filtering. These rows
are not used to claim superiority until every competitor has run on the same
commits and machine.

## Status

- Corpus: complete and pinned.
- Forty ground-truth source facts: verified against all ten commits.
- `ctx` cold/unchanged indexing: measured on all ten repositories.
- Competitor source and execution-surface audit: initial review complete.
- Isolated competitor builds and indexes: pending.
- Cross-tool Codex runs and final README comparison: pending.

No cross-tool winner should be claimed until the pending rows are complete.
