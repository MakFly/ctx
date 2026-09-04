from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("benchmark.py")
SPEC = importlib.util.spec_from_file_location("competitive_benchmark", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
BENCHMARK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCHMARK)


class ScoreAnswerTest(unittest.TestCase):
    def test_requires_symbol_path_and_exact_line(self) -> None:
        checks = [{"symbol": "Router.route", "path": "src/router.rs", "line": 42}]
        result = BENCHMARK.score_answer(
            "`Router.route` is defined at `src/router.rs:42`.", checks
        )
        self.assertEqual(1, result["passed"])

    def test_rejects_path_without_citation(self) -> None:
        checks = [{"symbol": "login", "path": "src/auth.py", "line": 10}]
        result = BENCHMARK.score_answer("login is in src/auth.py", checks)
        self.assertEqual(0, result["passed"])


if __name__ == "__main__":
    unittest.main()
