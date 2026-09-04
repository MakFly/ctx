import json
import re
import time

from ctx.briefing import generate_briefing
from ctx.index import index_repository


def test_explore_writes_valid_briefing_then_skips(mini_repo):
    index_repository(mini_repo)
    out = mini_repo / ".ctx"
    data, skipped = generate_briefing(mini_repo, intent="change", focus="login", harness="none", out=out)
    assert not skipped
    parsed = json.loads((out / "briefing.json").read_text())
    assert {"schema", "sha", "hits", "map"} <= parsed.keys()
    markdown = (out / "briefing.md").read_text()
    citations = re.findall(r"`([^`]+):(\d+)`", markdown)
    assert len(citations) >= 3
    assert all((mini_repo / path).exists() for path, _ in citations)
    mtimes = ((out / "briefing.json").stat().st_mtime_ns, (out / "briefing.md").stat().st_mtime_ns)
    time.sleep(.001)
    second, skipped = generate_briefing(mini_repo, intent="change", focus="login", harness="none", out=out)
    assert skipped and second["skipped"] is True
    assert mtimes == ((out / "briefing.json").stat().st_mtime_ns, (out / "briefing.md").stat().st_mtime_ns)
