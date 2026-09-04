use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use regex::Regex;
use tree_sitter::{Language, Node, Parser};

use crate::model::{Edge, Symbol};

pub fn parse_source(source: &str, language_name: &str) -> Result<(Vec<Symbol>, Vec<Edge>)> {
    let language = language_for(language_name)?;
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .with_context(|| format!("grammaire tree-sitter invalide: {language_name}"))?;
    let tree = parser
        .parse(source, None)
        .with_context(|| format!("tree-sitter n'a pas parsé {language_name}"))?;
    let lines = source.lines().collect::<Vec<_>>();
    let mut symbols = Vec::new();
    collect_symbols(
        tree.root_node(),
        source.as_bytes(),
        &lines,
        &[],
        &mut symbols,
    );
    dedupe_symbols(&mut symbols);

    let mut edges = Vec::new();
    collect_edges(tree.root_node(), source.as_bytes(), &symbols, &mut edges);
    dedupe_edges(&mut edges);
    Ok((symbols, edges))
}

fn language_for(name: &str) -> Result<Language> {
    let language = match name {
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        _ => bail!("langage non supporté: {name}"),
    };
    Ok(language)
}

fn symbol_kind(node_kind: &str) -> Option<&'static str> {
    match node_kind {
        "function_definition"
        | "function_declaration"
        | "generator_function_declaration"
        | "function_item" => Some("function"),
        "method_definition" | "method_declaration" => Some("method"),
        "class_definition" | "class_declaration" | "struct_item" => Some("class"),
        "interface_declaration"
        | "type_alias_declaration"
        | "type_declaration"
        | "enum_declaration"
        | "enum_item"
        | "trait_item"
        | "trait_declaration"
        | "type_spec" => Some("type"),
        "variable_declarator" | "const_item" | "static_item" => Some("variable"),
        _ => None,
    }
}

fn is_container(node_kind: &str) -> bool {
    matches!(
        node_kind,
        "class_definition"
            | "class_declaration"
            | "struct_item"
            | "trait_item"
            | "trait_declaration"
            | "interface_declaration"
    )
}

