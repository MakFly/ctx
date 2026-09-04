from __future__ import annotations

import ast
import bisect
import functools
import re
from dataclasses import dataclass
from typing import Any, Callable


@dataclass(frozen=True)
class Symbol:
    name: str
    qualname: str
    kind: str
    start: int
    end: int
    sig: str
    snippet: str


@dataclass(frozen=True)
class Edge:
    src_name: str | None
    dst_name: str
    kind: str
    line: int


def _slice(lines: list[str], start: int, end: int, limit: int = 2_000) -> str:
    return "".join(lines[max(0, start - 1):end])[:limit].rstrip()


def _line_locator(text: str) -> Callable[[int], int]:
    starts = [0]
    starts.extend(match.end() for match in re.finditer("\n", text))
    return lambda offset: bisect.bisect_right(starts, offset)


def _block_end(lines: list[str], start: int) -> int:
    depth = 0
    seen = False
    for number in range(start, min(len(lines), start + 500) + 1):
        line = lines[number - 1]
        depth += line.count("{") - line.count("}")
        seen = seen or "{" in line
        if seen and depth <= 0:
            return number
    return min(len(lines), start + 120)


def _owner(symbols: list[Symbol], line: int) -> str | None:
    candidates = [symbol for symbol in symbols if symbol.start <= line <= symbol.end]
    return min(candidates, key=lambda symbol: symbol.end - symbol.start).name if candidates else None


def _is_declaration(symbols: list[Symbol], line: int, name: str) -> bool:
    return any(symbol.start == line and symbol.name == name for symbol in symbols)


def _dedupe(symbols: list[Symbol], edges: list[Edge]) -> tuple[list[Symbol], list[Edge]]:
    priority = {"method": 5, "class": 4, "function": 3, "type": 2, "variable": 1}
    symbol_map: dict[tuple[int, str], Symbol] = {}
    for item in symbols:
        key = (item.start, item.name)
        if key not in symbol_map or priority.get(item.kind, 0) > priority.get(symbol_map[key].kind, 0):
            symbol_map[key] = item
    edge_map = {(item.line, item.src_name, item.dst_name, item.kind): item for item in edges}
    return (
        sorted(symbol_map.values(), key=lambda item: (item.start, item.name, item.kind)),
        sorted(edge_map.values(), key=lambda item: (item.line, item.kind, item.dst_name)),
    )


def parse_python(text: str) -> tuple[list[Symbol], list[Edge]]:
    lines = text.splitlines(keepends=True)
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return [], []
    symbols: list[Symbol] = []
    edges: list[Edge] = []

    def variable(name: str, line: int) -> None:
        symbols.append(Symbol(name, name, "variable", line, line, lines[line - 1].strip()[:300], _slice(lines, line, line)))

    class Visitor(ast.NodeVisitor):
        current: list[str] = []
        containers: list[str] = []

        def _function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
            qualname = ".".join([*self.current, node.name])
            kind = "method" if self.containers and self.containers[-1] == "class" else "function"
            end = node.end_lineno or node.lineno
            sig = lines[node.lineno - 1].strip() if node.lineno <= len(lines) else node.name
            symbols.append(Symbol(node.name, qualname, kind, node.lineno, end, sig, _slice(lines, node.lineno, end)))
            self.current.append(node.name)
            self.containers.append("function")
            self.generic_visit(node)
            self.containers.pop()
            self.current.pop()

        visit_FunctionDef = _function
        visit_AsyncFunctionDef = _function

        def visit_ClassDef(self, node: ast.ClassDef) -> None:
            qualname = ".".join([*self.current, node.name])
            end = node.end_lineno or node.lineno
            symbols.append(Symbol(node.name, qualname, "class", node.lineno, end, lines[node.lineno - 1].strip(), _slice(lines, node.lineno, end)))
            self.current.append(node.name)
            self.containers.append("class")
            self.generic_visit(node)
            self.containers.pop()
            self.current.pop()

        def visit_Call(self, node: ast.Call) -> None:
            name = _ast_name(node.func)
            if name:
                edges.append(Edge(self.current[-1] if self.current else None, name.rsplit(".", 1)[-1], "call", node.lineno))
            self.generic_visit(node)

        def visit_Import(self, node: ast.Import) -> None:
            for alias in node.names:
                edges.append(Edge(self.current[-1] if self.current else None, alias.name.rsplit(".", 1)[-1], "import", node.lineno))

        def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
            for alias in node.names:
                edges.append(Edge(self.current[-1] if self.current else None, alias.name, "import", node.lineno))

        def visit_AnnAssign(self, node: ast.AnnAssign) -> None:
            if isinstance(node.target, ast.Name) and not self.current:
                variable(node.target.id, node.lineno)
            self.generic_visit(node)

        def visit_Assign(self, node: ast.Assign) -> None:
            if not self.current:
                for target in node.targets:
                    if isinstance(target, ast.Name):
                        variable(target.id, node.lineno)
            self.generic_visit(node)

    Visitor().visit(tree)
    return _dedupe(symbols, edges)


