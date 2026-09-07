# ctx repository instructions

`CLAUDE.md` is the canonical project instruction file. `AGENTS.md` points to it;
keep this direction and do not replace this file with a symlink to `AGENTS.md`.

## Maintenance memory

Before changing or upgrading search, indexing, the watcher, or vendored tgrep
components, read [the tgrep maintenance memory](docs/TGREP_MAINTENANCE.md).
It identifies the upstream revision, local adaptations, invariants, migration
decisions, and verification requirements.

Update that document and the vendored provenance whenever the integration's
architecture, upstream revision, interfaces, defaults, or validation status
changes. Record evidence and remaining limitations; do not claim unexecuted
checks passed. Treat the actual code and current user instructions as
authoritative if the memory has become stale.

Keep project instructions and maintenance memory in English. Preserve existing
working-tree changes. Do not launch builds, compilation-based checks, or
development servers without the authorization required by the user's current
instructions; a command documented in the memory does not authorize running it.
