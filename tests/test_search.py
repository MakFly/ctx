from ctx.index import index_repository
from ctx.search import search_index


def test_login_hit_one_is_definition(mini_repo):
    index_repository(mini_repo)
    result = search_index("login", start=mini_repo)
    assert result["hits"][0]["path"] == "auth.py"
    assert result["hits"][0]["symbol"] == "login"
    assert result["hits"][0]["kind"] == "def"
