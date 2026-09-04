from ctx.briefing import detect_stack
from ctx.graph import graph_query
from ctx.index import index_repository
from ctx.map import _routes, build_map
from ctx.parse import parse_source
from ctx.search import search_index


def test_polyglot_symbols_and_calls(mini_repo):
    index_repository(mini_repo)
    expected = {
        "createOrder": "api.ts",
        "CheckoutService": "service.go",
        "retry_charge": "worker.rs",
        "BillingController": "billing.php",
    }
    for symbol, path in expected.items():
        result = search_index(symbol, start=mini_repo)
        assert result["hits"], symbol
        assert result["hits"][0]["path"] == path

    callers = graph_query("callers", "savePayment", start=mini_repo)
    assert "service.go" in {hit["path"] for hit in callers["hits"]}

    ts_symbols, _ = parse_source((mini_repo / "api.ts").read_text(), "typescript")
    assert any(symbol.name == "createOrder" and symbol.kind == "function" for symbol in ts_symbols)


def test_polyglot_routes_and_stack(mini_repo):
    index_repository(mini_repo)
    routes = {(item["method"], item["route"], item["path"]) for item in build_map(mini_repo)["router"]}
    assert ("POST", "/login", "app.py") in routes
    assert ("POST", "/orders", "api.ts") in routes
    assert ("POST", "/payments", "billing.php") in routes
    assert {"Python", "JavaScript/TypeScript", "Go", "Rust", "PHP"} <= set(detect_stack(mini_repo))


def test_framework_route_families(tmp_path):
    sources = {
        "urls.py": "urlpatterns = [path('users/', views.users)]\n",
        "controller.ts": "@Get('/health')\nhealth() {}\n",
        "app/api/items/[id]/route.ts": "export async function DELETE() {}\n",
        "main.go": 'router.GET("/ready", ready)\nhttp.HandleFunc("/metrics", metrics)\n',
        "lib.rs": '#[get("/status")]\nfn status() {}\nRouter::new().route("/jobs", post(create_job));\n',
        "routes.php": "Route::put('/accounts', handler);\n#[Route('/profile', methods: ['PATCH'])]\n",
    }
    for rel, content in sources.items():
        path = tmp_path / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
    routes = {(item["method"], item["route"]) for item in _routes(tmp_path, list(sources))}
    assert {
        ("ANY", "users/"), ("GET", "/health"), ("DELETE", "/api/items/:id"),
        ("GET", "/ready"), ("ANY", "/metrics"), ("GET", "/status"),
        ("POST", "/jobs"), ("PUT", "/accounts"), ("PATCH", "/profile"),
    } <= routes
