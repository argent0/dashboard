// The API IDL (architect-v3.md): one `<name>.api.kdl` per API, the source of
// truth for a flat sum of products. Bindings are generated from it into
// pinned files, so the opacity of every `T` holds by construction.
//
//   api "ab"
//   type "UserId"  repr="string"     // string | bytes | i64 | f64 | bool
//   type "Parent"  ref="xy.Event"    // a type declared by API xy
//   type "Name"    repr="string" display=#true
//   constructor "Created" "UserId" "Payload"
//   constructor "Tick"

use kdl::{KdlDocument, KdlNode, KdlValue};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repr {
    String,
    Bytes,
    I64,
    F64,
    Bool,
}

impl Repr {
    fn parse(s: &str) -> Option<Repr> {
        Some(match s {
            "string" => Repr::String,
            "bytes" => Repr::Bytes,
            "i64" => Repr::I64,
            "f64" => Repr::F64,
            "bool" => Repr::Bool,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Repr(Repr),
    /// `(api, type)` of another API.
    Ref(String, String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    pub name: String,
    pub kind: Kind,
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ctor {
    pub name: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Idl {
    pub api: String,
    pub types: Vec<TypeDecl>,
    pub ctors: Vec<Ctor>,
}

fn ident_ok(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(f) if f.is_ascii_uppercase())
        && c.all(|c| c.is_ascii_alphanumeric())
}

fn string_args(node: &KdlNode) -> Result<Vec<String>, String> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| {
            e.value()
                .as_string()
                .map(str::to_string)
                .ok_or_else(|| format!("{}: arguments must be strings", node.name().value()))
        })
        .collect()
}

fn prop<'a>(node: &'a KdlNode, key: &str) -> Option<&'a KdlValue> {
    node.entries().iter().find(|e| e.name().map(|n| n.value()) == Some(key)).map(|e| e.value())
}

impl Idl {
    pub fn parse(text: &str) -> Result<Idl, String> {
        let doc: KdlDocument = text.parse().map_err(|e: kdl::KdlError| e.to_string())?;
        let (mut api, mut types, mut ctors) = (None, Vec::new(), Vec::new());
        for node in doc.nodes() {
            let args = string_args(node)?;
            match node.name().value() {
                "api" => match args.as_slice() {
                    [n] if api.is_none() => api = Some(n.clone()),
                    _ => return Err("api: exactly one `api \"name\"` line".to_string()),
                },
                "type" => {
                    let [name] = args.as_slice() else {
                        return Err("type: expected one name".to_string());
                    };
                    let repr = prop(node, "repr").and_then(KdlValue::as_string);
                    let rf = prop(node, "ref").and_then(KdlValue::as_string);
                    let kind = match (repr, rf) {
                        (Some(r), None) => Kind::Repr(
                            Repr::parse(r).ok_or_else(|| format!("type {name}: unknown repr {r:?}"))?,
                        ),
                        (None, Some(r)) => match r.split_once('.') {
                            Some((a, t)) if !a.is_empty() && ident_ok(t) => {
                                Kind::Ref(a.to_string(), t.to_string())
                            }
                            _ => return Err(format!("type {name}: ref {r:?} must be api.Type")),
                        },
                        _ => return Err(format!("type {name}: exactly one of repr= or ref=")),
                    };
                    let display = match prop(node, "display") {
                        None => false,
                        Some(v) => v.as_bool().ok_or(format!("type {name}: display must be #true or #false"))?,
                    };
                    if display && matches!(kind, Kind::Ref(..)) {
                        return Err(format!("type {name}: display is only for repr types"));
                    }
                    types.push(TypeDecl { name: name.clone(), kind, display });
                }
                "constructor" => {
                    let Some((name, rest)) = args.split_first() else {
                        return Err("constructor: expected a name".to_string());
                    };
                    ctors.push(Ctor { name: name.clone(), args: rest.to_vec() });
                }
                other => return Err(format!("unknown node {other:?}")),
            }
        }
        let idl = Idl { api: api.ok_or("missing `api \"name\"`")?, types, ctors };
        idl.validate()?;
        Ok(idl)
    }

    fn validate(&self) -> Result<(), String> {
        let mut names = BTreeSet::new();
        for t in &self.types {
            if !ident_ok(&t.name) || !names.insert(t.name.as_str()) {
                return Err(format!("type {:?}: not a CamelCase identifier, or duplicate", t.name));
            }
        }
        let mut cnames = BTreeSet::new();
        let mut used = BTreeSet::new();
        for c in &self.ctors {
            if !ident_ok(&c.name) || !cnames.insert(c.name.as_str()) {
                return Err(format!("constructor {:?}: not a CamelCase identifier, or duplicate", c.name));
            }
            for a in &c.args {
                if !names.contains(a.as_str()) {
                    return Err(format!("constructor {}: unknown type {a:?}", c.name));
                }
                used.insert(a.as_str());
            }
        }
        if let Some(t) = self.types.iter().find(|t| !used.contains(t.name.as_str())) {
            return Err(format!("type {}: declared but never used", t.name));
        }
        if self.ctors.is_empty() {
            return Err("no constructors".to_string());
        }
        Ok(())
    }

