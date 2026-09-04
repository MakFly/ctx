from ctx.graph import graph_query
from ctx.index import index_repository


def test_definition_and_callers(mini_repo):
    index_repository(mini_repo)
    definition = graph_query("def", "AuthService", start=mini_repo)
    assert definition["hits"][0]["path"] == "auth.py"
    callers = graph_query("callers", "login", start=mini_repo)
    assert "app.py" in {hit["path"] for hit in callers["hits"]}
    assert callers["coverage"] in {"complete", "partial"}
