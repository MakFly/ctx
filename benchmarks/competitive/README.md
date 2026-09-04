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

### What these tests establish, and what they do not

- The forty checks score symbol/path/line presence. They do not grade the
  correctness of prose, call ordering, or whether a claimed relationship is
  supported. A 4/4 citation score must not be described as full answer accuracy.
- Questions supply many symbol names. They exercise lookup more than discovery
  from an unfamiliar bug report. Results should not be generalized to debugging.
- One run per repository avoids artificial repeated-prompt samples, but does
  not estimate run-to-run variance. With ten samples, nearest-rank p95 is the
  maximum; it is not a stable tail-latency estimate.
- Provider-cached input and uncached input must remain separate. The existing
  small-fixture token savings cannot be projected onto this corpus.
- Compact ctx retrieval uses an 800-token estimate internally and a 1,200-token
  MCP output cap. These settings are part of the tested configuration. Tool
  payload estimates are not provider-billed tokens, nor the full JSON envelope.
- A failed session has no valid answer score for aggregate comparisons. Its
  failure must remain visible; excluding failures is not evidence of reliability.

### Product regression exposed by the benchmark

Requests showed that a long first definition can consume the pack before other
requested definitions appear. A focused acceptance regression uses four long
Python functions, independently of a model. At the same 800-token budget,
the [before/after check](results/pack-regression-20260904.json) returns one
requested definition before the change and all four after it. The new pack
prioritizes definitions ahead of graph neighbours and shortens their snippets
when necessary. Partial evidence no longer carries an `answer-ready` hint.

This is a coverage improvement on the documented regression, not a measured
model-token or latency improvement. The old executable is a release binary and
the new executable comes from the test profile; no speed comparison is made.
Tiny budgets, ambiguous names, unresolved qualified names, and the existing
six-symbol/24-candidate lookup limits can still prevent complete coverage.

Resume attempt on 2026-09-04: the ten pinned corpus checkouts and forty
references were restored and verified. `agent_benchmark.py` now records paired
shell/ctx runs, raw events, answers, token usage, protocol violations, and
per-repository deltas. It uses the existing ctx executable and never builds tools.
Its five harness tests (including the original scorer tests) pass.

The [recorded attempt](results/agent-attempt-20260904.json) is incomplete:
Requests shell completed in 48.00 seconds with 4/4 citations; the ctx arm
encountered MCP transport failures followed by an explicit Codex CLI usage-limit
error. The CLI suggested retrying September 11 at 10:06 AM, without a timezone.
No completed pair exists. Failed answers are excluded from aggregate accuracy;
missing token usage is null, not zero. These results cannot establish a token
saving or a competitive ranking. Raw local events remain in
`/tmp/ctx-competitive/agent-run-20260904/`.

The four competitor source snapshots were restored, but no competitor build
was launched during this resume: the current user instructions prohibit builds
without an explicit request in the current message. Competitor execution and
the final comparison remain unfinished.

Run the harness tests without a build:

```console
python3 -m unittest discover -s benchmarks/competitive -p 'test*.py' -v
```

With an existing ctx executable, available Codex quota, and a fresh output
directory, the paired runner accepts:

```console
python3 benchmarks/competitive/agent_benchmark.py \
  --workspace /tmp/ctx-competitive --ctx-binary target/release/ctx \
  --output-dir /tmp/ctx-competitive/new-agent-run
```

The runner stops on the first failed or timed-out agent request and retains
the evidence. Before another measured run, diagnose the observed MCP transport
failure locally and record any changed conditions. Do not silently combine
retries with the initial attempt.

The [local MCP replay](results/mcp-replay-20260904.json) sent all three exact
Requests queries from the failed run through the saved bwrap command in one
session. All returned successfully, stderr was empty, and the process exited
with code 0. No model was called. The transport failure was not reproduced;
its cause remains undetermined. This diagnostic is not an agent measurement.

- Corpus: complete and pinned.
- Forty ground-truth source facts: verified against all ten commits.
- `ctx` cold/unchanged indexing: measured on all ten repositories.
- Competitor source and execution-surface audit: initial review complete.
- Isolated competitor builds and indexes: pending.
- Cross-tool Codex runs and final README comparison: pending.

No cross-tool winner should be claimed until the pending rows are complete.
