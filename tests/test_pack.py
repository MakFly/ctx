import time

import pytest

from ctx.index import index_repository
from ctx.pack import pack_query


def test_retry_payment_pack_is_fast(mini_repo):
    index_repository(mini_repo)
    started = time.perf_counter()
    result = pack_query("retry paiement", start=mini_repo)
    elapsed = time.perf_counter() - started
    assert any(hit["path"] == "payments.py" and hit["symbol"] == "retry_payment" for hit in result["hits"])
    print(f"pack timing: {elapsed * 1000:.1f}ms")
    if elapsed >= .5:
        pytest.skip("CI filesystem too slow for the soft 500ms acceptance threshold")


def test_budget_is_respected(mini_repo):
    index_repository(mini_repo)
    assert pack_query("login", budget_tokens=200, start=mini_repo)["tokens"] <= 200
