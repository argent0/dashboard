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
//
// Elements and containers (architect-v3.md, "Elements and containers"): an
// `element` is a readable flat sum of products over raw reprs, reachable
// only through a container; a container is `f a` with the operations of its
// classes and nothing else. The API owning a container (one per class
// module) declares it generic, with its classes; another class module adds
// an `instance`; a data API instantiates it with its element:
//
//   container "Seq" classes="Foldable"               // foldable-x: Seq a
//   instance "Functor" for="foldable-x.Seq"          // functor-x
//   element "Row" { variant "Consumption" "f64" "string"; }
//   container "Rows" of="Row" ref="foldable-x.Seq"   // db-x: Seq Row
//
// The instance is hand-written (the API's `impl` files); the class traits
// and the law tests it must pass are generated.

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

/// A container class: the operations a consumer gets, and the laws an
/// instance is tested against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Foldable,
    Functor,
    Traversable,
    Monoid,
}

impl Class {
    fn parse(s: &str) -> Option<Class> {
        Some(match s {
            "Foldable" => Class::Foldable,
            "Functor" => Class::Functor,
            "Traversable" => Class::Traversable,
            "Monoid" => Class::Monoid,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Class::Foldable => "Foldable",
            Class::Functor => "Functor",
            Class::Traversable => "Traversable",
            Class::Monoid => "Monoid",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Repr>,
}

/// A readable flat sum of products over raw reprs.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub name: String,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContainerKind {
    /// `Seq a`, owned by this API, with its classes (Foldable among them).
    Generic(Vec<Class>),
    /// `(element, api, container)`: another API's generic over an element of this one.
    Of(String, String, String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub name: String,
    pub kind: ContainerKind,
}

/// A class added to a container another API owns.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub class: Class,
    pub api: String,
    pub container: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Idl {
    pub api: String,
    pub types: Vec<TypeDecl>,
    pub ctors: Vec<Ctor>,
    pub elements: Vec<Element>,
    pub containers: Vec<Container>,
    pub instances: Vec<Instance>,
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

/// `api.Type` -> `(api, Type)`.
fn api_type(s: &str) -> Option<(String, String)> {
    match s.split_once('.') {
        Some((a, t)) if !a.is_empty() && ident_ok(t) => Some((a.to_string(), t.to_string())),
        _ => None,
    }
}

fn prop<'a>(node: &'a KdlNode, key: &str) -> Option<&'a KdlValue> {
    node.entries().iter().find(|e| e.name().map(|n| n.value()) == Some(key)).map(|e| e.value())
}

impl Idl {
    pub fn parse(text: &str) -> Result<Idl, String> {
        let doc: KdlDocument = text.parse().map_err(|e: kdl::KdlError| e.to_string())?;
        let (mut api, mut types, mut ctors) = (None, Vec::new(), Vec::new());
        let (mut elements, mut containers, mut instances) = (Vec::new(), Vec::new(), Vec::new());
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
                        (None, Some(r)) => match api_type(r) {
                            Some((a, t)) => Kind::Ref(a, t),
                            None => return Err(format!("type {name}: ref {r:?} must be api.Type")),
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
                "element" => {
                    let [name] = args.as_slice() else {
                        return Err("element: expected one name".to_string());
                    };
                    let mut variants = Vec::new();
                    for v in node.children().map(|d| d.nodes()).unwrap_or_default() {
                        let vargs = string_args(v)?;
                        let (Some((vname, fields)), "variant") = (vargs.split_first(), v.name().value()) else {
                            return Err(format!("element {name}: expected `variant \"Name\" \"repr\"...` lines"));
                        };
                        let fields = fields
                            .iter()
                            .map(|f| Repr::parse(f).ok_or_else(|| format!("element {name}: variant {vname}: unknown repr {f:?}")))
                            .collect::<Result<_, _>>()?;
                        variants.push(Variant { name: vname.clone(), fields });
                    }
                    elements.push(Element { name: name.clone(), variants });
                }
                "container" => {
                    let [name] = args.as_slice() else {
                        return Err("container: expected one name".to_string());
                    };
                    let get = |k: &str| prop(node, k).and_then(KdlValue::as_string);
                    let kind = match (get("classes"), get("of"), get("ref")) {
                        (Some(cs), None, None) => ContainerKind::Generic(
                            cs.split(',')
                                .map(|c| Class::parse(c.trim()).ok_or_else(|| format!("container {name}: unknown class {c:?}")))
                                .collect::<Result<_, _>>()?,
                        ),
                        (None, Some(of), Some(r)) => {
                            let (a, t) = api_type(r).ok_or_else(|| format!("container {name}: ref {r:?} must be api.Type"))?;
                            ContainerKind::Of(of.to_string(), a, t)
                        }
                        _ => {
                            return Err(format!(
                                "container {name}: either classes= (a generic this api owns) or of= and ref= (another api's generic over an element)"
                            ))
                        }
                    };
                    containers.push(Container { name: name.clone(), kind });
                }
                "instance" => {
                    let [class] = args.as_slice() else {
                        return Err("instance: expected one class".to_string());
                    };
                    let class = Class::parse(class).ok_or_else(|| format!("instance: unknown class {class:?}"))?;
                    let (api, container) = prop(node, "for")
                        .and_then(KdlValue::as_string)
                        .and_then(api_type)
                        .ok_or_else(|| format!("instance {}: for= must be api.Container", class.name()))?;
                    instances.push(Instance { class, api, container });
                }
                other => return Err(format!("unknown node {other:?}")),
            }
        }
        let idl = Idl {
            api: api.ok_or("missing `api \"name\"`")?,
            types,
            ctors,
            elements,
            containers,
            instances,
        };
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
        for (what, n) in self
            .elements
            .iter()
            .map(|e| ("element", &e.name))
            .chain(self.containers.iter().map(|c| ("container", &c.name)))
        {
            if !ident_ok(n) || !names.insert(n.as_str()) {
                return Err(format!("{what} {n:?}: not a CamelCase identifier, or duplicate"));
            }
        }
        for e in &self.elements {
            let mut vs = BTreeSet::new();
            for v in &e.variants {
                if !ident_ok(&v.name) || !vs.insert(v.name.as_str()) {
                    return Err(format!("element {}: variant {:?}: not a CamelCase identifier, or duplicate", e.name, v.name));
                }
            }
            if e.variants.is_empty() {
                return Err(format!("element {}: no variants", e.name));
            }
            if !self.containers.iter().any(|c| matches!(&c.kind, ContainerKind::Of(of, ..) if *of == e.name)) {
                return Err(format!("element {}: no container holds it (an element is reachable only through one)", e.name));
            }
        }
        for c in &self.containers {
            match &c.kind {
                ContainerKind::Generic(classes) => {
                    if !classes.contains(&Class::Foldable) {
                        return Err(format!("container {}: a generic's classes must include Foldable, its primitive", c.name));
                    }
                    if !self.instances.is_empty() {
                        return Err(format!("container {}: an api owning a container adds no instances to another's", c.name));
                    }
                }
                ContainerKind::Of(of, ..) => {
                    if !self.elements.iter().any(|e| e.name == *of) {
                        return Err(format!("container {}: of={of:?} is not an element of this api", c.name));
                    }
                }
            }
        }
        if let Some(i) = self.instances.iter().find(|i| i.class == Class::Foldable) {
            return Err(format!("instance Foldable for {}.{}: Foldable is declared by the api owning the container", i.api, i.container));
        }
        if self.instances.iter().any(|i| i.api != self.instances[0].api) {
            return Err("instances: all for containers of one api".to_string());
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
                if self.elements.iter().any(|e| e.name == *a) {
                    return Err(format!("constructor {}: element {a} is reachable only through a container", c.name));
                }
                if self.containers.iter().any(|x| x.name == *a && matches!(x.kind, ContainerKind::Generic(_))) {
                    return Err(format!("constructor {}: {a} is generic; instantiate it with `container ... of=`", c.name));
                }
                used.insert(a.as_str());
            }
        }
        if let Some(t) = self.types.iter().find(|t| !used.contains(t.name.as_str())) {
            return Err(format!("type {}: declared but never used", t.name));
        }
        if let Some(c) = self
            .containers
            .iter()
            .find(|c| matches!(c.kind, ContainerKind::Of(..)) && !used.contains(c.name.as_str()))
        {
            return Err(format!("container {}: declared but never used", c.name));
        }
        if self.ctors.is_empty() && self.classes().is_empty() {
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

    /// Variants across all elements (the `max_elements` measure).
    pub fn element_variants(&self) -> u64 {
        self.elements.iter().map(|e| e.variants.len() as u64).sum()
    }

    /// Fields across all element variants (the `max_fields` measure).
    pub fn element_fields(&self) -> u64 {
        self.elements.iter().flat_map(|e| &e.variants).map(|v| v.fields.len() as u64).sum()
    }

    /// Every `(class, container)` this API declares: its generics' classes,
    /// then its instances. Each needs an instance in the API's `impl` files.
    pub fn classes(&self) -> Vec<(Class, &str)> {
        let mut out: Vec<(Class, &str)> = self
            .containers
            .iter()
            .flat_map(|c| match &c.kind {
                ContainerKind::Generic(cs) => cs.iter().map(|k| (*k, c.name.as_str())).collect(),
                ContainerKind::Of(..) => Vec::new(),
            })
            .collect();
        out.extend(self.instances.iter().map(|i| (i.class, i.container.as_str())));
        out
    }

    /// Whether this API declares elements, containers or instances, which
    /// only the Rust generator renders so far.
    pub fn has_containers(&self) -> bool {
        !self.elements.is_empty() || !self.containers.is_empty() || !self.instances.is_empty()
    }

    /// `(api, container)` of every container `ref` and instance `for`.
    pub fn container_refs(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = self
            .containers
            .iter()
            .filter_map(|c| match &c.kind {
                ContainerKind::Of(_, a, t) => Some((a.as_str(), t.as_str())),
                ContainerKind::Generic(_) => None,
            })
            .collect();
        out.extend(self.instances.iter().map(|i| (i.api.as_str(), i.container.as_str())));
        out
    }

    /// Whether this API owns a generic container `name`.
    pub fn owns_generic(&self, name: &str) -> bool {
        self.containers.iter().any(|c| c.name == name && matches!(c.kind, ContainerKind::Generic(_)))
    }

    /// Every sum a consumer must match totally: `(name, variants, element)`,
    /// the API's own sum first, then each element.
    pub fn sums(&self) -> Vec<(String, Vec<&str>, bool)> {
        let mut out = Vec::new();
        if !self.ctors.is_empty() {
            out.push((pascal(&self.api), self.ctors.iter().map(|c| c.name.as_str()).collect(), false));
        }
        for e in &self.elements {
            out.push((e.name.clone(), e.variants.iter().map(|v| v.name.as_str()).collect(), true));
        }
        out
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
    /// `serde_json`), for a pair that crosses languages. `impls`: the API's
    /// hand-written instance files, relative to the generated file's
    /// directory; each is mounted as a child module (`#[path]`), so it sees
    /// the class traits as `super::Trait`.
    pub fn rust(&self, wire: bool, impls: &[String]) -> String {
        let en = pascal(&self.api);
        let mut o = format!(
            "// @generated by `dashboard arch gen` from the {} IDL. Do not edit.\n\
             // Every T is opaque: no trait impls, a private field, no accessors.\n\
             #![allow({})]\n",
            self.api,
            if self.has_containers() { "dead_code, unused_imports" } else { "dead_code" }
        );
        if wire {
            o.push_str("use serde_json::{json, Value};\n");
        }
        o.push('\n');
        self.rust_classes(impls, &mut o);
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
        for e in &self.elements {
            o.push_str(&format!("#[derive(Debug, Clone, PartialEq)]\npub enum {} {{\n", e.name));
            for v in &e.variants {
                if v.fields.is_empty() {
                    o.push_str(&format!("    {},\n", v.name));
                } else {
                    let fs: Vec<&str> = v.fields.iter().map(|f| rust_repr(*f)).collect();
                    o.push_str(&format!("    {}({}),\n", v.name, fs.join(", ")));
                }
            }
            o.push_str("}\n\n");
        }
        for c in &self.containers {
            if let ContainerKind::Of(of, a, t) = &c.kind {
                o.push_str(&format!("pub type {} = super::{}::{t}<{of}>;\n\n", c.name, a.replace('-', "_")));
            }
        }
        if self.ctors.is_empty() {
            o.push_str(&self.rust_laws());
            return o;
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
        o.push_str(&self.rust_laws());
        o
    }

    /// The class traits this API declares, the imports its instances need,
    /// and the `impl` files mounted beneath it.
    fn rust_classes(&self, impls: &[String], o: &mut String) {
        if let Some(i) = self.instances.first() {
            let owner = i.api.replace('-', "_");
            let mut cs: Vec<&str> = self.instances.iter().map(|i| i.container.as_str()).collect();
            cs.sort();
            cs.dedup();
            o.push_str(&format!("use super::{owner}::{{Foldable, {}}};\n\n", cs.join(", ")));
        }
        let mut classes: Vec<Class> = self.classes().into_iter().map(|(c, _)| c).collect();
        classes.sort();
        classes.dedup();
        for c in classes {
            o.push_str(match c {
                Class::Foldable => FOLDABLE_TRAIT,
                Class::Functor => FUNCTOR_TRAIT,
                Class::Traversable => TRAVERSABLE_TRAIT,
                Class::Monoid => MONOID_TRAIT,
            });
            o.push('\n');
        }
        let mut mods = BTreeSet::new();
        for path in impls {
            let stem = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("x");
            let base: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
            let mut name = format!("impl_{base}");
            let mut n = 1;
            while !mods.insert(name.clone()) {
                n += 1;
                name = format!("impl_{base}_{n}");
            }
            o.push_str(&format!("#[path = \"{path}\"]\nmod {name};\npub use {name}::*;\n\n"));
        }
    }

    /// The law tests of every `(class, container)` this API declares, run
    /// over `i64` and `String` elements with generated functions.
    fn rust_laws(&self) -> String {
        let classes = self.classes();
        if classes.is_empty() {
            return String::new();
        }
        let mut o = String::from(LAWS_HEAD);
        for (class, c) in classes {
            let lc = snake(c);
            let (body, ints, strs) = match class {
                Class::Foldable => (FOLDABLE_LAWS, "foldable_C(xs)", "foldable_C(xs)"),
                Class::Functor => (
                    FUNCTOR_LAWS,
                    "functor_C(xs, |x| x.wrapping_mul(3).wrapping_add(1), |x| x ^ 5)",
                    "functor_C(xs, |s| format!(\"{s}!\"), |s| s.chars().rev().collect())",
                ),
                Class::Traversable => (
                    TRAVERSABLE_LAWS,
                    "traversable_C(xs, |x| x.wrapping_mul(3).wrapping_add(1), |x| x % 3 == 0)",
                    "traversable_C(xs, |s| format!(\"{s}!\"), |s| s.ends_with('3'))",
                ),
                Class::Monoid => (MONOID_LAWS, "monoid_C(&ints(), xs)", "monoid_C(&strs(), xs)"),
            };
            let name = class.name().to_lowercase();
            o.push_str(&body.replace("_C", &format!("_{lc}")).replace("C::", &format!("{c}::")).replace("<C<", &format!("<{c}<")));
            o.push_str(&format!(
                "\n    #[test]\n    fn {name}_{lc}_laws() {{\n        for xs in ints() {{ {} }}\n        for xs in strs() {{ {} }}\n    }}\n",
                ints.replace("_C", &format!("_{lc}")),
                strs.replace("_C", &format!("_{lc}")),
            ));
        }
        o.push_str("}\n");
        o
    }

    /// Generated TypeScript (ES2022: `#private` fields). `wire` as in `rust`;
    /// i64 is a `bigint`, bytes a `Uint8Array` (hex on the wire).
    pub fn typescript(&self, wire: bool) -> String {
        let en = pascal(&self.api);
        let mut o = format!(
            "// @generated by `dashboard arch gen` from the {} IDL. Do not edit.\n\
             // Every T is opaque: a #private field, no accessors.\n",
            self.api
        );
        let mut imports: Vec<&str> = self.refs().into_iter().map(|(a, _)| a).collect();
        imports.sort();
        imports.dedup();
        for a in imports {
            o.push_str(&format!("import * as {} from \"./{a}\";\n", a.replace('-', "_")));
        }
        o.push('\n');
        for t in &self.types {
            let inner = match &t.kind {
                Kind::Repr(Repr::String) => "string".to_string(),
                Kind::Repr(Repr::Bytes) => "Uint8Array".to_string(),
                Kind::Repr(Repr::I64) => "bigint".to_string(),
                Kind::Repr(Repr::F64) => "number".to_string(),
                Kind::Repr(Repr::Bool) => "boolean".to_string(),
                Kind::Ref(a, ty) => format!("{}.{ty}", a.replace('-', "_")),
            };
            o.push_str(&format!(
                "export class {} {{\n  readonly #v: {inner};\n  constructor(v: {inner}) {{ this.#v = v; }}\n",
                t.name
            ));
            if t.display {
                let body = match t.kind {
                    Kind::Repr(Repr::Bytes) => "Array.from(this.#v, (b) => b.toString(16).padStart(2, \"0\")).join(\"\")",
                    _ => "String(this.#v)",
                };
                o.push_str(&format!("  toString(): string {{ return {body}; }}\n"));
            }
            if wire {
                let (enc, dec) = match &t.kind {
                    Kind::Repr(Repr::String) => (
                        "this.#v".to_string(),
                        "if (typeof v !== \"string\") throw new Error(\"expected string\");\n    return new this(v);".to_string(),
                    ),
                    Kind::Repr(Repr::Bytes) => (
                        "Array.from(this.#v, (b) => b.toString(16).padStart(2, \"0\")).join(\"\")".to_string(),
                        "if (typeof v !== \"string\" || !/^([0-9a-f]{2})*$/.test(v)) throw new Error(\"expected hex string\");\n    return new this(Uint8Array.from(v.match(/../g) ?? [], (h) => parseInt(h, 16)));".to_string(),
                    ),
                    Kind::Repr(Repr::I64) => (
                        "Number(this.#v)".to_string(),
                        "if (typeof v !== \"number\" || !Number.isSafeInteger(v)) throw new Error(\"expected a safe integer\");\n    return new this(BigInt(v));".to_string(),
                    ),
                    Kind::Repr(Repr::F64) => (
                        "this.#v".to_string(),
                        "if (typeof v !== \"number\") throw new Error(\"expected number\");\n    return new this(v);".to_string(),
                    ),
                    Kind::Repr(Repr::Bool) => (
                        "this.#v".to_string(),
                        "if (typeof v !== \"boolean\") throw new Error(\"expected boolean\");\n    return new this(v);".to_string(),
                    ),
                    Kind::Ref(a, ty) => (
                        "this.#v.toValue()".to_string(),
                        format!("return new this({}.{ty}.fromValue(v));", a.replace('-', "_")),
                    ),
                };
                o.push_str(&format!(
                    "  toValue(): unknown {{ return {enc}; }}\n  static fromValue(v: unknown): {} {{\n    {dec}\n  }}\n",
                    t.name
                ));
            }
            o.push_str("}\n\n");
        }
        let variants: Vec<String> = self
            .ctors
            .iter()
            .map(|c| {
                let mut f = format!("{{ kind: \"{}\"", c.name);
                for (i, a) in c.args.iter().enumerate() {
                    f.push_str(&format!("; a{i}: {a}"));
                }
                f + " }"
            })
            .collect();
        o.push_str(&format!("export type {en} =\n  | {};\n\n", variants.join("\n  | ")));
        o.push_str(&format!("export const {en} = {{\n"));
        for c in &self.ctors {
            let params: Vec<String> = c.args.iter().enumerate().map(|(i, a)| format!("a{i}: {a}")).collect();
            let mut fields = format!("kind: \"{}\"", c.name);
            for i in 0..c.args.len() {
                fields.push_str(&format!(", a{i}"));
            }
            o.push_str(&format!("  {}: ({}): {en} => ({{ {fields} }}),\n", c.name, params.join(", ")));
        }
        o.push_str("};\n");
        if wire {
            o.push_str(&format!("\nexport function toJson(v: {en}): string {{\n  switch (v.kind) {{\n"));
            for c in &self.ctors {
                let vals: Vec<String> = (0..c.args.len()).map(|i| format!("v.a{i}.toValue()")).collect();
                o.push_str(&format!(
                    "    case \"{}\": return JSON.stringify({{ c: \"{}\", a: [{}] }});\n",
                    c.name, c.name, vals.join(", ")
                ));
            }
            o.push_str(&format!(
                "    default: {{ const never: never = v; throw new Error(String(never)); }}\n  }}\n}}\n\n\
                 export function fromJson(s: string): {en} {{\n  const j = JSON.parse(s) as {{ c?: unknown; a?: unknown }};\n  \
                 if (!Array.isArray(j.a)) throw new Error(\"missing a\");\n  const a: unknown[] = j.a;\n  switch (j.c) {{\n"
            ));
            for c in &self.ctors {
                let n = c.args.len();
                let args: Vec<String> = c.args.iter().enumerate().map(|(i, t)| format!("{t}.fromValue(a[{i}])")).collect();
                o.push_str(&format!(
                    "    case \"{}\":\n      if (a.length !== {n}) throw new Error(\"wrong argument count for {}\");\n      return {en}.{}({});\n",
                    c.name, c.name, c.name, args.join(", ")
                ));
            }
            o.push_str("    default: throw new Error(`unknown constructor ${String(j.c)}`);\n  }\n}\n");
        }
        o
    }
    /// Generated Kotlin: package `api.<name>`; a `sealed interface` for the
    /// sum, plain (not data, not value) classes for the opaque `T`s. `wire`
    /// as in `rust`, with a small file-private JSON codec, no dependency.
    pub fn kotlin(&self, wire: bool) -> String {
        let en = pascal(&self.api);
        let pkg = kotlin_pkg(&self.api);
        let mut o = format!(
            "// @generated by `dashboard arch gen` from the {} IDL. Do not edit.\n\
             // Every T is opaque: a private field, no accessors, no value equality.\n\
             package {pkg}\n\n",
            self.api
        );
        for t in &self.types {
            let (inner, enc, dec) = match &t.kind {
                Kind::Repr(Repr::String) => ("String".to_string(), "v".to_string(),
                    "x as? String ?: throw IllegalArgumentException(\"expected string\")".to_string()),
                Kind::Repr(Repr::Bytes) => ("ByteArray".to_string(),
                    "v.joinToString(\"\") { \"%02x\".format(it) }".to_string(),
                    "(x as? String)?.takeIf { it.length % 2 == 0 && it.all { c -> c in \"0123456789abcdef\" } }?.chunked(2)?.map { it.toInt(16).toByte() }?.toByteArray() ?: throw IllegalArgumentException(\"expected hex string\")".to_string()),
                Kind::Repr(Repr::I64) => ("Long".to_string(), "v".to_string(),
                    "x as? Long ?: throw IllegalArgumentException(\"expected integer\")".to_string()),
                Kind::Repr(Repr::F64) => ("Double".to_string(), "v".to_string(),
                    "(x as? Number)?.toDouble() ?: throw IllegalArgumentException(\"expected number\")".to_string()),
                Kind::Repr(Repr::Bool) => ("Boolean".to_string(), "v".to_string(),
                    "x as? Boolean ?: throw IllegalArgumentException(\"expected boolean\")".to_string()),
                Kind::Ref(a, ty) => (format!("{}.{ty}", kotlin_pkg(a)), "v.toValue()".to_string(),
                    format!("{}.{ty}.fromValue(x)", kotlin_pkg(a))),
            };
            o.push_str(&format!("class {}(private val v: {inner}) {{\n", t.name));
            if t.display {
                let body = match t.kind {
                    Kind::Repr(Repr::Bytes) => "v.joinToString(\"\") { \"%02x\".format(it) }",
                    Kind::Repr(Repr::String) => "v",
                    _ => "v.toString()",
                };
                o.push_str(&format!("    override fun toString(): String = {body}\n"));
            }
            if wire {
                o.push_str(&format!(
                    "    internal fun toValue(): Any? = {enc}\n    companion object {{\n        internal fun fromValue(x: Any?): {} = {}({dec})\n    }}\n",
                    t.name, t.name
                ));
            }
            o.push_str("}\n\n");
        }
        o.push_str(&format!("sealed interface {en} {{\n"));
        for c in &self.ctors {
            if c.args.is_empty() {
                o.push_str(&format!("    object {} : {en}\n", c.name));
            } else {
                let ps: Vec<String> = c.args.iter().enumerate().map(|(i, a)| format!("val a{i}: {a}")).collect();
                o.push_str(&format!("    class {}({}) : {en}\n", c.name, ps.join(", ")));
            }
        }
        o.push_str("}\n");
        if wire {
            o.push_str(&format!("\nfun toJson(v: {en}): String = Wire.write(when (v) {{\n"));
            for c in &self.ctors {
                let vals: Vec<String> = (0..c.args.len()).map(|i| format!("v.a{i}.toValue()")).collect();
                let pat = if c.args.is_empty() { format!("{en}.{}", c.name) } else { format!("is {en}.{}", c.name) };
                o.push_str(&format!(
                    "    {pat} -> mapOf(\"c\" to \"{}\", \"a\" to listOf<Any?>({}))\n",
                    c.name, vals.join(", ")
                ));
            }
            o.push_str(&format!(
                "}})\n\nfun fromJson(s: String): {en} {{\n    val j = Wire.read(s) as? Map<*, *> ?: throw IllegalArgumentException(\"expected object\")\n    \
                 val a = j[\"a\"] as? List<*> ?: throw IllegalArgumentException(\"missing a\")\n    return when (val c = j[\"c\"]) {{\n"
            ));
            for c in &self.ctors {
                let n = c.args.len();
                let args: Vec<String> = c.args.iter().enumerate().map(|(i, t)| format!("{t}.fromValue(a[{i}])")).collect();
                let build = if n == 0 { format!("{en}.{}", c.name) } else { format!("{en}.{}({})", c.name, args.join(", ")) };
                o.push_str(&format!(
                    "        \"{}\" -> {{\n            require(a.size == {n}) {{ \"wrong argument count for {}\" }}\n            {build}\n        }}\n",
                    c.name, c.name
                ));
            }
            o.push_str("        else -> throw IllegalArgumentException(\"unknown constructor $c\")\n    }\n}\n");
            o.push_str(include_str!("kotlin_wire.kt"));
        }
        o
    }
}

fn rust_repr(r: Repr) -> &'static str {
    match r {
        Repr::String => "String",
        Repr::Bytes => "Vec<u8>",
        Repr::I64 => "i64",
        Repr::F64 => "f64",
        Repr::Bool => "bool",
    }
}

/// `RowSet` -> `row_set`.
fn snake(s: &str) -> String {
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            o.push('_');
        }
        o.push(c.to_ascii_lowercase());
    }
    o
}

const FOLDABLE_TRAIT: &str = "\
/// Foldable: `foldr` is the only way to read the container: no length, no
/// index, no iteration the consumer drives. Laws: `foldr f z` is the right
/// fold of the elements in order, and `new(xs)` holds exactly `xs`.
pub trait Foldable<A> {
    fn foldr<B>(&self, f: impl FnMut(&A, B) -> B, z: B) -> B;
}
";

const FUNCTOR_TRAIT: &str = "\
/// Functor. Laws: `fmap id == id`, `fmap (g . f) == fmap g . fmap f`, and
/// `foldr f z (fmap g xs) == foldr (f . g) z xs`.
pub trait Functor<A> {
    type Of<B>;
    fn fmap<B>(&self, f: impl FnMut(&A) -> B) -> Self::Of<B>;
}
";

const TRAVERSABLE_TRAIT: &str = "\
/// Traversable, over the `Result` applicative. Laws: `traverse Ok == Ok`,
/// naturality (mapping the error commutes with `traverse`), the first
/// failure in element order wins, and `fmap` recovered from `traverse` is
/// the elements mapped in order.
pub trait Traversable<A> {
    type Of<B>;
    fn traverse<B, E>(&self, f: impl FnMut(&A) -> Result<B, E>) -> Result<Self::Of<B>, E>;
}
";

const MONOID_TRAIT: &str = "\
/// Monoid: for the container (`append` is concatenation) and for any
/// accumulator a consumer folds into. Laws: `empty` is a left and right
/// identity, `append` is associative.
pub trait Monoid {
    fn empty() -> Self;
    fn append(self, other: Self) -> Self;
}

/// `foldMap`: maps each element into a monoid and appends the results in order.
pub fn fold_map<A, M: Monoid>(xs: &impl Foldable<A>, mut f: impl FnMut(&A) -> M) -> M {
    xs.foldr(|x, acc| f(x).append(acc), M::empty())
}
";

const LAWS_HEAD: &str = "
#[cfg(test)]
mod laws {
    use super::*;
    use std::fmt::Debug;