def _ast_name(node: ast.expr) -> str | None:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        base = _ast_name(node.value)
        return f"{base}.{node.attr}" if base else node.attr
    return None


JS_DECL_RE = re.compile(
    r"^[ \t]*(?:export\s+)?(?:default\s+)?(?:(?:declare|abstract)\s+)?(?:(async)\s+)?"
    r"(function|class|interface|type|enum|namespace|const|let|var)\s+([A-Za-z_$][\w$]*)", re.M,
)
JS_ARROW_RE = re.compile(
    r"^[ \t]*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=\n]+)?=\s*(?:async\s*)?(?:\([^\n]*\)|[A-Za-z_$][\w$]*)\s*(?::[^=\n]+)?=>", re.M,
)
JS_METHOD_RE = re.compile(r"^[ \t]*(?:(?:public|private|protected|static|readonly|abstract|override)\s+)*(?:async\s+)?([A-Za-z_$][\w$]*)\s*\([^\n;]*\)\s*(?::[^\{\n]+)?\{", re.M)
JS_CALL_RE = re.compile(r"\b([A-Za-z_$][\w$]*(?:\??\.[A-Za-z_$][\w$]*)*)\s*\(")
JS_IMPORT_RE = re.compile(r"(?:import|export)\s+(?:[^'\"]*?from\s*)?['\"]([^'\"]+)['\"]|require\s*\(\s*['\"]([^'\"]+)['\"]")


def parse_javascript(text: str) -> tuple[list[Symbol], list[Edge]]:
    lines = text.splitlines(keepends=True)
    line_at = _line_locator(text)
    symbols: list[Symbol] = []
    edges: list[Edge] = []
    occupied: set[tuple[int, str]] = set()
    class_ranges: list[tuple[int, int, str]] = []
    for match in JS_DECL_RE.finditer(text):
        raw_kind, name = match.group(2), match.group(3)
        line = line_at(match.start())
        end = _block_end(lines, line) if raw_kind in {"function", "class", "namespace"} else line
        kind = {"function": "function", "class": "class", "interface": "type", "type": "type", "enum": "type", "namespace": "type"}.get(raw_kind, "variable")
        symbols.append(Symbol(name, name, kind, line, end, lines[line - 1].strip()[:300], _slice(lines, line, end)))
        if raw_kind == "class":
            class_ranges.append((line, end, name))
        occupied.add((line, name))
    for match in JS_ARROW_RE.finditer(text):
        name = match.group(1)
        line = line_at(match.start())
        end = _block_end(lines, line) if "{" in lines[line - 1] else line
        symbols.append(Symbol(name, name, "function", line, end, lines[line - 1].strip()[:300], _slice(lines, line, end)))
        occupied.add((line, name))
    for match in JS_METHOD_RE.finditer(text):
        name = match.group(1)
        line = line_at(match.start())
        if (line, name) in occupied or name in {"if", "for", "while", "switch", "catch", "function"}:
            continue
        parent = next((class_name for start, end, class_name in class_ranges if start < line <= end), None)
        if parent:
            end = _block_end(lines, line)
            symbols.append(Symbol(name, f"{parent}.{name}", "method", line, end, lines[line - 1].strip()[:300], _slice(lines, line, end)))
    for match in JS_CALL_RE.finditer(text):
        raw = match.group(1).replace("?.", ".")
        name = raw.rsplit(".", 1)[-1]
        if raw in {"if", "for", "while", "switch", "catch", "function", "typeof", "async"} or _is_declaration(symbols, line_at(match.start()), name):
            continue
        line = line_at(match.start())
        edges.append(Edge(_owner(symbols, line), name, "call", line))
    for match in JS_IMPORT_RE.finditer(text):
        module = match.group(1) or match.group(2)
        edges.append(Edge(None, module.rstrip("/").rsplit("/", 1)[-1], "import", line_at(match.start())))
    return _dedupe(symbols, edges)