    /// Number of constructors (the `max_constructors` measure).
    pub fn constructors(&self) -> u64 {
        self.ctors.len() as u64
    }

    /// Argument slots across all constructors (the `max_types` measure).
    pub fn slots(&self) -> u64 {
        self.ctors.iter().map(|c| c.args.len() as u64).sum()
    }

    pub fn type_decl(&self, name: &str) -> Option<&TypeDecl> {
        self.types.iter().find(|t| t.name == name)
    }

    /// `(api, type)` of every `ref`.
    pub fn refs(&self) -> Vec<(&str, &str)> {
        self.types
            .iter()
            .filter_map(|t| match &t.kind {
                Kind::Ref(a, ty) => Some((a.as_str(), ty.as_str())),
                _ => None,
            })
            .collect()
    }

    /// Generated Rust. `wire`: also emit the JSON encode/decode (needs
    /// `serde_json`), for a pair that crosses languages.
    pub fn rust(&self, wire: bool) -> String {
        let en = pascal(&self.api);
        let mut o = format!(
            "// @generated by `dashboard arch gen` from the {} IDL. Do not edit.\n\
             // Every T is opaque: no trait impls, a private field, no accessors.\n\
             #![allow(dead_code)]\n",
            self.api
        );
        if wire {
            o.push_str("use serde_json::{json, Value};\n");
        }
        o.push('\n');
        for t in &self.types {
            let (inner, from) = match &t.kind {
                Kind::Repr(r) => (
                    match r {
                        Repr::String => "String",
                        Repr::Bytes => "Vec<u8>",
                        Repr::I64 => "i64",
                        Repr::F64 => "f64",
                        Repr::Bool => "bool",
                    }
                    .to_string(),
                    true,
                ),
                Kind::Ref(a, ty) => (format!("super::{}::{ty}", a.replace('-', "_")), false),
            };
            o.push_str(&format!("pub struct {}({inner});\n", t.name));
            o.push_str(&format!("impl {} {{\n    pub fn new(v: {inner}) -> Self {{ Self(v) }}\n", t.name));
            if wire {
                let (enc, dec) = match &t.kind {
                    Kind::Repr(Repr::Bytes) => (
                        "Value::String(self.0.iter().map(|b| format!(\"{b:02x}\")).collect())".to_string(),
                        "let s = v.as_str().ok_or(\"expected hex string\")?;\n        \
                         if s.len() % 2 != 0 { return Err(\"odd hex length\".into()); }\n        \
                         let mut b = Vec::new();\n        \
                         for i in (0..s.len()).step_by(2) { b.push(u8::from_str_radix(s.get(i..i + 2).ok_or(\"bad hex\")?, 16).map_err(|e| e.to_string())?); }\n        \
                         Ok(Self(b))".to_string(),
                    ),
                    Kind::Repr(Repr::String) => (
                        "json!(self.0)".to_string(),
                        "Ok(Self(v.as_str().ok_or(\"expected string\")?.to_string()))".to_string(),
                    ),
                    Kind::Repr(Repr::I64) => (
                        "json!(self.0)".to_string(),
                        "Ok(Self(v.as_i64().ok_or(\"expected i64\")?))".to_string(),
                    ),
                    Kind::Repr(Repr::F64) => (
                        "json!(self.0)".to_string(),
                        "Ok(Self(v.as_f64().ok_or(\"expected f64\")?))".to_string(),
                    ),
                    Kind::Repr(Repr::Bool) => (
                        "json!(self.0)".to_string(),
                        "Ok(Self(v.as_bool().ok_or(\"expected bool\")?))".to_string(),
                    ),
                    Kind::Ref(..) => ("self.0.to_value()".to_string(), "Ok(Self(".to_string()),
                };
                o.push_str(&format!("    pub fn to_value(&self) -> Value {{ {enc} }}\n"));
                if let Kind::Ref(a, ty) = &t.kind {
                    o.push_str(&format!(
                        "    pub fn from_value(v: &Value) -> Result<Self, String> {{ Ok(Self(super::{}::{ty}::from_value(v)?)) }}\n",
                        a.replace('-', "_")
                    ));
                } else {
                    o.push_str(&format!("    pub fn from_value(v: &Value) -> Result<Self, String> {{\n        {dec}\n    }}\n"));
                }
            }
            o.push_str("}\n");
            if t.display && from {
                let body = match t.kind {
                    Kind::Repr(Repr::Bytes) => "for b in &self.0 { write!(f, \"{b:02x}\")?; } Ok(())",
                    _ => "write!(f, \"{}\", self.0)",
                };
                o.push_str(&format!(
                    "impl std::fmt::Display for {} {{\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{ {body} }}\n}}\n",
                    t.name
                ));
            }
            o.push('\n');
        }
        o.push_str(&format!("pub enum {en} {{\n"));
        for c in &self.ctors {
            if c.args.is_empty() {
                o.push_str(&format!("    {},\n", c.name));
            } else {
                o.push_str(&format!("    {}({}),\n", c.name, c.args.join(", ")));
            }
        }
        o.push_str("}\n");
        if wire {
            o.push_str(&format!("\nimpl {en} {{\n    pub fn to_json(&self) -> String {{\n        match self {{\n"));
            for c in &self.ctors {
                let vars: Vec<String> = (0..c.args.len()).map(|i| format!("a{i}")).collect();
                let pat = if vars.is_empty() { String::new() } else { format!("({})", vars.join(", ")) };
                let vals: Vec<String> = vars.iter().map(|v| format!("{v}.to_value()")).collect();
                o.push_str(&format!(
                    "            {en}::{}{pat} => json!({{\"c\": \"{}\", \"a\": [{}]}}).to_string(),\n",
                    c.name, c.name, vals.join(", ")
                ));
            }
            o.push_str(&format!(
                "        }}\n    }}\n\n    pub fn from_json(s: &str) -> Result<Self, String> {{\n        \
                 let v: Value = serde_json::from_str(s).map_err(|e| e.to_string())?;\n        \
                 let args = v[\"a\"].as_array().ok_or(\"missing a\")?;\n        \
                 match v[\"c\"].as_str().ok_or(\"missing c\")? {{\n"
            ));
            for c in &self.ctors {
                let n = c.args.len();
                let got: Vec<String> = c
                    .args
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("{t}::from_value(&args[{i}])?"))
                    .collect();
                let build = if n == 0 { format!("{en}::{}", c.name) } else { format!("{en}::{}({})", c.name, got.join(", ")) };
                o.push_str(&format!(
                    "            \"{}\" => {{\n                if args.len() != {n} {{ return Err(\"wrong argument count for {}\".into()); }}\n                Ok({build})\n            }}\n",
                    c.name, c.name
                ));
            }
            o.push_str("            other => Err(format!(\"unknown constructor {other:?}\")),\n        }\n    }\n}\n");
        }
        o
    }
}

