from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Callable

import click

from .briefing import generate_briefing
from .config import ctx_dir, find_ctx
from .db import connect, get_meta
from .gitinfo import git_info
from .graph import graph_query
from .index import index_repository
from .map import build_map
from .pack import pack_query
from .search import search_index


def _emit(data: Any, as_json: bool) -> None:
    if as_json:
        click.echo(json.dumps(data, ensure_ascii=False, indent=2))
    elif isinstance(data, dict) and "hits" in data:
        for hit in data["hits"]:
            click.echo(f"{hit['path']}:{hit['start']} {hit.get('kind', '')} {hit.get('symbol') or ''} — {hit.get('why', '')}")
        click.echo(f"{data['tokens']} tokens; coverage={data['coverage']}; {data['freshness_ms']}ms")
    else:
        click.echo(json.dumps(data, ensure_ascii=False, indent=2))


def _guard(operation: Callable[[], None]) -> None:
    try:
        operation()
    except click.ClickException:
        raise
    except Exception as exc:
        raise click.ClickException(str(exc)) from exc


@click.group()
def app() -> None:
    """Explore codebases locally with bounded evidence."""


@app.command("init")
def init_command() -> None:
    """Create local ctx configuration."""
    root = Path.cwd()
    ctx_dir(root).mkdir(parents=True, exist_ok=True)
    ignore = root / ".ctxignore"
    if not ignore.exists():
        ignore.write_text("# Additional ctx ignore patterns\n*.generated.*\n", encoding="utf-8")
    gitignore = root / ".gitignore"
    if gitignore.exists():
        text = gitignore.read_text(encoding="utf-8")
        if ".ctx/" not in text.splitlines():
            gitignore.write_text(text.rstrip("\n") + "\n.ctx/\n", encoding="utf-8")
    click.echo(f"initialized {ctx_dir(root)}")


@app.command("index")
@click.argument("path", type=click.Path(path_type=Path, file_okay=False), default=".")
@click.option("--watch", is_flag=True)
def index_command(path: Path, watch: bool) -> None:
    """Index a repository into SQLite."""
    if watch:
        click.echo("ctx: --watch est optionnel au MVP; index unique effectué", err=True)

    def work() -> None:
        result = index_repository(path)
        click.echo(f"indexed {result.files} files ({result.changed} changed), {result.symbols} symbols, {result.edges} edges")
        click.echo(str(result.database))
    _guard(work)


@app.command("status")
@click.option("--json", "as_json", is_flag=True)
def status_command(as_json: bool) -> None:
    """Show index and Git freshness."""
    def work() -> None:
        database = find_ctx() / "index.sqlite"
        with connect(database) as conn:
            root = Path(get_meta(conn, "repo_root", str(Path.cwd())))
            counts = {name: conn.execute(f"SELECT count(*) FROM {name}").fetchone()[0] for name in ("files", "symbols", "edges")}
            counts["lsp_edges"] = conn.execute("SELECT count(*) FROM edges WHERE source='lsp'").fetchone()[0]
            stale = any(
                not (root / row["path"]).exists() or (root / row["path"]).stat().st_mtime > float(row["mtime"]) + 1e-6
                for row in conn.execute("SELECT path,mtime FROM files")
            )
            info = git_info(root)
            data = {"sha": info.sha, "indexed_sha": get_meta(conn, "indexed_sha", ""), "dirty": info.dirty,
                    **counts, "database": str(database), "size_bytes": database.stat().st_size, "stale": stale}
        _emit(data, as_json)
    _guard(work)


@app.command("search")
@click.argument("query")
@click.option("--mode", type=click.Choice(["auto", "text", "symbol"]), default="auto", show_default=True)
@click.option("--path", "path_filter")
@click.option("--limit", type=click.IntRange(1), default=20, show_default=True)
@click.option("--budget-tokens", type=click.IntRange(1), default=1500, show_default=True)
@click.option("--json", "as_json", is_flag=True)
def search_command(query: str, mode: str, path_filter: str | None, limit: int, budget_tokens: int, as_json: bool) -> None:
    """Search indexed text and symbols."""
    _guard(lambda: _emit(search_index(query, mode=mode, path_filter=path_filter, limit=limit, budget_tokens=budget_tokens), as_json))


@app.command("graph")
@click.option("--op", required=True, type=click.Choice(["def", "refs", "callers", "callees", "path", "impact"]))
@click.option("--symbol", required=True)
@click.option("--depth", type=click.IntRange(1), default=2, show_default=True)
@click.option("--json", "as_json", is_flag=True)
def graph_command(op: str, symbol: str, depth: int, as_json: bool) -> None:
    """Query definitions and static relationships."""
    _guard(lambda: _emit(graph_query(op, symbol, depth=depth), as_json))


@app.command("pack")
@click.argument("query")
@click.option("--budget-tokens", type=click.IntRange(1), default=2000, show_default=True)
@click.option("--intent", type=click.Choice(["explore", "edit", "review"]), default="explore", show_default=True)
@click.option("--json", "as_json", is_flag=True)
def pack_command(query: str, budget_tokens: int, intent: str, as_json: bool) -> None:
    """Build a bounded evidence pack."""
    _guard(lambda: _emit(pack_query(query, budget_tokens=budget_tokens, intent=intent), as_json))


@app.command("map")
@click.option("--out", type=click.Path(path_type=Path, dir_okay=False), default=Path(".ctx/map.json"), show_default=True)
@click.option("--json", "as_json", is_flag=True)
def map_command(out: Path, as_json: bool) -> None:
    """Create a deterministic repository map."""
    def work() -> None:
        data = build_map()
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        _emit(data, as_json)
    _guard(work)