GO_DECL_RE = re.compile(r"^[ \t]*func\s*(?:\(\s*[^)]*?\s+\*?([A-Za-z_]\w*)\s*\)\s*)?([A-Za-z_]\w*)\s*\([^\n]*", re.M)
GO_TYPE_RE = re.compile(r"^[ \t]*type\s+([A-Za-z_]\w*)\s+(struct|interface|\w+)", re.M)
GO_CALL_RE = re.compile(r"\b([A-Za-z_]\w*(?:\.[A-Za-z_]\w*)?)\s*\(")


def parse_go(text: str) -> tuple[list[Symbol], list[Edge]]:
    lines = text.splitlines(keepends=True)
    line_at = _line_locator(text)
    symbols: list[Symbol] = []
    edges: list[Edge] = []
    for match in GO_TYPE_RE.finditer(text):
        name, raw_kind = match.groups()
        line = line_at(match.start())
        end = _block_end(lines, line) if raw_kind in {"struct", "interface"} else line
        kind = "class" if raw_kind == "struct" else "type"
        symbols.append(Symbol(name, name, kind, line, end, lines[line - 1].strip()[:300], _slice(lines, line, end)))
    for match in GO_DECL_RE.finditer(text):
        receiver, name = match.groups()
        line = line_at(match.start())
        end = _block_end(lines, line)
        symbols.append(Symbol(name, f"{receiver}.{name}" if receiver else name, "method" if receiver else "function", line, end, lines[line - 1].strip()[:300], _slice(lines, line, end)))
    for match in GO_CALL_RE.finditer(text):
        raw = match.group(1)
        name = raw.rsplit(".", 1)[-1]
        line = line_at(match.start())
        if raw in {"func", "if", "for", "switch", "select", "go", "defer"} or _is_declaration(symbols, line, name):
            continue
        edges.append(Edge(_owner(symbols, line), name, "call", line))
    for match in re.finditer(r"^[ \t]*import\s*(?:\((.*?)\)|(?:[._A-Za-z]\w*\s+)?['\"]([^'\"]+)['\"])", text, re.M | re.S):
        modules = re.findall(r"['\"]([^'\"]+)['\"]", match.group(1) or "") or ([match.group(2)] if match.group(2) else [])
        edges.extend(Edge(None, module.rsplit("/", 1)[-1], "import", line_at(match.start())) for module in modules)
    return _dedupe(symbols, edges)


RUST_DECL_RE = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const|extern\s+(?:\"[^\"]+\"\s+)?)\s+)*(fn|struct|enum|trait|type|const|static|mod)\s+([A-Za-z_]\w*)", re.M,
)
RUST_CALL_RE = re.compile(r"(?:\b([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)|\.([A-Za-z_]\w*))\s*(?:!\s*)?\(")


def parse_rust(text: str) -> tuple[list[Symbol], list[Edge]]:
    lines = text.splitlines(keepends=True)
    line_at = _line_locator(text)
    symbols: list[Symbol] = []
    edges: list[Edge] = []
    impl_ranges: list[tuple[int, int, str]] = []
    for match in re.finditer(r"^[ \t]*impl(?:\s*<[^>]+>)?(?:\s+[^\n{]+\s+for)?\s+([A-Za-z_]\w*)[^\n{]*\{", text, re.M):
        line = line_at(match.start())
        impl_ranges.append((line, _block_end(lines, line), match.group(1)))
    for match in RUST_DECL_RE.finditer(text):
        raw_kind, name = match.groups()
        line = line_at(match.start())
        end = _block_end(lines, line) if raw_kind in {"fn", "struct", "enum", "trait", "mod"} else line
        parent = next((target for start, stop, target in impl_ranges if start < line <= stop), None)
        kind = {"fn": "method" if parent else "function", "struct": "class", "enum": "type", "trait": "type", "type": "type", "mod": "type"}.get(raw_kind, "variable")
        symbols.append(Symbol(name, f"{parent}.{name}" if parent else name, kind, line, end, lines[line - 1].strip()[:300], _slice(lines, line, end)))
    for match in RUST_CALL_RE.finditer(text):
        raw = match.group(1) or match.group(2)
        name = raw.rsplit("::", 1)[-1]
        line = line_at(match.start())
        if raw in {"if", "for", "while", "match", "loop", "fn"} or _is_declaration(symbols, line, name):
            continue
        edges.append(Edge(_owner(symbols, line), name, "call", line))
    for match in re.finditer(r"^[ \t]*use\s+([^;]+);", text, re.M):
        names = re.findall(r"[A-Za-z_]\w*", match.group(1))
        if names:
            edges.append(Edge(None, names[-1], "import", line_at(match.start())))
    return _dedupe(symbols, edges)


