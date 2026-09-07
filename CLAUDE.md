# ctx repository instructions

`CLAUDE.md` is the canonical project instruction file. `AGENTS.md` points to it;
keep this direction and do not replace this file with a symlink to `AGENTS.md`.

## Versioning and releases

Use Semantic Versioning for project releases: bump PATCH for backward-compatible
bug fixes, MINOR for backward-compatible features, and MAJOR for breaking changes.
For a release containing several change types, use the highest required bump.
Apply this policy from the initial public release, `v0.0.1`; do not downgrade an
existing published version or move an existing release tag.

Keep the root `Cargo.toml` package version and the `ctx-code` entry in `Cargo.lock`
in sync. Release tags use `vMAJOR.MINOR.PATCH` and point to the committed release
changes. The vendored `ctx-tgrep` package has its own version; do not bump it just
to match ctx. Preserve version labels in historical benchmark records.

Release notes must describe shipped behavior, actual validation and remaining
limitations. Do not attach stale binaries or claim unexecuted builds/tests passed.
A version bump does not itself authorize compilation or publication; follow the
user's current instructions for builds, commits, pushes and releases.

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