fn collect_symbols(
    node: Node<'_>,
    source: &[u8],
    lines: &[&str],
    parents: &[String],
    output: &mut Vec<Symbol>,
) {
    let mut children_parents = parents.to_vec();
    if let Some(mut kind) = symbol_kind(node.kind())
        && let Some(name_node) = node.child_by_field_name("name")
    {
        if node.kind() == "variable_declarator"
            && node
                .child_by_field_name("value")
                .is_some_and(|value| value.kind() == "arrow_function")
        {
            kind = "function";
        }
        if node.kind() == "type_spec"
            && node
                .child_by_field_name("type")
                .is_some_and(|value| value.kind() == "struct_type")
        {
            kind = "class";
        }
        let name = node_text(name_node, source)
            .trim()
            .trim_start_matches('$')
            .to_owned();
        if !name.is_empty() {
            if kind == "function" && !parents.is_empty() {
                kind = "method";
            }
            let start = node.start_position().row + 1;
            let end = node.end_position().row + 1;
            let qualname = parents
                .iter()
                .chain(std::iter::once(&name))
                .cloned()
                .collect::<Vec<_>>()
                .join(".");
            output.push(Symbol {
                name: name.clone(),
                qualname,
                kind: kind.to_owned(),
                start,
                end,
                sig: lines
                    .get(start.saturating_sub(1))
                    .copied()
                    .unwrap_or_default()
                    .trim()
                    .chars()
                    .take(300)
                    .collect(),
                snippet: line_slice(lines, start, end, 2_000),
            });
            if is_container(node.kind()) {
                children_parents.push(name);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols(child, source, lines, &children_parents, output);
    }
}

fn collect_edges(node: Node<'_>, source: &[u8], symbols: &[Symbol], output: &mut Vec<Edge>) {
    let line = node.start_position().row + 1;
    if is_call(node.kind()) {
        let target = node
            .child_by_field_name("function")
            .or_else(|| node.child_by_field_name("name"));
        let raw = target
            .map(|child| node_text(child, source).to_owned())
            .unwrap_or_else(|| {
                node_text(node, source)
                    .split('(')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            });
        if let Some(name) = last_identifier(&raw)
            && !is_keyword(&name)
            && !is_declaration(symbols, line, &name)
        {
            output.push(Edge {
                src_name: owner(symbols, line),
                dst_name: name,
                kind: "call".to_owned(),
                line,
            });
        }
    } else if is_import(node.kind()) {
        let raw = node_text(node, source);
        let quoted = Regex::new(r#"['"]([^'"]+)['"]"#)
            .expect("valid regex")
            .captures_iter(raw)
            .filter_map(|capture| capture.get(1))
            .filter_map(|value| {
                value
                    .as_str()
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        let names = if quoted.is_empty() {
            last_identifier(raw).into_iter().collect()
        } else {
            quoted
        };
        for name in names {
            output.push(Edge {
                src_name: owner(symbols, line),
                dst_name: name,
                kind: "import".to_owned(),
                line,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_edges(child, source, symbols, output);
    }
}

fn is_call(kind: &str) -> bool {
    matches!(
        kind,
        "call"
            | "call_expression"
            | "function_call_expression"
            | "member_call_expression"
            | "scoped_call_expression"
            | "object_creation_expression"
            | "macro_invocation"
    )
}

fn is_import(kind: &str) -> bool {
    matches!(
        kind,
        "import_statement"
            | "import_from_statement"
            | "import_declaration"
            | "use_declaration"
            | "namespace_use_declaration"
            | "include_expression"
    )
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or_default()
}

fn line_slice(lines: &[&str], start: usize, end: usize, limit: usize) -> String {
    let mut value = lines[start.saturating_sub(1)..end.min(lines.len())].join("\n");
    if value.len() > limit {
        let mut boundary = limit;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
    }
    value.trim_end().to_owned()
}

fn last_identifier(value: &str) -> Option<String> {
    Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*")
        .expect("valid regex")
        .find_iter(value)
        .last()
        .map(|item| item.as_str().trim_start_matches('$').to_owned())
}

fn owner(symbols: &[Symbol], line: usize) -> Option<String> {
    symbols
        .iter()
        .filter(|symbol| symbol.start <= line && line <= symbol.end)
        .min_by_key(|symbol| symbol.end.saturating_sub(symbol.start))
        .map(|symbol| symbol.name.clone())
}

fn is_declaration(symbols: &[Symbol], line: usize, name: &str) -> bool {
    symbols
        .iter()
        .any(|symbol| symbol.start == line && symbol.name == name)
}

fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "switch"
            | "catch"
            | "function"
            | "func"
            | "fn"
            | "match"
            | "loop"
            | "isset"
            | "empty"
    )
}

fn dedupe_symbols(symbols: &mut Vec<Symbol>) {
    let priorities = HashMap::from([
        ("method", 5),
        ("class", 4),
        ("function", 3),
        ("type", 2),
        ("variable", 1),
    ]);
    symbols.sort_by(|left, right| {
        (left.start, &left.name, &left.kind).cmp(&(right.start, &right.name, &right.kind))
    });
    let mut best: HashMap<(usize, String), Symbol> = HashMap::new();
    for symbol in symbols.drain(..) {
        let key = (symbol.start, symbol.name.clone());
        match best.get(&key) {
            Some(existing)
                if priorities.get(existing.kind.as_str()).unwrap_or(&0)
                    >= priorities.get(symbol.kind.as_str()).unwrap_or(&0) => {}
            _ => {
                best.insert(key, symbol);
            }
        }
    }
    *symbols = best.into_values().collect();
    symbols.sort_by(|left, right| {
        (left.start, &left.name, &left.kind).cmp(&(right.start, &right.name, &right.kind))
    });
}

fn dedupe_edges(edges: &mut Vec<Edge>) {
    let mut seen = HashSet::new();
    edges.retain(|edge| {
        seen.insert((
            edge.line,
            edge.src_name.clone(),
            edge.dst_name.clone(),
            edge.kind.clone(),
        ))
    });
    edges.sort_by(|left, right| {
        (&left.line, &left.kind, &left.dst_name).cmp(&(&right.line, &right.kind, &right.dst_name))
    });
}

#[cfg(test)]
mod tests {
    use super::parse_source;

    #[test]
    fn parses_python_class_method_and_call() {
        let source = "class AuthService:\n    def login(self, user):\n        return save(user)\n";
        let (symbols, edges) = parse_source(source, "python").unwrap();
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.qualname == "AuthService.login")
        );
        assert!(
            edges
                .iter()
                .any(|edge| edge.dst_name == "save" && edge.src_name.as_deref() == Some("login"))
        );
    }

    #[test]
    fn parses_all_supported_grammars() {
        let fixtures = [
            ("javascript", "function login() { return save(); }"),
            (
                "typescript",
                "export function login(): string { return save(); }",
            ),
            ("tsx", "export function Login() { return <main />; }"),
            ("go", "package main\nfunc login() string { return save() }"),
            ("rust", "fn login() -> String { save() }"),
            ("php", "<?php function login(): string { return save(); }"),
        ];
        for (language, source) in fixtures {
            let (symbols, _) = parse_source(source, language).unwrap();
            assert!(
                !symbols.is_empty(),
                "{language} should produce at least one symbol"
            );
        }
    }

    #[test]
    fn classifies_typescript_arrows_and_go_structs() {
        let (typescript, _) = parse_source(
            "export const login = async (user: User): Promise<User> => save(user);",
            "typescript",
        )
        .unwrap();
        assert!(
            typescript
                .iter()
                .any(|symbol| symbol.name == "login" && symbol.kind == "function")
        );
        let (go, _) = parse_source(
            "package auth\ntype AuthService struct { Name string }",
            "go",
        )
        .unwrap();
        assert!(
            go.iter()
                .any(|symbol| symbol.name == "AuthService" && symbol.kind == "class")
        );
    }
}