/// `user-events` -> `UserEvents`.
pub fn pascal(api: &str) -> String {
    api.split(['-', '_'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AB: &str = r#"
        api "ab"
        type "UserId" repr="string" display=#true
        type "Payload" repr="bytes"
        constructor "Created" "UserId" "Payload"
        constructor "Deleted" "UserId"
        constructor "Tick"
    "#;

    #[test]
    fn parses_and_measures() {
        let idl = Idl::parse(AB).unwrap();
        assert_eq!((idl.constructors(), idl.slots()), (3, 3));
        assert!(idl.type_decl("UserId").unwrap().display);
    }

    #[test]
    fn rejects_bad_idl() {
        for (bad, want) in [
            ("api \"a\"\nconstructor \"X\" \"Nope\"", "unknown type"),
            ("api \"a\"\ntype \"T\" repr=\"string\"\nconstructor \"X\"", "never used"),
            ("api \"a\"\ntype \"T\" repr=\"float\"\nconstructor \"X\" \"T\"", "unknown repr"),
            ("api \"a\"\ntype \"T\" ref=\"nodot\"\nconstructor \"X\" \"T\"", "api.Type"),
            ("api \"a\"", "no constructors"),
        ] {
            let e = Idl::parse(bad).unwrap_err();
            assert!(e.contains(want), "{bad}: {e}");
        }
    }

    #[test]
    fn rust_is_opaque_and_wire_is_optional() {
        let idl = Idl::parse(AB).unwrap();
        let plain = idl.rust(false);
        assert!(plain.contains("pub struct UserId(String);"));
        assert!(plain.contains("pub enum Ab {\n    Created(UserId, Payload),"));
        assert!(!plain.contains("derive") && !plain.contains("serde_json"));
        assert!(plain.contains("impl std::fmt::Display for UserId"));
        assert!(!plain.contains("Display for Payload"));
        let wire = idl.rust(true);
        assert!(wire.contains("pub fn to_json") && wire.contains("pub fn from_json"));
    }
}
