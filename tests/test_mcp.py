from __future__ import annotations

import asyncio
import json
import os
import sys
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from ctx.index import index_repository


def test_mcp_stdio_lists_and_calls_tools(mini_repo: Path) -> None:
    index_repository(mini_repo)

    async def exercise() -> None:
        def payload(result):
            assert not result.isError
            if result.structuredContent is not None:
                return result.structuredContent
            assert result.content and result.content[0].type == "text"
            return json.loads(result.content[0].text)

        env = dict(os.environ)
        source = Path(__file__).parents[1] / "src"
        env["PYTHONPATH"] = os.pathsep.join(filter(None, (str(source), env.get("PYTHONPATH", ""))))
        env["CTX_DIR"] = str(mini_repo / ".ctx")
        parameters = StdioServerParameters(
            command=sys.executable,
            args=["-m", "ctx.cli", "mcp"],
            cwd=mini_repo,
            env=env,
        )
        async with stdio_client(parameters) as (reader, writer):
            async with ClientSession(reader, writer) as session:
                await session.initialize()
                listed = await session.list_tools()
                assert {tool.name for tool in listed.tools} == {
                    "ctx_file", "ctx_graph", "ctx_pack", "ctx_search",
                }
                search = payload(await session.call_tool("ctx_search", {"query": "login"}))
                assert search["hits"][0]["path"] == "auth.py"
                assert search["hits"][0]["symbol"] == "login"
                graph = payload(await session.call_tool("ctx_graph", {"op": "callers", "symbol": "login"}))
                assert "app.py" in {hit["path"] for hit in graph["hits"]}
                pack = payload(await session.call_tool("ctx_pack", {"query": "retry paiement"}))
                assert "payments.py" in {hit["path"] for hit in pack["hits"]}
                files = payload(await session.call_tool("ctx_file", {"q": "auth"}))
                assert "auth.py" in {hit["path"] for hit in files["hits"]}

    asyncio.run(exercise())