PHP_DECL_RE = re.compile(r"^[ \t]*(?:(?:final|abstract|readonly|public|protected|private|static)\s+)*(class|interface|trait|enum|function)\s+&?\s*([A-Za-z_][\w]*)", re.M | re.I)
PHP_CALL_RE = re.compile(r"(?:(?:->|::)([A-Za-z_]\w*)|\b([A-Za-z_]\w*))\s*\(")


def parse_php(text: str) -> tuple[list[Symbol], list[Edge]]:
    lines = text.splitlines(keepends=True)
    line_at = _line_locator(text)
    symbols: list[Symbol] = []
    edges: list[Edge] = []
    class_ranges: list[tuple[int, int, str]] = []
    declarations = list(PHP_DECL_RE.finditer(text))
    for match in declarations:
        raw_kind, name = match.group(1).lower(), match.group(2)
        if raw_kind in {"class", "interface", "trait", "enum"}:
            line = line_at(match.start())
            class_ranges.append((line, _block_end(lines, line), name))
    for match in declarations:
        raw_kind, name = match.group(1).lower(), match.group(2)
        line = line_at(match.start())
        end = _block_end(lines, line)
        parent = next((class_name for start, stop, class_name in class_ranges if start < line <= stop), None)
        kind = "method" if raw_kind == "function" and parent else ("function" if raw_kind == "function" else ("class" if raw_kind == "class" else "type"))
        symbols.append(Symbol(name, f"{parent}.{name}" if parent and raw_kind == "function" else name, kind, line, end, lines[line - 1].strip()[:300], _slice(lines, line, end)))
    for match in PHP_CALL_RE.finditer(text):
        name = match.group(1) or match.group(2)
        line = line_at(match.start())
        if name.lower() in {"if", "for", "foreach", "while", "switch", "catch", "function", "isset", "empty"} or _is_declaration(symbols, line, name):
            continue
        edges.append(Edge(_owner(symbols, line), name, "call", line))
    for match in re.finditer(r"^[ \t]*(?:use\s+([^;]+)|(?:require|require_once|include|include_once)\s*\(?\s*['\"]([^'\"]+))", text, re.M | re.I):
        raw = match.group(1) or match.group(2) or ""
        names = re.findall(r"[A-Za-z_]\w*", raw.replace("\\", " "))
        if names:
            edges.append(Edge(None, names[-1], "import", line_at(match.start())))
    return _dedupe(symbols, edges)


FALLBACKS: dict[str, Callable[[str], tuple[list[Symbol], list[Edge]]]] = {
    "python": parse_python, "javascript": parse_javascript, "typescript": parse_javascript,
    "tsx": parse_javascript, "go": parse_go, "rust": parse_rust, "php": parse_php,
}


def parse_source(text: str, lang: str) -> tuple[list[Symbol], list[Edge]]:
    tree = _tree_sitter_parse(text, lang)
    fallback_symbols, fallback_edges = FALLBACKS[lang](text)
    if tree is None:
        return fallback_symbols, fallback_edges
    symbols = _tree_sitter_symbols(tree.root_node, text)
    edges = _tree_sitter_edges(tree.root_node, text, symbols)
    return _dedupe([*symbols, *fallback_symbols], [*edges, *fallback_edges])