    fn ints() -> Vec<Vec<i64>> {
        vec![vec![], vec![7], vec![1, -2, 3], (0..40).map(|i| (i * 7919) % 101 - 50).collect()]
    }

    fn strs() -> Vec<Vec<String>> {
        ints().into_iter().map(|v| v.into_iter().map(|x| format!(\"s{x}\")).collect()).collect()
    }

    /// The elements in order, read through `foldr` alone.
    fn items<A: Clone>(s: &impl Foldable<A>) -> Vec<A> {
        let mut v = s.foldr(|x, mut acc: Vec<A>| { acc.push(x.clone()); acc }, Vec::new());
        v.reverse();
        v
    }

    /// A non-associative, order-revealing fold step.
    fn tree<A: Debug>(x: &A, acc: String) -> String {
        format!(\"({x:?} {acc})\")
    }
";

const FOLDABLE_LAWS: &str = "
    fn foldable_C<A: Clone + PartialEq + Debug>(xs: Vec<A>) {
        let s = C::new(xs.clone());
        assert_eq!(items(&s), xs, \"new(xs) holds xs, in order\");
        let want = xs.iter().rev().fold(String::new(), |acc, x| tree(x, acc));
        assert_eq!(s.foldr(tree, String::new()), want, \"foldr is the right fold\");
    }
";

const FUNCTOR_LAWS: &str = "
    fn functor_C<A: Clone + PartialEq + Debug>(xs: Vec<A>, f: fn(&A) -> A, g: fn(&A) -> A) {
        let s = C::new(xs.clone());
        assert_eq!(items(&s.fmap(|x| x.clone())), xs, \"fmap id == id\");
        assert_eq!(items(&s.fmap(|x| g(&f(x)))), items(&s.fmap(f).fmap(g)), \"fmap (g . f) == fmap g . fmap f\");
        assert_eq!(
            s.fmap(g).foldr(tree, String::new()),
            s.foldr(|x, acc| tree(&g(x), acc), String::new()),
            \"foldr f z (fmap g xs) == foldr (f . g) z xs\"
        );
    }
";

const TRAVERSABLE_LAWS: &str = "
    fn traversable_C<A: Clone + PartialEq + Debug>(xs: Vec<A>, f: fn(&A) -> A, bad: fn(&A) -> bool) {
        let s = C::new(xs.clone());
        let id = s.traverse(|x| Ok::<A, ()>(x.clone())).map(|t| items(&t));
        assert_eq!(id, Ok(xs.clone()), \"traverse Ok == Ok\");
        let mapped = s.traverse(|x| Ok::<A, ()>(f(x))).map(|t| items(&t));
        assert_eq!(mapped, Ok(xs.iter().map(f).collect()), \"fmap from traverse maps in order\");
        let step = |x: &A| if bad(x) { Err(x.clone()) } else { Ok(f(x)) };
        let got = s.traverse(step).map(|t| items(&t));
        match xs.iter().find(|x| bad(x)) {
            Some(e) => assert_eq!(got, Err(e.clone()), \"the first failure in order wins\"),
            None => assert_eq!(got, Ok(xs.iter().map(f).collect()), \"no failure: every element mapped\"),
        }
        let h = |e: A| format!(\"{e:?}\");
        let lhs = s.traverse(|x| step(x).map_err(h)).map(|t| items(&t));
        let rhs = s.traverse(step).map(|t| items(&t)).map_err(h);
        assert_eq!(lhs, rhs, \"naturality\");
    }
";

const MONOID_LAWS: &str = "
    fn monoid_C<A: Clone + PartialEq + Debug>(all: &[Vec<A>], xs: Vec<A>) {
        let n = |v: &Vec<A>| C::new(v.clone());
        let e = || <C<A> as Monoid>::empty();
        assert_eq!(items(&e().append(n(&xs))), xs, \"empty <> x == x\");
        assert_eq!(items(&n(&xs).append(e())), xs, \"x <> empty == x\");
        for ys in all {
            let cat: Vec<A> = xs.iter().chain(ys).cloned().collect();
            assert_eq!(items(&n(&xs).append(n(ys))), cat, \"append is concatenation\");
            for zs in all {
                assert_eq!(
                    items(&n(&xs).append(n(ys)).append(n(zs))),
                    items(&n(&xs).append(n(ys).append(n(zs)))),
                    \"append is associative\"
                );
            }
        }
        assert_eq!(items(&fold_map(&n(&xs), |x| n(&vec![x.clone()]))), xs, \"fold_map of singletons is the identity\");
    }
";

/// `api.user_events` for API `user-events`.
pub fn kotlin_pkg(api: &str) -> String {
    format!("api.{}", api.replace('-', "_"))
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

    const DB: &str = r#"
        api "db"
        element "Row" {
            variant "Meal" "f64" "string"
            variant "Note" "string"
        }
        container "Rows" of="Row" ref="foldable.Seq"
        type "Reason" repr="string"
        constructor "Success" "Rows"
        constructor "Failure" "Reason"
    "#;

    #[test]
    fn parses_elements_containers_and_instances() {
        let idl = Idl::parse(DB).unwrap();
        assert_eq!((idl.constructors(), idl.slots()), (2, 2));
        assert_eq!((idl.element_variants(), idl.element_fields()), (2, 3));
        assert_eq!(idl.container_refs(), [("foldable", "Seq")]);
        assert!(idl.classes().is_empty());
        let sums: Vec<String> = idl.sums().into_iter().map(|s| s.0).collect();
        assert_eq!(sums, ["Db", "Row"]);
        let owner = Idl::parse("api \"foldable\"\ncontainer \"Seq\" classes=\"Foldable, Monoid\"").unwrap();
        assert_eq!(owner.classes(), [(Class::Foldable, "Seq"), (Class::Monoid, "Seq")]);
        assert!(owner.owns_generic("Seq"));
        let inst = Idl::parse("api \"m\"\ninstance \"Monoid\" for=\"foldable.Seq\"").unwrap();
        assert_eq!(inst.classes(), [(Class::Monoid, "Seq")]);
        let rs = inst.rust(false, &["../../m/seq.rs".to_string()]);
        assert!(rs.contains("use super::foldable::{Foldable, Seq};"), "{rs}");
        assert!(rs.contains("#[path = \"../../m/seq.rs\"]\nmod impl_seq;"), "{rs}");
        assert!(rs.contains("pub fn fold_map") && rs.contains("fn monoid_seq_laws()"), "{rs}");
        assert!(!rs.contains("pub enum M"), "{rs}");
    }

    #[test]
    fn rejects_bad_containers() {
        let el = "element \"Row\" { variant \"A\" \"i64\"; }\n";
        for (bad, want) in [
            (format!("{el}constructor \"X\" \"Row\""), "no container holds it"),
            (format!("{el}container \"Rs\" of=\"Row\" ref=\"f.Seq\"\nconstructor \"X\" \"Row\" \"Rs\""), "reachable only through a container"),
            ("element \"Row\" { variant \"A\" \"Nope\"; }".to_string(), "unknown repr"),
            ("container \"Seq\" classes=\"Functor\"".to_string(), "must include Foldable"),
            ("container \"Seq\" classes=\"Foldable,Monad\"".to_string(), "unknown class"),
            ("container \"Seq\" classes=\"Foldable\"\nconstructor \"X\" \"Seq\"".to_string(), "is generic"),
            ("container \"Rs\" of=\"Row\"".to_string(), "either classes="),
            (format!("{el}container \"Rs\" of=\"Row\" ref=\"f.Seq\"\nconstructor \"X\""), "never used"),
            ("instance \"Foldable\" for=\"f.Seq\"".to_string(), "Foldable is declared by"),
            ("instance \"Functor\" for=\"f.Seq\"\ninstance \"Monoid\" for=\"g.Seq\"".to_string(), "of one api"),
            ("instance \"Functor\" for=\"Seq\"".to_string(), "api.Container"),
        ] {
            let e = Idl::parse(&format!("api \"a\"\n{bad}")).unwrap_err();
            assert!(e.contains(want), "{bad}: {e}");
        }
    }

    #[test]
    fn typescript_is_opaque_and_wire_is_optional() {
        let idl = Idl::parse(AB).unwrap();
        let plain = idl.typescript(false);
        assert!(plain.contains("export class UserId {\n  readonly #v: string;"));
        assert!(plain.contains("kind: \"Created\"; a0: UserId; a1: Payload"));
        assert!(plain.contains("toString()") && !plain.contains("toValue"));
        assert!(idl.typescript(true).contains("export function fromJson"));
    }

    #[test]
    fn kotlin_is_opaque_and_wire_is_optional() {
        let idl = Idl::parse(AB).unwrap();
        let plain = idl.kotlin(false);
        assert!(plain.contains("package api.ab"));
        assert!(plain.contains("class UserId(private val v: String)"));
        assert!(plain.contains("class Created(val a0: UserId, val a1: Payload) : Ab"));
        assert!(plain.contains("object Tick : Ab"));
        assert!(!plain.contains("data class") && !plain.contains("value class") && !plain.contains("Wire"));
        assert!(idl.kotlin(true).contains("private object Wire"));
    }

    #[test]
    fn rust_is_opaque_and_wire_is_optional() {
        let idl = Idl::parse(AB).unwrap();
        let plain = idl.rust(false, &[]);
        assert!(plain.contains("pub struct UserId(String);"));
        assert!(plain.contains("pub enum Ab {\n    Created(UserId, Payload),"));
        assert!(!plain.contains("derive") && !plain.contains("serde_json"));
        assert!(plain.contains("impl std::fmt::Display for UserId"));
        assert!(!plain.contains("Display for Payload"));
        let wire = idl.rust(true, &[]);
        assert!(wire.contains("pub fn to_json") && wire.contains("pub fn from_json"));
    }
}
