from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from agent_benchmark import parse_events, summarize


class AgentBenchmarkTest(unittest.TestCase):
    def test_counts_completed_calls_once_and_flags_wrong_tool(self) -> None:
        call = {"id": "x", "type": "command_execution"}
        events = [
            {"type": "item.started", "item": call},
            {"type": "item.completed", "item": call},
            {"type": "turn.completed", "usage": {
                "input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 10}},
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            path.write_text("\n".join(map(json.dumps, events)))
            result = parse_events(path, "ctx")
        self.assertEqual(1, result["tool_calls"])
        self.assertEqual(["x"], result["protocol_violations"])
        self.assertEqual(40, result["uncached_input_tokens"])
        self.assertIsNone(result["reasoning_output_tokens"])

    def test_failed_turn_does_not_fabricate_zero_tokens(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "events.jsonl"
            path.write_text('{"type":"turn.failed","error":{"message":"quota"}}\n')
            result = parse_events(path, "shell")
        self.assertFalse(result["completed"])
        self.assertIsNone(result["input_tokens"])
        self.assertEqual(1, len(result["errors"]))

    def test_pairs_by_repository_and_excludes_failed_runs(self) -> None:
        rows = []
        for repo, variant, elapsed, status in [
            ("a", "shell", 10, "completed"), ("a", "ctx", 5, "completed"),
            ("b", "shell", 100, "completed"), ("b", "ctx", 1, "failed"),
        ]:
            rows.append(dict(id=repo, variant=variant, elapsed_seconds=elapsed, status=status,
                             score={"passed": 4, "total": 4}, input_tokens=None,
                             cached_input_tokens=None, uncached_input_tokens=None,
                             output_tokens=None, tool_calls=1, mcp_result_json_bytes=0))
        result = summarize(rows)
        self.assertEqual(1, result["paired"]["repositories"])
        self.assertEqual(-50, result["paired"]["median_delta_percent"]["elapsed_seconds"])
        self.assertEqual(1, result["ctx"]["completed_repositories"])


if __name__ == "__main__":
    unittest.main()
