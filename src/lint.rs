// The consumer contract checks (architect-v3.md, "Contracts"), language-aware
// through tree-sitter rather than a toolchain: a consumer must match every
// constructor of an API exhaustively, with no wildcard, and must not reach
// into a T through the generated wire helpers. The same holds for each of
// an API's elements: an algebra over one must be total in its variants.
// Heuristic where the language has no types to ask: a match "is over an API"
// when an arm names one of its constructors qualified by the API's sum type
// (or a variant by its element).

use crate::idl::Idl;
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

/// Wildcard arms in matches over `idl`'s sum type or any of its elements,
/// in one file of `lang`.
pub fn wildcard_arms(lang: &str, language: Language, src: &[u8], idl: &Idl) -> Vec<Finding> {
    let mut out = Vec::new();
    for (en, variants, element) in idl.sums() {
        out.extend(wildcards_over(lang, language.clone(), src, &en, &variants, element));
    }
    out.sort_by_key(|f| f.line);
    out
}

fn wildcards_over(lang: &str, language: Language, src: &[u8], en: &str, variants: &[&str], element: bool) -> Vec<Finding> {
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else { return Vec::new() };
    let mut out = Vec::new();
    let mut flag = |n: Node, what: &str| {
        out.push(Finding {
            line: n.start_position().row + 1,
            message: if element {
                format!("{what} in a match over {en}: an algebra must be total in {en}")
            } else {
                format!("{what} in a match over {en}: adding a constructor must break the consumer")
            },
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
                    && variants.iter().any(|c| text(*k, src).trim_start().starts_with(&format!("case \"{c}\"")))
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
                .any(|e| variants.iter().any(|c| text(*e, src).contains(&format!("{en}.{c}"))));
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

const TUNNEL: &str = "serialized structure crosses the API as a string: declare it as constructors, Ts or an element";

/// Serialized structure handed to an API as a string, or a T's text parsed
/// back into structure (architect-v3.md, "No tunnels"): a `::new(` whose
/// argument serializes (`serde_json::to_string`, `json!`), a `serde_json::
/// from_str`/`from_value` of a `.to_string()`; `JSON.stringify` inside a
/// `new T(` and `JSON.parse(String(t))` in TypeScript; `encodeToString` into
/// a constructor and `decodeFromString(t.toString())` in Kotlin. Best effort,
/// like `wildcard_arms`.
pub fn serialized_boundary(lang: &str, language: Language, src: &[u8]) -> Vec<Finding> {
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else { return Vec::new() };
    let (ctor_kind, into, parse_fn, of_text): (&[&str], Regex, Regex, Regex) = match lang {
        "rust" => (
            &["call_expression"],
            Regex::new(r"serde_json::to_string(_pretty)?\(|\bjson!\s*[(\[{]|to_string_pretty\(").unwrap(),
            Regex::new(r"(serde_json::|^)from_(str|value)$").unwrap(),
            Regex::new(r"\.to_string\(\)|\bformat!\s*\(").unwrap(),
        ),
        "typescript" => (
            &["new_expression", "call_expression"],
            Regex::new(r"\bJSON\.stringify\(").unwrap(),
            Regex::new(r"^JSON\.parse$").unwrap(),
            Regex::new(r"\bString\(|\.toString\(\)").unwrap(),
        ),
        "kotlin" => (
            &["call_expression"],
            Regex::new(r"\bencodeToString\(").unwrap(),
            Regex::new(r"decodeFromString(<[^(]*>)?$").unwrap(),
            Regex::new(r"\.toString\(\)").unwrap(),
        ),
        _ => return Vec::new(),
    };
    let is_ctor = |f: &str| match lang {
        "rust" => f.ends_with("::new"),
        "typescript" => f.chars().next().is_some_and(|c| c.is_ascii_uppercase()) || f.contains(".new"),
        _ => f.chars().next().is_some_and(|c| c.is_ascii_uppercase()),
    };
    let mut out = Vec::new();
    walk(tree.root_node(), &mut |n| {
        if !ctor_kind.contains(&n.kind()) {
            return;
        }
        let Some(func) = n.child(0).or_else(|| n.child_by_field_name("function")) else { return };
        let (func, args) = if lang == "typescript" && n.kind() == "new_expression" {
            (n.child_by_field_name("constructor").unwrap_or(func), n.child_by_field_name("arguments"))
        } else {
            (func, n.child_by_field_name("arguments").or_else(|| n.child(n.child_count().saturating_sub(1))))
        };
        let Some(args) = args else { return };
        let (f, a) = (text(func, src).trim(), text(args, src));
        let line = n.start_position().row + 1;
        if (is_ctor(f) && into.is_match(a)) || (parse_fn.is_match(f) && of_text.is_match(a)) {
            out.push(Finding { line, message: TUNNEL.to_string() });
        }
    });
    if lang == "kotlin" {
        // The grammar reads `decodeFromString<T>(x)` as comparisons, not a
        // call: match the text instead.
        let re = Regex::new(r"decodeFromString(<[^(]*>)?\([^)]*\.toString\(\)").unwrap();
        for (i, l) in String::from_utf8_lossy(src).lines().enumerate() {
            if re.is_match(l) {
                out.push(Finding { line: i + 1, message: TUNNEL.to_string() });
            }
        }
    }
    out.sort_by_key(|f| f.line);
    out.dedup();
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

/// Calls of a token's `mint` (`ProductRef::mint(`, `ProductRef.mint(`) in a
/// file that may not mint it: evidence is the generator's to issue. By
/// name, like the other lints.
pub fn mint_calls(src: &str, tokens: &[&str]) -> Vec<Finding> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let names = tokens.iter().map(|t| regex::escape(t)).collect::<Vec<_>>().join("|");
    let re = Regex::new(&format!(r"\b({names})(::|\.)mint\(")).unwrap();
    src.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            re.captures(l).map(|c| Finding {
                line: i + 1,
                message: format!("mints the evidence token {}; only the generator of its api may", &c[1]),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idl::idl_from_json;

    const IDL: &str = r#"{"api":"ab","types":[{"name":"Id","kind":{"repr":"string"}}],"ctors":[{"name":"Created","args":["Id"]},{"name":"Tick"}]}"#;

    #[test]
    fn serialized_structure_at_the_boundary_is_a_tunnel() {
        let rs = "fn f(r: &Report) -> Payload {\n    let p = Payload::new(serde_json::to_string(r).unwrap());\n    let q = Name::new(\"x\".to_string());\n    \
                  let r2: Report = serde_json::from_str(&p.to_string()).unwrap();\n    let ok: Report = serde_json::from_str(raw).unwrap();\n    p\n}\n";
        let lines: Vec<usize> = serialized_boundary("rust", tree_sitter_rust::LANGUAGE.into(), rs.as_bytes()).iter().map(|f| f.line).collect();
        assert_eq!(lines, [2, 4]);
        let ts = "const p = new Payload(JSON.stringify(report));\nconst n = new Name(\"x\");\nconst r = JSON.parse(String(p));\nconst s = JSON.parse(raw);\n";
        let lines: Vec<usize> = serialized_boundary("typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), ts.as_bytes()).iter().map(|f| f.line).collect();
        assert_eq!(lines, [1, 3]);
        let kt = "val p = Payload(Json.encodeToString(report))\nval n = Name(\"x\")\nval r = Json.decodeFromString<Report>(p.toString())\nval s = Json.decodeFromString<Report>(raw)\n";
        let lines: Vec<usize> = serialized_boundary("kotlin", tree_sitter_kotlin_ng::LANGUAGE.into(), kt.as_bytes()).iter().map(|f| f.line).collect();
        assert_eq!(lines, [1, 3]);
    }

    fn run(lang: &str, l: Language, src: &str) -> Vec<Finding> {
        wildcard_arms(lang, l, src.as_bytes(), &idl_from_json(IDL).unwrap())
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
    fn element_algebras_must_be_total() {
        let idl = idl_from_json(
            r#"{"api":"db","elements":[{"name":"Row","variants":[{"name":"Meal","fields":[{"repr":"f64"}]},{"name":"Note","fields":[{"repr":"string"}]}]}],
                "containers":[{"name":"Rows","kind":{"of":["Row","f","Seq"]}}],"ctors":[{"name":"Ok","args":["Rows"]}]}"#,
        )
        .unwrap();
        let rs: Language = tree_sitter_rust::LANGUAGE.into();
        let bad = "fn k(r: &Row) -> f64 { match r { Row::Meal(k) => *k, _ => 0.0 } }";
        let found = wildcard_arms("rust", rs.clone(), bad.as_bytes(), &idl);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("an algebra must be total in Row"), "{:?}", found);
        let total = "fn k(r: &Row) -> f64 { match r { Row::Meal(k) => *k, Row::Note(_) => 0.0 } }";
        assert!(wildcard_arms("rust", rs, total.as_bytes(), &idl).is_empty());
    }

    #[test]
    fn mint_outside_the_generator_is_found() {
        let found = mint_calls("let p = ProductRef::mint(1);\nlet q = Funds.mint(2n);\nlet r = Other::mint(3);", &["ProductRef", "Funds"]);
        assert_eq!(found.iter().map(|f| f.line).collect::<Vec<_>>(), [1, 2]);
        assert!(found[0].message.contains("ProductRef"));
        assert!(mint_calls("ProductRef::mint(1)", &[]).is_empty());
    }

    #[test]
    fn wire_leaks_are_found() {
        assert_eq!(wire_leaks("let a = 1;\nlet v = id.to_value();").len(), 1);
        assert!(wire_leaks("serde_json::to_value(x)").is_empty());
    }
}
