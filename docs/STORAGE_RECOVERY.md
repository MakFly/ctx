# Storage recovery and cancellation

## Observed failure on 2026-10-17

The installed executable was `/home/kev/.local/bin/ctx`; its source repository
was `/home/kev/lab/tools/ctx` (package version 0.3.0). The installed binary's
provenance was not verified against that source revision.

One orphaned MCP process, PID 242271 with parent PID 1, held the writer lock on
the index for `/home/kev`. Its stdin pipe had no other remaining file descriptor
and therefore no writer. The WAL occupied 1,972,231,672 bytes. The committed
database had 29 pages, zero files, zero symbols, zero edges and zero content rows.
This was unfinished initial indexing, not two gigabytes of published knowledge.

Before recovery, the committed SQLite state was copied through the backup API.
Only the confirmed orphan was sent SIGTERM using a pidfd after checking its birth
identity again. No native harness session or persistent memory service was
stopped. A SQLite TRUNCATE checkpoint then returned `(0, 0, 0)` and the WAL
occupied zero bytes. `quick_check` returned `ok`; all table definitions and rows
had the same SHA-256 before and after:

```text
b537f5f56355a7c361e27a60ea4b9c5530374a6764d88dd6c3f0b0882abaad81
```

Private recovery receipts and the small committed-state backup are retained in
the KonohaOS installation under
`.konohaos/reports/storage-live-migration-20261017/`. Never delete an occupied
SQLite WAL manually; it may contain the only committed copy of recent pages.

## Corrected lifecycle

Previously `WatchSession::drop` set its stop flag and joined a worker whose
initial and incremental index operations never checked that flag. Cancellation
now reaches directory registration, repository traversal, document updates,
staging and the final pre-commit boundary. Errors unwind the SQLite transaction
and unpublished generation guard together. A complete transaction may still
publish if cancellation races just after the final check; publication remains
atomic.

The cached RMCP 3.2.0 source maps transport EOF to `QuitReason::Closed` in its
service loop; `RunningService::waiting` joins that loop. The handler-drain path
has an EOF timeout. This is static dependency evidence, not a new live shutdown
probe. The patch does not add independent parent-death detection.

The focused regression case covers cancellation after the first document write
and at finalization, checking that the original document and generation survive.
It was authored but not executed. Formatting, locked offline Cargo metadata and
static review passed. No compilation or runtime tests were authorized, and the
installed executable has not been replaced with this source change.

## Remaining storage limits

The graph and full-text generation are published as one unit. SQLite retains
the full write transaction until staging succeeds, so normal large indexing can
still create a large WAL. `journal_size_limit` and automatic checkpoints cannot
cap a transaction that is still open. Cooperative cancellation does not make
file parsing, a SQL statement, external sorting or merging interruptible.

Do not introduce intermediate commits without a replacement publication model:
they would expose graph changes independently of their text generation. A hard
WAL budget would need an explicit abort policy, including how to report that a
large repository cannot finish indexing under the configured budget.

The SQL `content_fts` is an external-content index over `file_contents`, not a
second copy of the original text. Ranked full-text queries still use it in
`src/search.rs`; exact matching also uses the immutable trigram index. Removing
one without adapting retrieval would change supported behavior. The first
immediate improvement is cancellation and careful workspace scope, rather than
removing a live search path on an assumption of duplication.
