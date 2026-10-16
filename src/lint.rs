// The consumer contract checks (architect-v3.md, "Contracts"), language-aware
// through tree-sitter rather than a toolchain: a consumer must match every
// constructor of an API exhaustively, with no wildcard, and must not reach
// into a T through the generated wire helpers. Heuristic where the language
// has no types to ask: a match "is over an API" when an arm names one of its
// constructors qualified by the API's sum type.

use crate::idl::{Idl, pascal};
use regex::Regex;
use tree_sitter::{Language, Node, Parser};

#[derive(Debug, PartialEq)]
pub struct Finding {
    pub line: usize,
    pub message: String,
}

fn walk<'a>(node: Node<'a>, f: &mut impl FnMut(Node<'a>)) {
    f(node);
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        walk(ch, f);
    }
}

fn text<'a>(n: Node, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

/// A Rust arm pattern that matches anything: `_`, or a lone binding.
fn rust_catch_all(pat: &str) -> bool {
    let re = Regex::new(r"^(_|[a-z_][A-Za-z0-9_]*)$").unwrap();
    pat.split('|').any(|p| re.is_match(p.trim()))
}

/// Wildcard arms in matches over `idl`'s sum type, in one file of `lang`.
pub fn wildcard_arms(lang: &str, language: Language, src: &[u8], idl: &Idl) -> Vec<Finding> {
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else { return Vec::new() };
    let en = pascal(&idl.api);
    let mut out = Vec::new();
    let mut flag = |n: Node, what: &str| {
        out.push(Finding {
            line: n.start_position().row + 1,
            message: format!("{what} in a match over {en}: adding a constructor must break the consumer"),
        })
    };
    walk(tree.root_node(), &mut |n| match (lang, n.kind()) {
        ("rust", "match_block") => {
            let mut c = n.walk();
            let arms: Vec<Node> = n.children(&mut c).filter(|a| a.kind() == "match_arm").collect();
            let pats: Vec<(Node, &str)> = arms
                .iter()
                .filter_map(|a| {
                    let mut c = a.walk();
                    let p = a.children(&mut c).find(|x| x.kind() == "match_pattern");
                    p.map(|p| (*a, text(p, src)))
                })
                .collect();
            if pats.iter().any(|(_, p)| p.contains(&format!("{en}::"))) {
                for (a, p) in &pats {
                    if rust_catch_all(p) {
                        flag(*a, "wildcard arm");
                    }
                }
            }
        }
        ("typescript", "switch_body") => {
            let mut c = n.walk();
            let kids: Vec<Node> = n.children(&mut c).collect();
            let over_api = kids.iter().any(|k| {
                k.kind() == "switch_case"
                    && idl.ctors.iter().any(|c| text(*k, src).trim_start().starts_with(&format!("case \"{}\"", c.name)))
            });
            if over_api {
                for k in kids.iter().filter(|k| k.kind() == "switch_default") {
                    flag(*k, "default clause");
                }
            }
        }
        ("kotlin", "when_expression") => {
            let mut c = n.walk();
            let entries: Vec<Node> = n.children(&mut c).filter(|e| e.kind() == "when_entry").collect();
            let over_api = entries
                .iter()
                .any(|e| idl.ctors.iter().any(|c| text(*e, src).contains(&format!("{en}.{}", c.name))));
            if over_api {
                for e in entries.iter().filter(|e| text(**e, src).trim_start().starts_with("else")) {
                    flag(*e, "else branch");
                }
            }
        }
        _ => {}
    });
    out
}

/// Calls of the generated wire helpers that read a T's inner value.
pub fn wire_leaks(src: &str) -> Vec<Finding> {
    let re = Regex::new(r"\.(to_value|toValue)\(\)").unwrap();
    src.lines()
        .enumerate()
        .filter(|(_, l)| re.is_match(l))
        .map(|(i, _)| Finding {
            line: i + 1,
            message: "reads a T's inner value through the generated wire helper; only the generated file may".to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDL: &str = "api \"ab\"\ntype \"Id\" repr=\"string\"\nconstructor \"Created\" \"Id\"\nconstructor \"Tick\"\n";

    fn run(lang: &str, l: Language, src: &str) -> Vec<Finding> {
        wildcard_arms(lang, l, src.as_bytes(), &Idl::parse(IDL).unwrap())
    }

    #[test]
    fn rust_wildcards_over_the_api_only() {
        let rs: Language = tree_sitter_rust::LANGUAGE.into();
        let bad = "fn f(x: Ab) { match x { Ab::Created(a) => 1, _ => 3 }; }";
        assert_eq!(run("rust", rs.clone(), bad).len(), 1);
        let binding = "fn f(x: Ab) { match x { Ab::Tick => 1, other => 3 }; }";
        assert_eq!(run("rust", rs.clone(), binding).len(), 1);
        let exhaustive = "fn f(x: Ab) { match x { Ab::Created(_) => 1, Ab::Tick => 2 }; }";
        assert!(run("rust", rs.clone(), exhaustive).is_empty());
        let unrelated = "fn f(x: u8) { match x { 1 => 1, _ => 3 }; }";
        assert!(run("rust", rs, unrelated).is_empty());
    }

    #[test]
    fn typescript_default_and_kotlin_else() {
        let ts: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let bad = "function f(x: Ab) { switch (x.kind) { case \"Created\": return 1; default: return 3; } }";
        assert_eq!(run("typescript", ts.clone(), bad).len(), 1);
        let other = "function f(x: number) { switch (x) { case 1: return 1; default: return 3; } }";
        assert!(run("typescript", ts, other).is_empty());
        let kt: Language = tree_sitter_kotlin_ng::LANGUAGE.into();
        let bad = "fun f(x: Ab): Int = when (x) { is Ab.Created -> 1; else -> 3 }";
        assert_eq!(run("kotlin", kt.clone(), bad).len(), 1);
        let good = "fun f(x: Ab): Int = when (x) { is Ab.Created -> 1; Ab.Tick -> 2 }";
        assert!(run("kotlin", kt, good).is_empty());
    }

    #[test]
    fn wire_leaks_are_found() {
        assert_eq!(wire_leaks("let a = 1;\nlet v = id.to_value();").len(), 1);
        assert!(wire_leaks("serde_json::to_value(x)").is_empty());
    }
}