def _tree_sitter_parse(text: str, lang: str) -> Any | None:
    parser = _tree_sitter_parser(lang)
    return parser.parse(text.encode("utf-8")) if parser is not None else None


@functools.lru_cache(maxsize=8)
def _tree_sitter_parser(lang: str) -> Any | None:
    try:
        from tree_sitter import Language, Parser
        if lang == "python":
            import tree_sitter_python as grammar
            capsule = grammar.language()
        elif lang == "javascript":
            import tree_sitter_javascript as grammar
            capsule = grammar.language()
        elif lang in {"typescript", "tsx"}:
            import tree_sitter_typescript as grammar
            capsule = grammar.language_tsx() if lang == "tsx" else grammar.language_typescript()
        elif lang == "go":
            import tree_sitter_go as grammar
            capsule = grammar.language()
        elif lang == "rust":
            import tree_sitter_rust as grammar
            capsule = grammar.language()
        elif lang == "php":
            import tree_sitter_php as grammar
            capsule = grammar.language_php()
        else:
            return None
        language = Language(capsule)
        try:
            return Parser(language)
        except TypeError:
            parser = Parser()
            parser.set_language(language)
            return parser
    except (ImportError, AttributeError, TypeError, ValueError):
        return None


def _tree_sitter_symbols(root: Any, text: str) -> list[Symbol]:
    lines = text.splitlines(keepends=True)
    source = text.encode("utf-8")
    kinds = {
        "function_definition": "function", "function_declaration": "function", "generator_function_declaration": "function",
        "method_definition": "method", "method_declaration": "method", "function_item": "function",
        "class_definition": "class", "class_declaration": "class", "struct_item": "class",
        "interface_declaration": "type", "type_alias_declaration": "type", "type_declaration": "type",
        "enum_declaration": "type", "enum_item": "type", "trait_item": "type",
        "variable_declarator": "variable", "const_item": "variable", "static_item": "variable",
    }
    containers = {"class_definition", "class_declaration", "struct_item", "trait_item"}
    symbols: list[Symbol] = []

    def visit(node: Any, parents: list[str]) -> None:
        kind = kinds.get(node.type)
        name_node = node.child_by_field_name("name") if kind else None
        next_parents = parents
        if kind and name_node is not None:
            name = source[name_node.start_byte:name_node.end_byte].decode("utf-8", errors="replace").lstrip("$")
            start, end = node.start_point[0] + 1, node.end_point[0] + 1
            if name:
                symbols.append(Symbol(name, ".".join([*parents, name]), kind, start, end, lines[start - 1].strip()[:300], _slice(lines, start, end)))
                if node.type in containers:
                    next_parents = [*parents, name]
        for child in node.children:
            visit(child, next_parents)

    visit(root, [])
    return symbols


def _tree_sitter_edges(root: Any, text: str, symbols: list[Symbol]) -> list[Edge]:
    source = text.encode("utf-8")
    edges: list[Edge] = []
    calls = {"call", "call_expression", "function_call_expression", "member_call_expression", "scoped_call_expression", "object_creation_expression", "macro_invocation"}
    imports = {"import_statement", "import_from_statement", "import_declaration", "use_declaration", "namespace_use_declaration", "include_expression"}

    def node_text(node: Any) -> str:
        return source[node.start_byte:node.end_byte].decode("utf-8", errors="replace")

    def visit(node: Any) -> None:
        line = node.start_point[0] + 1
        if node.type in calls:
            target = node.child_by_field_name("function") or node.child_by_field_name("name")
            raw = node_text(target) if target is not None else node_text(node).split("(", 1)[0]
            names = re.findall(r"[A-Za-z_$][\w$]*", raw)
            if names:
                edges.append(Edge(_owner(symbols, line), names[-1].lstrip("$"), "call", line))
        elif node.type in imports:
            raw = node_text(node)
            quoted = re.findall(r"['\"]([^'\"]+)['\"]", raw)
            names = [item.rstrip("/").rsplit("/", 1)[-1] for item in quoted]
            if not names:
                identifiers = re.findall(r"[A-Za-z_]\w*", raw)
                names = identifiers[-1:] if identifiers else []
            edges.extend(Edge(_owner(symbols, line), name, "import", line) for name in names)
        for child in node.children:
            visit(child)

    visit(root)
    return edges