@app.command("explore")
@click.option("--intent", type=click.Choice(["onboard", "change", "handoff", "impact"]), default="onboard", show_default=True)
@click.option("--focus")
@click.option("--harness", default="none", show_default=True)
@click.option("--out", type=click.Path(path_type=Path, file_okay=False), default=Path(".ctx"), show_default=True)
@click.option("--force", is_flag=True)
@click.option("--json", "as_json", is_flag=True)
def explore_command(intent: str, focus: str | None, harness: str, out: Path, force: bool, as_json: bool) -> None:
    """Index, map, and write a reusable briefing."""
    if harness != "none":
        click.echo("harness claude/codex = v1.1, fallback none", err=True)
        harness = "none"

    def work() -> None:
        root = Path.cwd().resolve()
        if not (ctx_dir(root) / "index.sqlite").exists():
            index_repository(root)
        data, skipped = generate_briefing(root, intent=intent, focus=focus, harness=harness, out=out, force=force)
        if as_json:
            _emit(data, True)
        else:
            click.echo("briefing unchanged (same clean SHA)" if skipped else f"wrote {out / 'briefing.json'} then {out / 'briefing.md'}")
    _guard(work)


@app.command("mcp")
def mcp_command() -> None:
    """Run the MCP server over stdio."""
    def work() -> None:
        from .mcp_server import run
        run()
    _guard(work)


@app.group("lsp")
def lsp_group() -> None:
    """Detect and fetch optional language servers."""


@lsp_group.command("sources")
@click.option("--json", "as_json", is_flag=True)
def lsp_sources_command(as_json: bool) -> None:
    """List the upstream GitHub repositories used by ctx."""
    from .lsp import sources
    _emit({"servers": sources()}, as_json)


@lsp_group.command("status")
@click.option("--json", "as_json", is_flag=True)
def lsp_status_command(as_json: bool) -> None:
    """Detect fetched and PATH-provided language servers."""
    from .lsp import status
    _guard(lambda: _emit(status(Path.cwd().resolve()), as_json))


@lsp_group.command("fetch")
@click.option("--language", "languages", multiple=True,
              type=click.Choice(["all", "python", "typescript", "go", "rust", "php"]),
              help="Language to fetch; repeat the option or omit it for all.")
@click.option("--dry-run", is_flag=True)
@click.option("--force", is_flag=True)
@click.option("--json", "as_json", is_flag=True)
def lsp_fetch_command(languages: tuple[str, ...], dry_run: bool, force: bool, as_json: bool) -> None:
    """Fetch compatible official release artifacts from GitHub."""
    from .lsp import fetch
    _guard(lambda: _emit(fetch(Path.cwd().resolve(), list(languages), dry_run=dry_run, force=force), as_json))


@lsp_group.command("enrich")
@click.argument("path", type=click.Path(path_type=Path, file_okay=False), default=".")
@click.option("--language", "languages", multiple=True,
              type=click.Choice(["all", "python", "typescript", "go", "rust", "php"]))
@click.option("--max-symbols", type=click.IntRange(1), default=500, show_default=True)
@click.option("--timeout", type=click.FloatRange(min=1), default=60.0, show_default=True)
@click.option("--background", is_flag=True, help="Run enrichment in a detached process.")
@click.option("--json", "as_json", is_flag=True)
def lsp_enrich_command(path: Path, languages: tuple[str, ...], max_symbols: int,
                       timeout: float, background: bool, as_json: bool) -> None:
    """Enrich static edges using installed language servers."""
    from .lsp_enrich import enrich, start_background
    operation = start_background if background else enrich
    _guard(lambda: _emit(operation(path.resolve(), list(languages), max_symbols=max_symbols, timeout=timeout), as_json))


@app.command("install")
@click.option("--target", type=click.Choice(["auto", "claude", "codex", "opencode", "cursor", "both", "all"]), default="auto", show_default=True)
@click.option("--dry-run", is_flag=True, help="Detect and print planned changes without writing files.")
@click.option("--json", "as_json", is_flag=True)
def install_command(target: str, dry_run: bool, as_json: bool) -> None:
    """Install ctx exploration skills for agent harnesses."""
    _run_harness_install("install", target, dry_run, as_json)


@app.command("update")
@click.option("--target", type=click.Choice(["auto", "claude", "codex", "opencode", "cursor", "both", "all"]), default="auto", show_default=True)
@click.option("--dry-run", is_flag=True, help="Detect and print planned changes without writing files.")
@click.option("--json", "as_json", is_flag=True)
def update_command(target: str, dry_run: bool, as_json: bool) -> None:
    """Refresh installed ctx agents, skills, and MCP configuration."""
    _run_harness_install("update", target, dry_run, as_json)


def _run_harness_install(mode: str, target: str, dry_run: bool, as_json: bool) -> None:
    def work() -> None:
        from .install_harness import install, installation_plan
        root = Path.cwd().resolve()
        if dry_run:
            plan = installation_plan(root, target, mode=mode)
            if as_json:
                _emit(plan, True)
                return
            for name, details in plan["detected"].items():
                status = "detected" if details["detected"] else "absent"
                signals = ", ".join(details["signals"]) or "no signal"
                click.echo(f"{name}: {status} ({signals})")
            click.echo(f"selected: {', '.join(plan['selected']) or 'none'}")
            for path in plan["files"]:
                click.echo(f"would {mode}: {path}")
            if plan["hint"]:
                click.echo(f"hint: {plan['hint']}")
            return
        verb = "updated" if mode == "update" else "installed"
        for path in install(root, target, mode=mode):
            click.echo(f"{verb} {path}")
    _guard(work)


def main() -> None:
    app()


if __name__ == "__main__":
    main()
