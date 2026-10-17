# Setup and integration

Use this reference for installation, integration updates, and connection or
index problems. Inspect the current state before choosing a repair.

## Binary versus project setup

`command -v ctx`, `ctx --version`, and `ctx --help` identify the executable and
its supported interface. Bare `ctx` lists commands; `ctx gain` shows usage.
The binary is installed once per host, while each repository has its own index
and optional harness configuration. An SSH terminal uses the remote host's
binary, configuration, and filesystem.

When installation is requested and the binary is absent, use the known ctx
source checkout or the user's chosen source. Do not mistake the repository
being searched for the ctx source checkout. A source installation needs the
Rust version declared in ctx's Cargo.toml. From that source checkout:

```sh
cargo install --path . --root "$HOME/.local" --locked --force
```

Ensure that destination is in the harness's PATH and verify the installed
executable. Respect any compilation restrictions or standing authorization in
the current task. A usable existing binary does not need recompilation merely
because another repository needs indexing.

## Existing integration and effective index

Inspect available tools and relevant project configuration. Check only the
needed fields; do not print credentials or entire harness configurations.

- An existing direct ctx MCP server can serve the repository already.
- A KonohaOS session may expose `konoha_code_context` and store its index outside
  the repository through `CTX_DIR`. Reuse that route. For CLI diagnostics, use
  the exact observed `CTX_DIR`; do not infer it from `.ctx/` or reconstruct a hash.
- With no integration, a project setup request can create one for the requested
  harness. A missing skill is not proof that the MCP server is missing.

For a new standalone project, run from its root before starting the agent:

```sh
ctx init
ctx index .
ctx install --target codex --dry-run
ctx install --target codex
```

Replace `codex` with the selected target. The dry run previews configuration
changes; it is not an extra approval gate when setup is already authorized.
Use `all` only when all integrations were requested. `ctx install` writes the
integration and skill, but does not itself perform the initial index build.
Keep `.ctx/` out of version control according to the repository's ignore policy.

| Target | MCP configuration | Project skill folder |
|---|---|---|
| `claude` | `.mcp.json` | `.claude/skills/ctx-explore/` |
| `codex` | `.codex/config.toml` | `.agents/skills/ctx-explore/` |
| `grok` | `.grok/config.toml` | `.grok/skills/ctx-explore/` |
| `opencode` | `opencode.json` | `.opencode/skills/ctx-explore/` |
| `cursor` | `.cursor/mcp.json` | `.cursor/skills/ctx-explore/` |

The installer merges supported configuration and refreshes its managed
instruction blocks. Preserve unrelated settings, user rules, and the direction
of any existing CLAUDE.md/AGENTS.md symlink. Project trust remains controlled
by the user and harness.

Open or refresh a session in the project to load changed integrations. The
harness starts `ctx mcp`; running that stdio server manually is not an
interactive search interface. Do not restart someone else's active session.

## Updates and discovery

`ctx update --target codex --dry-run` previews an integration refresh and
`ctx update --target codex` applies it. This refreshes project files, not the
ctx executable. Keep the skill's `references/` and `agents/` files with SKILL.md.

For user-level availability, the native locations are `~/.claude/skills/`,
`~/.agents/skills/` for current Codex, `~/.grok/skills/`,
`~/.config/opencode/skills/`, and `~/.cursor/skills/`. Older Codex installations
may also load `~/.codex/skills/`; verify the installed version's discovery
instead of adding duplicate entries blindly. A shared directory or symlink can
keep these definitions in sync on one host. Project copies remain independent.

Claude Code can invoke `/ctx-explore`; Codex can select `$ctx-explore`.
For Grok, use its skill selector or `/ctx-explore`, checking `grok inspect --json`
for the discovered name and source. If names collide, Grok's qualified command
can distinguish user and project definitions. For other harnesses, inspect
their skill list rather than assuming identical slash-command syntax.

User-level installation is local to the machine and is not cloud publication.
Documented skill locations are not proof that a running session loaded a file.

Discovery references, checked 2026-10-17:
[Claude Code](https://code.claude.com/docs/en/skills),
[Codex](https://learn.chatgpt.com/docs/build-skills),
[Grok Build](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/08-skills.md),
[OpenCode](https://opencode.ai/docs/skills/),
[Cursor](https://cursor.com/docs/skills).
