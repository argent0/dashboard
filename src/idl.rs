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
//
// Proofs supplied by the caller (architect-v3.md): a `where` refinement on a
// type (`type Grams : f64 where 0 < v ∧ v ≤ 20000`, Lean only) becomes the
// smart constructor's check in every language; a `token` is a T only the
// API's generator mints (`versioned stale=Ctor` carries a version and
// `linear spent=Ctor` is single-use; each forces its failure constructor
// into the sum of every fn taking it).

use kdl::{KdlDocument, KdlNode, KdlValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Repr(Repr),
    /// `(api, type)` of another API.
    Ref(String, String),
    /// A product of two to four prims (`i64 × i64`): a tuple in the binding.
    Tuple(Vec<Field>),
}

/// A comparison of a `where` refinement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "≤")]
    Le,
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "≠")]
    Ne,
}

/// An operand of a `where` refinement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Atom {
    /// The value.
    V,
    /// `v.N`: a component of a product, 1-based.
    Field(u32),
    /// `v.length`: characters of a string, bytes of a blob.
    Len,
    /// A numeric literal, as written.
    Num(String),
    Str(String),
    Bool(bool),
}

/// A `where` refinement (architect-v3.md, "Proofs supplied by the caller"):
/// the decidable fragment the Lean DSL elaborates. The same tree is the
/// reference type's `Prop` and the generated smart constructor's check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pred {
    Cmp(Op, Atom, Atom),
    /// `a ∈ [literals]`.
    Mem(Atom, Vec<Atom>),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
}

/// How the fact a token proves can change between minting and use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    /// Never, once true: an append-only "exists".
    Permanent,
    /// Revocable: the token carries `(id, version)` and the owner re-checks
    /// atomically; every fn taking it returns this constructor.
    Versioned(String),
    /// Single use (a hold, a one-shot): moved in Rust; every fn taking it
    /// returns this constructor.
    Linear(String),
}

impl TokenKind {
    /// The constructor a fn taking the token must be able to return.
    pub fn failure(&self) -> Option<&str> {
        match self {
            TokenKind::Permanent => None,
            TokenKind::Versioned(c) | TokenKind::Linear(c) => Some(c),
        }
    }

    pub fn word(&self) -> &'static str {
        match self {
            TokenKind::Permanent => "permanent",
            TokenKind::Versioned(_) => "versioned",
            TokenKind::Linear(_) => "linear",
        }
    }
}

/// Evidence: a T only the API's generator mints (`mint`, never `new`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Token {
    pub name: String,
    pub repr: Repr,
    pub max_len: u64,
    pub kind: TokenKind,
    pub store: bool,
}

impl Default for Token {
    fn default() -> Self {
        Token { name: String::new(), repr: Repr::I64, max_len: 0, kind: TokenKind::Permanent, store: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TypeDecl {
    pub name: String,
    pub kind: Kind,
    pub display: bool,
    /// The most characters (string) or bytes a value may hold; 0: no bound
    /// was declared (the Lean IDL requires one).
    pub max_len: u64,
    /// The callee stores this T without reading it: a forwarding adapter
    /// (`ToSql` in Rust) is generated.
    pub store: bool,
    /// The `where` refinement (Lean only), beside the `max_len` bound.
    pub pred: Option<Pred>,
}

impl Default for TypeDecl {
    fn default() -> Self {
        TypeDecl { name: String::new(), kind: Kind::Repr(Repr::String), display: false, max_len: 0, store: false, pred: None }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Ctor {
    pub name: String,
    pub args: Vec<String>,
}

/// A named sum (`sum "Written" { constructor ... }`): what a `fn` returns.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sum {
    pub name: String,
    pub ctors: Vec<Ctor>,
}

/// A method of the API: `name :: inputs -> returns`, where every input is a
/// type or container of this API and `returns` names one of its sums.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fn {
    pub name: String,
    pub inputs: Vec<String>,
    pub returns: String,
}

/// A container class: the operations a consumer gets, and the laws an
/// instance is tested against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

/// A raw field of an element variant: a repr and, for strings and bytes,
/// its `max_len` (0: none declared).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Field {
    pub repr: Repr,
    pub max_len: u64,
}

impl Default for Field {
    fn default() -> Self {
        Field { repr: Repr::String, max_len: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
}

/// A readable flat sum of products over raw reprs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Element {
    pub name: String,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerKind {
    /// `Seq a`, owned by this API, with its classes (Foldable among them).
    Generic(Vec<Class>),
    /// `(element, api, container)`: another API's generic over an element of this one.
    Of(String, String, String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Container {
    pub name: String,
    pub kind: ContainerKind,
}

/// A class added to a container another API owns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub class: Class,
    pub api: String,
    pub container: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Idl {
    pub api: String,
    pub types: Vec<TypeDecl>,
    pub tokens: Vec<Token>,
    /// The default sum's constructors (the API's own `pascal(api)` sum).
    pub ctors: Vec<Ctor>,
    pub elements: Vec<Element>,
    pub containers: Vec<Container>,
    pub instances: Vec<Instance>,
    pub sums: Vec<Sum>,
    pub fns: Vec<Fn>,
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
                    types.push(TypeDecl { name: name.clone(), kind, display, max_len: 0, store: false, pred: None });
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
                            .map(|f| {
                                Repr::parse(f)
                                    .map(|repr| Field { repr, max_len: 0 })
                                    .ok_or_else(|| format!("element {name}: variant {vname}: unknown repr {f:?}"))
                            })
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
            tokens: Vec::new(),
            ctors,
            elements,
            containers,
            instances,
            sums: Vec::new(),
            fns: Vec::new(),
        };
        let errs = idl.validate();
        if !errs.is_empty() {
            return Err(errs.join("\n"));
        }
        Ok(idl)
    }

    /// An IDL the Lean exporter produced (`lean::export`): the same checks
    /// the KDL parser runs, since Lean type-checks only part of them.
    /// Every problem is listed, one per line.
    pub fn from_export(idl: Idl) -> Result<Idl, String> {
        let errs = idl.validate();
        if !errs.is_empty() {
            return Err(errs.join("\n"));
        }
        Ok(idl)
    }

    /// Every problem with the IDL, not just the first.
    pub(crate) fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        let mut names = BTreeSet::new();
        for t in &self.types {
            if !ident_ok(&t.name) || !names.insert(t.name.as_str()) {
                errs.push(format!("type {:?}: not a CamelCase identifier, or duplicate", t.name));
            }
            if let Kind::Tuple(fs) = &t.kind {
                if !(2..=4).contains(&fs.len()) {
                    errs.push(format!("type {}: a product has two to four components", t.name));
                }
                if t.display || t.store {
                    errs.push(format!("type {}: display and store are for scalar types, not a product", t.name));
                }
            }
            if let Some(p) = &t.pred {
                if matches!(t.kind, Kind::Ref(..)) {
                    errs.push(format!("type {}: a ref carries its own refinement; `where` is for a repr or a product", t.name));
                }
                if let Err(e) = p.check(&self.components(t)) {
                    errs.push(format!("type {}: {e}", t.name));
                }
            }
        }
        for k in &self.tokens {
            if !ident_ok(&k.name) || !names.insert(k.name.as_str()) {
                errs.push(format!("token {:?}: not a CamelCase identifier, or duplicate", k.name));
            }
        }
        for (what, n) in self
            .elements
            .iter()
            .map(|e| ("element", &e.name))
            .chain(self.containers.iter().map(|c| ("container", &c.name)))
        {
            if !ident_ok(n) || !names.insert(n.as_str()) {
                errs.push(format!("{what} {n:?}: not a CamelCase identifier, or duplicate"));
            }
        }
        for e in &self.elements {
            let mut vs = BTreeSet::new();
            for v in &e.variants {
                if !ident_ok(&v.name) || !vs.insert(v.name.as_str()) {
                    errs.push(format!("element {}: variant {:?}: not a CamelCase identifier, or duplicate", e.name, v.name));
                }
            }
            if e.variants.is_empty() {
                errs.push(format!("element {}: no variants", e.name));
            }
            if !self.containers.iter().any(|c| matches!(&c.kind, ContainerKind::Of(of, ..) if *of == e.name)) {
                errs.push(format!("element {}: no container holds it (an element is reachable only through one)", e.name));
            }
        }
        for c in &self.containers {
            match &c.kind {
                ContainerKind::Generic(classes) => {
                    if !classes.contains(&Class::Foldable) {
                        errs.push(format!("container {}: a generic's classes must include Foldable, its primitive", c.name));
                    }
                    if !self.instances.is_empty() {
                        errs.push(format!("container {}: an api owning a container adds no instances to another's", c.name));
                    }
                }
                ContainerKind::Of(of, ..) => {
                    if !self.elements.iter().any(|e| e.name == *of) {
                        errs.push(format!("container {}: of={of:?} is not an element of this api", c.name));
                    }
                }
            }
        }
        for i in self.instances.iter().filter(|i| i.class == Class::Foldable) {
            errs.push(format!("instance Foldable for {}.{}: Foldable is declared by the api owning the container", i.api, i.container));
        }
        if self.instances.iter().any(|i| i.api != self.instances[0].api) {
            errs.push("instances: all for containers of one api".to_string());
        }
        let mut cnames = BTreeSet::new();
        let mut used = BTreeSet::new();
        for c in &self.ctors {
            if !ident_ok(&c.name) || !cnames.insert(c.name.as_str()) {
                errs.push(format!("constructor {:?}: not a CamelCase identifier, or duplicate", c.name));
            }
            for a in &c.args {
                if self.is_sum(a) {
                    // A named sum nests: the consumer matches it in turn.
                    used.insert(a.as_str());
                    continue;
                }
                if !names.contains(a.as_str()) {
                    errs.push(format!("constructor {}: unknown type {a:?}", c.name));
                }
                if self.elements.iter().any(|e| e.name == *a) {
                    errs.push(format!("constructor {}: element {a} is reachable only through a container", c.name));
                }
                if self.containers.iter().any(|x| x.name == *a && matches!(x.kind, ContainerKind::Generic(_))) {
                    errs.push(format!("constructor {}: {a} is generic; instantiate it with `container ... of=`", c.name));
                }
                used.insert(a.as_str());
            }
        }
        // Named sums and fns (architect-v3.md, "APIs are signatures").
        let mut snames = BTreeSet::new();
        for s in &self.sums {
            if !ident_ok(&s.name) || names.contains(s.name.as_str()) || !snames.insert(s.name.as_str()) {
                errs.push(format!("sum {:?}: not a CamelCase identifier, or duplicate", s.name));
            }
            if s.ctors.is_empty() {
                errs.push(format!("sum {}: no constructors", s.name));
            }
            for c in &s.ctors {
                if !ident_ok(&c.name) || !cnames.insert(c.name.as_str()) {
                    errs.push(format!("sum {}: constructor {:?}: not a CamelCase identifier, or duplicate across the api's sums", s.name, c.name));
                }
                for a in &c.args {
                    if self.is_sum(a) {
                        // Declared before this one: sums nest, never recursively.
                        if snames.contains(a.as_str()) && *a != s.name {
                            used.insert(a.as_str());
                        } else {
                            errs.push(format!("sum {}: constructor {}: sum {a} must be declared before {} (a sum never contains itself)", s.name, c.name, s.name));
                        }
                        continue;
                    }
                    if !names.contains(a.as_str()) {
                        errs.push(format!("sum {}: constructor {}: unknown type {a:?}", s.name, c.name));
                    }
                    if self.elements.iter().any(|e| e.name == *a) {
                        errs.push(format!("sum {}: constructor {}: element {a} is reachable only through a container", s.name, c.name));
                    }
                    if self.containers.iter().any(|x| x.name == *a && matches!(x.kind, ContainerKind::Generic(_))) {
                        errs.push(format!("sum {}: constructor {}: {a} is generic; instantiate it with `container ... of=`", s.name, c.name));
                    }
                    used.insert(a.as_str());
                }
            }
        }
        let mut fnames = BTreeSet::new();
        for f in &self.fns {
            if f.name.is_empty()
                || !f.name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                || !f.name.chars().all(|c| c.is_ascii_alphanumeric())
                || !fnames.insert(f.name.as_str())
            {
                errs.push(format!("fn {:?}: not a camelCase identifier, or duplicate", f.name));
            }
            for a in &f.inputs {
                if snames.contains(a.as_str()) {
                    // A closed vocabulary: the one input a callee may match on.
                    used.insert(a.as_str());
                    continue;
                }
                if !names.contains(a.as_str()) {
                    errs.push(format!("fn {}: unknown input {a:?}", f.name));
                }
                if self.elements.iter().any(|e| e.name == *a) {
                    errs.push(format!("fn {}: element {a} is reachable only through a container", f.name));
                }
                if self.containers.iter().any(|x| x.name == *a && matches!(x.kind, ContainerKind::Generic(_))) {
                    errs.push(format!("fn {}: {a} is generic; instantiate it with `container ... of=`", f.name));
                }
                used.insert(a.as_str());
            }
            if !snames.contains(f.returns.as_str()) {
                errs.push(format!("fn {}: returns {:?}, which is not a sum of this api", f.name, f.returns));
            }
            used.insert(f.returns.as_str());
        }
        for s in self.sums.iter().filter(|s| !used.contains(s.name.as_str())) {
            errs.push(format!("sum {}: declared but no fn returns or takes it", s.name));
        }
        for t in self.types.iter().filter(|t| !used.contains(t.name.as_str())) {
            errs.push(format!("type {}: declared but never used", t.name));
        }
        for k in self.tokens.iter().filter(|k| !used.contains(k.name.as_str())) {
            errs.push(format!("token {}: declared but never used", k.name));
        }
        // A fact that can change by the call: the signature admits it.
        for k in &self.tokens {
            let Some(c) = k.kind.failure() else { continue };
            for f in self.fns_missing(&k.name, c) {
                errs.push(format!(
                    "fn {}: takes {} ({}), so {} must have a `{c}` constructor: the fact may have changed by the call",
                    f.name,
                    k.name,
                    k.kind.word(),
                    f.returns
                ));
            }
        }
        for c in self
            .containers
            .iter()
            .filter(|c| matches!(c.kind, ContainerKind::Of(..)) && !used.contains(c.name.as_str()))
        {
            errs.push(format!("container {}: declared but never used", c.name));
        }
        if self.ctors.is_empty() && self.fns.is_empty() && self.classes().is_empty() {
            errs.push("no constructors".to_string());
        }
        errs
    }

    /// Every constructor across the default sum and the named sums.
    fn all_ctors(&self) -> impl Iterator<Item = &Ctor> {
        self.ctors.iter().chain(self.sums.iter().flat_map(|s| s.ctors.iter()))
    }

    /// Number of constructors across all sums (the `max_constructors` measure).
    pub fn constructors(&self) -> u64 {
        self.all_ctors().count() as u64
    }

    /// Argument slots across all constructors (the `max_types` measure).
    pub fn slots(&self) -> u64 {
        self.all_ctors().map(|c| c.args.len() as u64).sum()
    }

    /// Number of fns (the `max_fns` measure).
    pub fn fn_count(&self) -> u64 {
        self.fns.len() as u64
    }

    /// Input slots across all fns (the `max_inputs` measure).
    pub fn inputs(&self) -> u64 {
        self.fns.iter().map(|f| f.inputs.len() as u64).sum()
    }

    /// Comparisons and memberships across the API's `where` refinements
    /// (the `max_rules` measure).
    pub fn rules(&self) -> u64 {
        self.types.iter().filter_map(|t| t.pred.as_ref()).map(Pred::leaves).sum()
    }

    /// Tokens declared (the `max_tokens` measure).
    pub fn token_count(&self) -> u64 {
        self.tokens.len() as u64
    }

    pub fn token(&self, name: &str) -> Option<&Token> {
        self.tokens.iter().find(|k| k.name == name)
    }

    pub fn token_names(&self) -> Vec<&str> {
        self.tokens.iter().map(|k| k.name.as_str()).collect()
    }

    /// Whether `name` is a type or token of this API (what a `ref` may name).
    pub fn declares(&self, name: &str) -> bool {
        self.type_decl(name).is_some() || self.token(name).is_some()
    }

    /// The fns taking `input` whose returned sum has no `ctor` constructor:
    /// a versioned or linear token's failure must be in the signature.
    pub fn fns_missing(&self, input: &str, ctor: &str) -> Vec<&Fn> {
        self.fns
            .iter()
            .filter(|f| f.inputs.iter().any(|i| i == input))
            .filter(|f| !self.sums.iter().find(|s| s.name == f.returns).is_some_and(|s| s.ctors.iter().any(|c| c.name == ctor)))
            .collect()
    }

    /// The raw components of a type: one for a scalar, each of a product.
    fn components(&self, t: &TypeDecl) -> Vec<Field> {
        match &t.kind {
            Kind::Repr(r) => vec![Field { repr: *r, max_len: t.max_len }],
            Kind::Tuple(fs) => fs.clone(),
            Kind::Ref(..) => Vec::new(),
        }
    }

    /// Every check a type's smart constructor runs, in order: the bounds,
    /// then the `where` predicate. Empty: `new` is infallible.
    fn rules_of(&self, t: &TypeDecl) -> Vec<Rule> {
        let comps = self.components(t);
        let mut out = Vec::new();
        for (i, f) in comps.iter().enumerate() {
            if f.max_len == 0 || !matches!(f.repr, Repr::String | Repr::Bytes) {
                continue;
            }
            let at = if comps.len() > 1 { Some(i) } else { None };
            out.push(Rule {
                text: format!("{} ≤ {}", Atom::len_text(at), f.max_len),
                rust: format!("{} <= {}", Atom::rust_len(f.repr, at), f.max_len),
                ts: format!("{} <= {}", Atom::ts_len(f.repr, at), f.max_len),
                kotlin: format!("{} <= {}", Atom::kotlin_len(f.repr, at), f.max_len),
            });
        }
        if let Some(p) = &t.pred {
            out.push(Rule { text: p.render(), rust: p.rust(&comps), ts: p.ts(&comps), kotlin: p.kotlin(&comps) });
        }
        out
    }

    /// The largest `max_len` among string types and fields, and among bytes.
    pub fn longest(&self) -> (u64, u64) {
        let mut out = (0, 0);
        let mut see = |repr: Repr, n: u64| match repr {
            Repr::String => out.0 = out.0.max(n),
            Repr::Bytes => out.1 = out.1.max(n),
            _ => {}
        };
        for t in &self.types {
            match &t.kind {
                Kind::Repr(r) => see(*r, t.max_len),
                Kind::Tuple(fs) => fs.iter().for_each(|f| see(f.repr, f.max_len)),
                Kind::Ref(..) => {}
            }
        }
        for k in &self.tokens {
            see(k.repr, k.max_len);
        }
        for f in self.elements.iter().flat_map(|e| &e.variants).flat_map(|v| &v.fields) {
            see(f.repr, f.max_len);
        }
        out
    }

    /// Strings and bytes with no `max_len` (architect-v3.md, "No tunnels"):
    /// one line each. The Lean IDL requires a bound on every one.
    pub fn unbounded(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in &self.types {
            if matches!(t.kind, Kind::Repr(Repr::String | Repr::Bytes)) && t.max_len == 0 {
                out.push(format!("type {}: a string or bytes type needs a max_len (a tunnel otherwise)", t.name));
            }
            if let Kind::Tuple(fs) = &t.kind {
                for (i, f) in fs.iter().enumerate() {
                    if matches!(f.repr, Repr::String | Repr::Bytes) && f.max_len == 0 {
                        out.push(format!("type {}: component {} needs a max_len (`string 64`)", t.name, i + 1));
                    }
                }
            }
        }
        for k in &self.tokens {
            if matches!(k.repr, Repr::String | Repr::Bytes) && k.max_len == 0 {
                out.push(format!("token {}: a string or bytes token needs a max_len", k.name));
            }
        }
        for e in &self.elements {
            for v in &e.variants {
                for (i, f) in v.fields.iter().enumerate() {
                    if matches!(f.repr, Repr::String | Repr::Bytes) && f.max_len == 0 {
                        out.push(format!("element {}: variant {}: field {i} needs a max_len (`string 64`)", e.name, v.name));
                    }
                }
            }
        }
        out
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

    /// Whether this API declares nothing but containers and instances (a
    /// class API).
    pub fn only_containers(&self) -> bool {
        self.types.is_empty()
            && self.tokens.is_empty()
            && self.ctors.is_empty()
            && self.sums.is_empty()
            && self.fns.is_empty()
            && self.elements.is_empty()
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
    /// the API's own sum first, then its named sums, then each element.
    pub fn sums(&self) -> Vec<(String, Vec<&str>, bool)> {
        let mut out = Vec::new();
        if !self.ctors.is_empty() {
            out.push((pascal(&self.api), self.ctors.iter().map(|c| c.name.as_str()).collect(), false));
        }
        for s in &self.sums {
            out.push((s.name.clone(), s.ctors.iter().map(|c| c.name.as_str()).collect(), false));
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

    /// `(name, constructors)` of every sum: the named ones, then the
    /// default sum (`pascal(api)`) if the API has one.
    fn sum_list(&self) -> Vec<(String, &[Ctor])> {
        let mut out: Vec<(String, &[Ctor])> = self.sums.iter().map(|s| (s.name.clone(), s.ctors.as_slice())).collect();
        if !self.ctors.is_empty() {
            out.push((pascal(&self.api), self.ctors.as_slice()));
        }
        out
    }

    /// Whether `name` is one of this API's named sums.
    fn is_sum(&self, name: &str) -> bool {
        self.sums.iter().any(|s| s.name == name)
    }

    /// Whether `name` is one of this API's instantiated containers.
    fn is_container(&self, name: &str) -> bool {
        self.containers.iter().any(|c| c.name == name && matches!(c.kind, ContainerKind::Of(..)))
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
            // Encoding a container walks it with `foldr`.
            let mut owners: Vec<&str> = self
                .containers
                .iter()
                .filter_map(|c| match &c.kind {
                    ContainerKind::Of(_, a, _) => Some(a.as_str()),
                    _ => None,
                })
                .collect();
            owners.sort();
            owners.dedup();
            for a in owners {
                o.push_str(&format!("use super::{}::Foldable;\n", a.replace('-', "_")));
            }
        }
        o.push('\n');
        self.rust_classes(impls, &mut o);
        if self.types.iter().any(|t| !self.rules_of(t).is_empty()) {
            o.push_str(VIOLATION_RS);
        }
        for t in &self.types {
            let inner = match &t.kind {
                Kind::Repr(r) => rust_repr(*r).to_string(),
                Kind::Ref(a, ty) => format!("super::{}::{ty}", a.replace('-', "_")),
                Kind::Tuple(fs) => format!("({})", fs.iter().map(|f| rust_repr(f.repr)).collect::<Vec<_>>().join(", ")),
            };
            o.push_str(&format!("pub struct {}({inner});\n", t.name));
            o.push_str(&format!("impl {} {{\n", t.name));
            let rules = self.rules_of(t);
            if rules.is_empty() {
                o.push_str(&format!("    pub fn new(v: {inner}) -> Self {{ Self(v) }}\n"));
            } else {
                o.push_str(&format!(
                    "    /// `Err` when `v` breaks a rule of the IDL: never truncated or clamped.\n    pub fn new(v: {inner}) -> Result<Self, Violation> {{\n"
                ));
                for r in &rules {
                    o.push_str(&format!(
                        "        if !({}) {{ return Err(Violation {{ type_name: \"{}\", rule: {:?} }}); }}\n",
                        r.rust, t.name, r.text
                    ));
                }
                o.push_str("        Ok(Self(v))\n    }\n");
            }
            if wire {
                match &t.kind {
                    Kind::Ref(a, ty) => o.push_str(&format!(
                        "    pub fn to_value(&self) -> Value {{ self.0.to_value() }}\n    \
                         pub fn from_value(v: &Value) -> Result<Self, String> {{ Ok(Self(super::{}::{ty}::from_value(v)?)) }}\n",
                        a.replace('-', "_")
                    )),
                    Kind::Repr(r) => {
                        o.push_str(&format!("    pub fn to_value(&self) -> Value {{ {} }}\n", rust_raw_encode(*r, "self.0")));
                        let dec = rust_raw_decode(*r, "v");
                        o.push_str(&format!(
                            "    pub fn from_value(v: &Value) -> Result<Self, String> {{\n        let x = {{ {dec} }};\n        {}\n    }}\n",
                            if rules.is_empty() { "Ok(Self(x))" } else { "Self::new(x).map_err(|e| e.to_string())" }
                        ));
                    }
                    Kind::Tuple(fs) => {
                        let enc: Vec<String> = fs.iter().enumerate().map(|(i, f)| rust_raw_encode(f.repr, &format!("self.0.{i}"))).collect();
                        o.push_str(&format!("    pub fn to_value(&self) -> Value {{ Value::Array(vec![{}]) }}\n", enc.join(", ")));
                        let dec: Vec<String> = fs.iter().enumerate().map(|(i, f)| format!("{{ {} }}", rust_raw_decode(f.repr, &format!("a[{i}]")))).collect();
                        o.push_str(&format!(
                            "    pub fn from_value(v: &Value) -> Result<Self, String> {{\n        \
                             let a = v.as_array().ok_or(\"expected array\")?;\n        \
                             if a.len() != {} {{ return Err(\"wrong component count for {}\".into()); }}\n        \
                             let x = ({});\n        {}\n    }}\n",
                            fs.len(),
                            t.name,
                            dec.join(", "),
                            if rules.is_empty() { "Ok(Self(x))" } else { "Self::new(x).map_err(|e| e.to_string())" }
                        ));
                    }
                }
            }
            o.push_str("}\n");
            if t.display && matches!(t.kind, Kind::Repr(_)) {
                let body = match t.kind {
                    Kind::Repr(Repr::Bytes) => "for b in &self.0 { write!(f, \"{b:02x}\")?; } Ok(())",
                    _ => "write!(f, \"{}\", self.0)",
                };
                o.push_str(&format!(
                    "impl std::fmt::Display for {} {{\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{ {body} }}\n}}\n",
                    t.name
                ));
            }
            if t.store {
                // A forwarding adapter: the callee stores the T without reading it.
                let out = match &t.kind {
                    Kind::Repr(Repr::String) => "Ok(rusqlite::types::ToSqlOutput::from(self.0.as_str()))".to_string(),
                    Kind::Repr(Repr::Bytes) => "Ok(rusqlite::types::ToSqlOutput::from(self.0.as_slice()))".to_string(),
                    Kind::Repr(_) | Kind::Tuple(_) => "Ok(rusqlite::types::ToSqlOutput::from(self.0))".to_string(),
                    Kind::Ref(..) => "self.0.to_sql()".to_string(),
                };
                o.push_str(&format!(
                    "impl rusqlite::ToSql for {} {{\n    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {{ {out} }}\n}}\n",
                    t.name
                ));
            }
            o.push('\n');
        }
        for k in &self.tokens {
            // Evidence: `mint` instead of `new`, the generator's alone (a
            // consumer calling it is a lint finding). Linear: no `Clone`, so
            // a second use is a compile error.
            let inner = rust_repr(k.repr);
            let versioned = matches!(k.kind, TokenKind::Versioned(_));
            o.push_str(&format!("/// Evidence ({}): only the generator of `{}` mints one.\n", k.kind.word(), self.api));
            if !matches!(k.kind, TokenKind::Linear(_)) {
                o.push_str("#[derive(Clone)]\n");
            }
            if versioned {
                o.push_str(&format!("pub struct {}({inner}, i64);\nimpl {} {{\n    pub fn mint(v: {inner}, version: i64) -> Self {{ Self(v, version) }}\n", k.name, k.name));
            } else {
                o.push_str(&format!("pub struct {}({inner});\nimpl {} {{\n    pub fn mint(v: {inner}) -> Self {{ Self(v) }}\n", k.name, k.name));
            }
            if wire {
                let enc = rust_raw_encode(k.repr, "self.0");
                let bound = match k.repr {
                    Repr::String if k.max_len > 0 => format!(" if x.chars().count() > {} {{ return Err(\"{} longer than {}\".into()); }}", k.max_len, k.name, k.max_len),
                    Repr::Bytes if k.max_len > 0 => format!(" if x.len() > {} {{ return Err(\"{} longer than {}\".into()); }}", k.max_len, k.name, k.max_len),
                    _ => String::new(),
                };
                if versioned {
                    o.push_str(&format!(
                        "    pub fn to_value(&self) -> Value {{ Value::Array(vec![{enc}, json!(self.1)]) }}\n    \
                         pub fn from_value(v: &Value) -> Result<Self, String> {{\n        \
                         let a = v.as_array().filter(|a| a.len() == 2).ok_or(\"expected [id, version]\")?;\n        \
                         let x = {{ {} }};{bound}\n        let version = a[1].as_i64().ok_or(\"expected i64 version\")?;\n        Ok(Self(x, version))\n    }}\n",
                        rust_raw_decode(k.repr, "a[0]")
                    ));
                } else {
                    o.push_str(&format!(
                        "    pub fn to_value(&self) -> Value {{ {enc} }}\n    \
                         pub fn from_value(v: &Value) -> Result<Self, String> {{\n        let x = {{ {} }};{bound}\n        Ok(Self(x))\n    }}\n",
                        rust_raw_decode(k.repr, "v")
                    ));
                }
            }
            if k.store && versioned {
                o.push_str("    /// The version, for the owner's atomic re-check (`where version = ?`).\n    pub fn version(&self) -> rusqlite::types::ToSqlOutput<'_> { rusqlite::types::ToSqlOutput::from(self.1) }\n");
            }
            o.push_str("}\n");
            if k.store {
                let out = match k.repr {
                    Repr::String => "Ok(rusqlite::types::ToSqlOutput::from(self.0.as_str()))",
                    Repr::Bytes => "Ok(rusqlite::types::ToSqlOutput::from(self.0.as_slice()))",
                    _ => "Ok(rusqlite::types::ToSqlOutput::from(self.0))",
                };
                o.push_str(&format!(
                    "impl rusqlite::ToSql for {} {{\n    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {{ {out} }}\n}}\n",
                    k.name
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
                    let fs: Vec<&str> = v.fields.iter().map(|f| rust_repr(f.repr)).collect();
                    o.push_str(&format!("    {}({}),\n", v.name, fs.join(", ")));
                }
            }
            o.push_str("}\n");
            if wire {
                // `{"v": "<Variant>", "f": [raw fields...]}`.
                o.push_str(&format!("\nimpl {} {{\n    pub fn to_value(&self) -> Value {{\n        match self {{\n", e.name));
                for v in &e.variants {
                    let vars: Vec<String> = (0..v.fields.len()).map(|i| format!("f{i}")).collect();
                    let pat = if vars.is_empty() { String::new() } else { format!("({})", vars.join(", ")) };
                    let vals: Vec<String> = v.fields.iter().enumerate().map(|(i, f)| rust_raw_encode(f.repr, &format!("f{i}"))).collect();
                    o.push_str(&format!(
                        "            {}::{}{pat} => json!({{\"v\": \"{}\", \"f\": [{}]}}),\n",
                        e.name, v.name, v.name, vals.join(", ")
                    ));
                }
                o.push_str("        }\n    }\n\n    pub fn from_value(v: &Value) -> Result<Self, String> {\n        \
                     let f = v[\"f\"].as_array().ok_or(\"missing f\")?;\n        match v[\"v\"].as_str().ok_or(\"missing v\")? {\n");
                for v in &e.variants {
                    let n = v.fields.len();
                    let got: Vec<String> = v
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(i, f)| {
                            let dec = rust_raw_decode(f.repr, &format!("f[{i}]"));
                            let bound = match f.repr {
                                Repr::String if f.max_len > 0 => format!(
                                    " let x: String = x; if x.chars().count() > {} {{ return Err(\"{}.{} field {i} longer than {}\".into()); }} x",
                                    f.max_len, e.name, v.name, f.max_len
                                ),
                                Repr::Bytes if f.max_len > 0 => format!(
                                    " let x: Vec<u8> = x; if x.len() > {} {{ return Err(\"{}.{} field {i} longer than {}\".into()); }} x",
                                    f.max_len, e.name, v.name, f.max_len
                                ),
                                _ => " x".to_string(),
                            };
                            format!("{{ let x = {{ {dec} }};{bound} }}")
                        })
                        .collect();
                    let build = if n == 0 { format!("{}::{}", e.name, v.name) } else { format!("{}::{}({})", e.name, v.name, got.join(", ")) };
                    o.push_str(&format!(
                        "            \"{}\" => {{\n                if f.len() != {n} {{ return Err(\"wrong field count for {}\".into()); }}\n                Ok({build})\n            }}\n",
                        v.name, v.name
                    ));
                }
                o.push_str("            other => Err(format!(\"unknown variant {other:?}\")),\n        }\n    }\n}\n");
            }
            o.push('\n');
        }
        for c in &self.containers {
            if let ContainerKind::Of(of, a, t) = &c.kind {
                o.push_str(&format!("pub type {} = super::{}::{t}<{of}>;\n", c.name, a.replace('-', "_")));
                if wire {
                    // A container is a JSON array of elements, walked with `foldr`.
                    let sn = snake(&c.name);
                    o.push_str(&format!(
                        "pub fn {sn}_to_value(x: &{}) -> Value {{\n    \
                         let mut v = x.foldr(|e, mut acc: Vec<Value>| {{ acc.push(e.to_value()); acc }}, Vec::new());\n    \
                         v.reverse();\n    Value::Array(v)\n}}\n\
                         pub fn {sn}_from_value(v: &Value) -> Result<{}, String> {{\n    \
                         let items = v.as_array().ok_or(\"expected array\")?;\n    \
                         let mut out = Vec::new();\n    for i in items {{ out.push({of}::from_value(i)?); }}\n    \
                         Ok({}::new(out))\n}}\n",
                        c.name, c.name, c.name
                    ));
                }
                o.push('\n');
            }
        }
        for (name, ctors) in self.sum_list() {
            self.rust_sum(&name, ctors, wire, &mut o);
        }
        if !self.fns.is_empty() {
            // The call surface: the generator implements it, a consumer
            // receives `&mut dyn Trait` from a composition root.
            o.push_str(&format!("/// The {} call surface.\npub trait {en} {{\n", self.api));
            for f in &self.fns {
                let params = rust_params(&f.inputs);
                o.push_str(&format!("    fn {}(&mut self{}) -> {};\n", snake(&f.name), params, f.returns));
            }
            o.push_str("}\n\n");
            if wire {
                o.push_str(&format!(
                    "/// Serves one call `{{\"fn\": <name>, \"args\": [...]}}`: the reply is the sum's JSON,\n\
                     /// or `{{\"error\": <text>}}`.\n\
                     pub fn dispatch(impl_: &mut dyn {en}, req: &str) -> String {{\n    \
                     fn fail(e: impl std::fmt::Display) -> String {{ json!({{\"error\": e.to_string()}}).to_string() }}\n    \
                     let v: Value = match serde_json::from_str(req) {{ Ok(v) => v, Err(e) => return fail(e) }};\n    \
                     let Some(args) = v[\"args\"].as_array() else {{ return fail(\"missing args\") }};\n    \
                     match v[\"fn\"].as_str() {{\n"
                ));
                for f in &self.fns {
                    let n = f.inputs.len();
                    o.push_str(&format!(
                        "        Some(\"{}\") => {{\n            if args.len() != {n} {{ return fail(\"wrong argument count for {}\"); }}\n",
                        f.name, f.name
                    ));
                    for (i, t) in f.inputs.iter().enumerate() {
                        let dec = self.rust_wire_decode(t, &format!("&args[{i}]"));
                        o.push_str(&format!("            let a{i} = match {dec} {{ Ok(x) => x, Err(e) => return fail(e) }};\n"));
                    }
                    let vars: Vec<String> = (0..n).map(|i| format!("a{i}")).collect();
                    o.push_str(&format!("            impl_.{}({}).to_json()\n        }}\n", snake(&f.name), vars.join(", ")));
                }
                o.push_str("        other => fail(format!(\"unknown fn {other:?}\")),\n    }\n}\n\n");
            }
        }
        o.push_str(&self.rust_laws());
        o
    }

    /// `x.to_value()`, or the container helper for a container argument.
    fn rust_wire_encode(&self, ty: &str, var: &str) -> String {
        if self.is_container(ty) { format!("{}_to_value({var})", snake(ty)) } else { format!("{var}.to_value()") }
    }

    fn rust_wire_decode(&self, ty: &str, var: &str) -> String {
        if self.is_container(ty) { format!("{}_from_value({var})", snake(ty)) } else { format!("{ty}::from_value({var})") }
    }

    /// One sum as a Rust enum, with `to_json`/`from_json` when `wire`.
    fn rust_sum(&self, en: &str, ctors: &[Ctor], wire: bool, o: &mut String) {
        o.push_str(&format!("pub enum {en} {{\n"));
        for c in ctors {
            if c.args.is_empty() {
                o.push_str(&format!("    {},\n", c.name));
            } else {
                o.push_str(&format!("    {}({}),\n", c.name, c.args.join(", ")));
            }
        }
        o.push_str("}\n");
        if wire {
            o.push_str(&format!("\nimpl {en} {{\n    pub fn to_value(&self) -> Value {{\n        match self {{\n"));
            for c in ctors {
                let vars: Vec<String> = (0..c.args.len()).map(|i| format!("a{i}")).collect();
                let pat = if vars.is_empty() { String::new() } else { format!("({})", vars.join(", ")) };
                let vals: Vec<String> = c.args.iter().zip(&vars).map(|(t, v)| self.rust_wire_encode(t, v)).collect();
                o.push_str(&format!(
                    "            {en}::{}{pat} => json!({{\"c\": \"{}\", \"a\": [{}]}}),\n",
                    c.name, c.name, vals.join(", ")
                ));
            }
            o.push_str(&format!(
                "        }}\n    }}\n\n    pub fn to_json(&self) -> String {{ self.to_value().to_string() }}\n\n    \
                 pub fn from_json(s: &str) -> Result<Self, String> {{\n        \
                 let v: Value = serde_json::from_str(s).map_err(|e| e.to_string())?;\n        Self::from_value(&v)\n    }}\n\n    \
                 pub fn from_value(v: &Value) -> Result<Self, String> {{\n        \
                 let args = v[\"a\"].as_array().ok_or(\"missing a\")?;\n        \
                 match v[\"c\"].as_str().ok_or(\"missing c\")? {{\n"
            ));
            for c in ctors {
                let n = c.args.len();
                let got: Vec<String> = c
                    .args
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("{}?", self.rust_wire_decode(t, &format!("&args[{i}]"))))
                    .collect();
                let build = if n == 0 { format!("{en}::{}", c.name) } else { format!("{en}::{}({})", c.name, got.join(", ")) };
                o.push_str(&format!(
                    "            \"{}\" => {{\n                if args.len() != {n} {{ return Err(\"wrong argument count for {}\".into()); }}\n                Ok({build})\n            }}\n",
                    c.name, c.name
                ));
            }
            o.push_str("            other => Err(format!(\"unknown constructor {other:?}\")),\n        }\n    }\n}\n");
        }
        o.push('\n');
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
    /// i64 is a `bigint`, bytes a `Uint8Array` (hex on the wire). `imports`
    /// maps a ref'd API to its module specifier (default `./<api>`).
    pub fn typescript(&self, wire: bool, classes: &BTreeMap<String, Vec<Class>>, imports: &BTreeMap<String, String>) -> String {
        let en = pascal(&self.api);
        let mut o = format!(
            "// @generated by `dashboard arch gen` from the {} IDL. Do not edit.\n\
             // Every T is opaque: a #private field, no accessors.\n",
            self.api
        );
        let mut refs: Vec<&str> = self.refs().into_iter().map(|(a, _)| a).collect();
        refs.sort();
        refs.dedup();
        for a in refs {
            let spec = imports.get(a).cloned().unwrap_or_else(|| format!("./{a}"));
            o.push_str(&format!("import * as {} from \"{spec}\";\n", a.replace('-', "_")));
        }
        o.push('\n');
        self.typescript_containers(wire, classes, &mut o);
        if self.types.iter().any(|t| !self.rules_of(t).is_empty()) {
            o.push_str(VIOLATION_TS);
        }
        for t in &self.types {
            let inner = match &t.kind {
                Kind::Repr(r) => ts_repr(*r).to_string(),
                Kind::Ref(a, ty) => format!("{}.{ty}", a.replace('-', "_")),
                Kind::Tuple(fs) => format!("[{}]", fs.iter().map(|f| ts_repr(f.repr)).collect::<Vec<_>>().join(", ")),
            };
            let guard: String = self
                .rules_of(t)
                .iter()
                .map(|r| format!(" if (!({})) throw new Violation(\"{}\", {});", r.ts, t.name, js_str(&r.text)))
                .collect();
            o.push_str(&format!(
                "export class {} {{\n  readonly #v: {inner};\n  constructor(v: {inner}) {{{guard} this.#v = v; }}\n",
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
                    Kind::Repr(r) => (
                        ts_raw_encode(*r, "this.#v"),
                        format!("return new this({});", ts_raw_decode(*r, 0, "v", &t.name)),
                    ),
                    Kind::Ref(a, ty) => (
                        "this.#v.toValue()".to_string(),
                        format!("return new this({}.{ty}.fromValue(v));", a.replace('-', "_")),
                    ),
                    Kind::Tuple(fs) => (
                        format!("[{}]", fs.iter().enumerate().map(|(i, f)| ts_raw_encode(f.repr, &format!("this.#v[{i}]"))).collect::<Vec<_>>().join(", ")),
                        format!(
                            "if (!Array.isArray(v) || v.length !== {}) throw new Error(\"{}: expected {} components\");\n    return new this([{}]);",
                            fs.len(),
                            t.name,
                            fs.len(),
                            fs.iter().enumerate().map(|(i, f)| ts_raw_decode(f.repr, 0, &format!("v[{i}]"), &format!("{} component {}", t.name, i + 1))).collect::<Vec<_>>().join(", ")
                        ),
                    ),
                };
                o.push_str(&format!(
                    "  toValue(): unknown {{ return {enc}; }}\n  static fromValue(v: unknown): {} {{\n    {dec}\n  }}\n",
                    t.name
                ));
            }
            o.push_str("}\n\n");
        }
        for k in &self.tokens {
            let inner = ts_repr(k.repr);
            let versioned = matches!(k.kind, TokenKind::Versioned(_));
            let (params, args, fields, init) = if versioned {
                (format!("v: {inner}, version: bigint"), "v, version", format!("  readonly #v: {inner};\n  readonly #version: bigint;\n"), "this.#v = v; this.#version = version;")
            } else {
                (format!("v: {inner}"), "v", format!("  readonly #v: {inner};\n"), "this.#v = v;")
            };
            o.push_str(&format!(
                "/** Evidence ({}): only the generator of `{}` mints one; a consumer calling `mint` is a lint finding. */\n\
                 export class {} {{\n{fields}  private constructor({params}) {{ {init} }}\n  static mint({params}): {} {{ return new {}({args}); }}\n",
                k.kind.word(),
                self.api,
                k.name,
                k.name,
                k.name
            ));
            if wire {
                let enc = ts_raw_encode(k.repr, "this.#v");
                if versioned {
                    o.push_str(&format!(
                        "  toValue(): unknown {{ return [{enc}, Number(this.#version)]; }}\n  static fromValue(v: unknown): {} {{\n    \
                         if (!Array.isArray(v) || v.length !== 2) throw new Error(\"{}: expected [id, version]\");\n    \
                         return new this({}, {});\n  }}\n",
                        k.name,
                        k.name,
                        ts_raw_decode(k.repr, k.max_len, "v[0]", &k.name),
                        ts_raw_decode(Repr::I64, 0, "v[1]", &format!("{} version", k.name))
                    ));
                } else {
                    o.push_str(&format!(
                        "  toValue(): unknown {{ return {enc}; }}\n  static fromValue(v: unknown): {} {{\n    return new this({});\n  }}\n",
                        k.name,
                        ts_raw_decode(k.repr, k.max_len, "v", &k.name)
                    ));
                }
            }
            o.push_str("}\n\n");
        }
        for (name, ctors) in self.sum_list() {
            let is_default = name == en;
            self.typescript_sum(&name, ctors, wire, is_default, &mut o);
        }
        if !self.fns.is_empty() {
            let ret = |s: &str| if wire { format!("Promise<{s}>") } else { s.to_string() };
            o.push_str(&format!("/** The {} call surface. */\nexport interface {en} {{\n", self.api));
            for f in &self.fns {
                o.push_str(&format!("  {}({}): {};\n", f.name, ts_params(&f.inputs), ret(&f.returns)));
            }
            o.push_str("}\n\n");
            if wire {
                // The client stub: every method is one call over `send`.
                o.push_str(&format!(
                    "export class {en}Client implements {en} {{\n  constructor(private readonly send: (json: string) => Promise<string>) {{}}\n"
                ));
                for f in &self.fns {
                    let names: Vec<String> = ts_param_names(&f.inputs);
                    let vals: Vec<String> = names.iter().zip(&f.inputs).map(|(n, t)| self.ts_wire_encode(t, n)).collect();
                    let from = if f.returns == en { "fromJson".to_string() } else { format!("{}FromJson", camel(&f.returns)) };
                    o.push_str(&format!(
                        "  async {}({}): Promise<{}> {{\n    return {from}(await this.send(JSON.stringify({{ fn: \"{}\", args: [{}] }})));\n  }}\n",
                        f.name,
                        ts_params(&f.inputs),
                        f.returns,
                        f.name,
                        vals.join(", ")
                    ));
                }
                o.push_str("}\n");
            }
        }
        o
    }

    fn ts_wire_encode(&self, ty: &str, var: &str) -> String {
        if self.is_container(ty) || self.is_sum(ty) { format!("{}ToValue({var})", camel(ty)) } else { format!("{var}.toValue()") }
    }

    fn ts_wire_decode(&self, ty: &str, var: &str) -> String {
        if self.is_container(ty) || self.is_sum(ty) { format!("{}FromValue({var})", camel(ty)) } else { format!("{ty}.fromValue({var})") }
    }

    /// The generic container classes this API's `container ... :=` lines
    /// ref, then those it owns, as `(generated class name, owner api, owner
    /// container)`.
    fn ts_seqs(&self) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = Vec::new();
        for c in &self.containers {
            if let ContainerKind::Of(_, a, t) = &c.kind
                && !out.iter().any(|(_, oa, ot)| oa == a && ot == t)
            {
                out.push((t.clone(), a.clone(), t.clone()));
            }
        }
        for c in &self.containers {
            if matches!(c.kind, ContainerKind::Generic(_)) {
                out.push((c.name.clone(), self.api.clone(), c.name.clone()));
            }
        }
        // Two owners with the same container name: prefix the owner.
        let mut names: Vec<&str> = out.iter().map(|(n, ..)| n.as_str()).collect();
        names.sort();
        names.dedup();
        if names.len() < out.len() {
            for (n, a, t) in &mut out {
                *n = format!("{}{t}", pascal(a));
            }
        }
        out
    }

    /// Elements as discriminated unions of readable records, and each ref'd
    /// generic container as a class with exactly its classes' operations
    /// (`classes`: per owner container, those this consumer may use), the
    /// instantiations as aliases, and (`wire`) their JSON helpers.
    fn typescript_containers(&self, wire: bool, classes: &BTreeMap<String, Vec<Class>>, o: &mut String) {
        for e in &self.elements {
            let variants: Vec<String> = e
                .variants
                .iter()
                .map(|v| {
                    let mut f = format!("{{ v: \"{}\"", v.name);
                    for (i, fl) in v.fields.iter().enumerate() {
                        f.push_str(&format!("; f{i}: {}", ts_repr(fl.repr)));
                    }
                    f + " }"
                })
                .collect();
            o.push_str(&format!("/** A readable element: match every variant. */\nexport type {} =\n  | {};\n\n", e.name, variants.join("\n  | ")));
            if wire {
                let cm = camel(&e.name);
                o.push_str(&format!("export function {cm}ToValue(e: {}): unknown {{\n  switch (e.v) {{\n", e.name));
                for v in &e.variants {
                    let vals: Vec<String> = v.fields.iter().enumerate().map(|(i, f)| ts_raw_encode(f.repr, &format!("e.f{i}"))).collect();
                    o.push_str(&format!("    case \"{}\": return {{ v: \"{}\", f: [{}] }};\n", v.name, v.name, vals.join(", ")));
                }
                // One variant is no union: TypeScript can't narrow it to never.
                if e.variants.len() > 1 {
                    o.push_str("    default: { const never: never = e; throw new Error(String(never)); }\n");
                }
                o.push_str(&format!(
                    "  }}\n}}\n\n\
                     export function {cm}FromValue(x: unknown): {} {{\n  \
                     const j = x as {{ v?: unknown; f?: unknown }};\n  if (!Array.isArray(j.f)) throw new Error(\"missing f\");\n  const f: unknown[] = j.f;\n  switch (j.v) {{\n",
                    e.name
                ));
                for v in &e.variants {
                    let n = v.fields.len();
                    let mut fields = String::new();
                    for (i, fl) in v.fields.iter().enumerate() {
                        fields.push_str(&format!(", f{i}: {}", ts_raw_decode(fl.repr, fl.max_len, &format!("f[{i}]"), &format!("{}.{} field {i}", e.name, v.name))));
                    }
                    o.push_str(&format!(
                        "    case \"{}\":\n      if (f.length !== {n}) throw new Error(\"wrong field count for {}\");\n      return {{ v: \"{}\"{fields} }};\n",
                        v.name, v.name, v.name
                    ));
                }
                o.push_str("    default: throw new Error(`unknown variant ${String(j.v)}`);\n  }\n}\n\n");
            }
        }
        let seqs = self.ts_seqs();
        if seqs.iter().any(|(_, a, t)| classes.get(&format!("{a}.{t}")).is_some_and(|cs| cs.contains(&Class::Traversable))) {
            o.push_str("export type Result<T, E> = { ok: true; value: T } | { ok: false; error: E };\n\n");
        }
        for (name, a, t) in &seqs {
            let cs = classes.get(&format!("{a}.{t}")).cloned().unwrap_or_default();
            let ops: Vec<&str> = cs.iter().map(|c| c.name()).collect();
            o.push_str(&format!(
                "/** {a}.{t}: {} and nothing else (no length, no index, no iteration). */\n\
                 export class {name}<A> {{\n  readonly #items: readonly A[];\n  private constructor(items: readonly A[]) {{ this.#items = items; }}\n  \
                 static new<A>(items: readonly A[]): {name}<A> {{ return new {name}([...items]); }}\n  \
                 foldr<B>(f: (a: A, acc: B) => B, z: B): B {{\n    let acc = z;\n    for (let i = this.#items.length - 1; i >= 0; i--) acc = f(this.#items[i], acc);\n    return acc;\n  }}\n",
                ops.join(", ")
            ));
            if cs.contains(&Class::Functor) {
                o.push_str(&format!("  fmap<B>(f: (a: A) => B): {name}<B> {{ return new {name}(this.#items.map((a) => f(a))); }}\n"));
            }
            if cs.contains(&Class::Traversable) {
                o.push_str(&format!(
                    "  traverse<B, E>(f: (a: A) => Result<B, E>): Result<{name}<B>, E> {{\n    const out: B[] = [];\n    \
                     for (const a of this.#items) {{\n      const r = f(a);\n      if (!r.ok) return r;\n      out.push(r.value);\n    }}\n    \
                     return {{ ok: true, value: new {name}(out) }};\n  }}\n"
                ));
            }
            if cs.contains(&Class::Monoid) {
                o.push_str(&format!(
                    "  static empty<A>(): {name}<A> {{ return new {name}([]); }}\n  \
                     append(other: {name}<A>): {name}<A> {{ return new {name}([...this.#items, ...other.#items]); }}\n  \
                     foldMap<M>(f: (a: A) => M, empty: M, append: (x: M, y: M) => M): M {{ return this.foldr((a, acc) => append(f(a), acc), empty); }}\n"
                ));
            }
            o.push_str("}\n\n");
        }
        for c in &self.containers {
            if let ContainerKind::Of(of, a, t) = &c.kind {
                let seq = &seqs.iter().find(|(_, oa, ot)| oa == a && ot == t).unwrap().0;
                o.push_str(&format!(
                    "export type {} = {seq}<{of}>;\nexport const {} = {{ new: (items: readonly {of}[]): {seq}<{of}> => {seq}.new(items) }};\n",
                    c.name, c.name
                ));
                if wire {
                    let cm = camel(&c.name);
                    o.push_str(&format!(
                        "export function {cm}ToValue(x: {}): unknown {{ return x.foldr((e, acc: unknown[]) => [{}ToValue(e), ...acc], []); }}\n\
                         export function {cm}FromValue(v: unknown): {} {{\n  if (!Array.isArray(v)) throw new Error(\"expected array\");\n  \
                         return {seq}.new(v.map((e) => {}FromValue(e)));\n}}\n",
                        c.name,
                        camel(of),
                        c.name,
                        camel(of)
                    ));
                }
                o.push('\n');
            }
        }
    }

    /// The law tests of every generic container the TypeScript binding
    /// generates (`node:test`), or `None` when it generates none.
    pub fn typescript_laws(&self, classes: &BTreeMap<String, Vec<Class>>) -> Option<String> {
        let seqs = self.ts_seqs();
        if seqs.is_empty() {
            return None;
        }
        let names: Vec<&str> = seqs.iter().map(|(n, ..)| n.as_str()).collect();
        let mut o = format!(
            "// @generated by `dashboard arch gen` from the {} IDL. Do not edit.\n\
             // The class laws of the generated containers.\n\
             import {{ test }} from \"node:test\";\nimport assert from \"node:assert/strict\";\n\
             import {{ {} }} from \"./{}\";\n\n\
             const ints: number[][] = [[], [7], [1, -2, 3], Array.from({{ length: 40 }}, (_, i) => ((i * 7919) % 101) - 50)];\n\
             const strs: string[][] = ints.map((v) => v.map((x) => `s${{x}}`));\n\
             const tree = (x: unknown, acc: string): string => `(${{String(x)}} ${{acc}})`;\n\n",
            self.api,
            names.join(", "),
            self.api.replace('-', "_")
        );
        for (name, a, t) in &seqs {
            let cs = classes.get(&format!("{a}.{t}")).cloned().unwrap_or_default();
            o.push_str(&format!(
                "const items{name} = <A>(s: {name}<A>): A[] => s.foldr((x, acc: A[]) => [x, ...acc], []);\n\n\
                 test(\"foldable {name} laws\", () => {{\n  const law = <A>(xs: A[]) => {{\n    \
                 const s = {name}.new(xs);\n    assert.deepEqual(items{name}(s), xs);\n    \
                 const want = xs.slice().reverse().reduce<string>((acc, x) => tree(x, acc), \"\");\n    \
                 assert.equal(s.foldr(tree, \"\"), want);\n  }};\n  for (const xs of ints) law(xs);\n  for (const xs of strs) law(xs);\n}});\n\n"
            ));
            if cs.contains(&Class::Functor) {
                o.push_str(&format!(
                    "test(\"functor {name} laws\", () => {{\n  const f = (x: number) => x * 3 + 1;\n  const g = (x: number) => x ^ 5;\n  \
                     for (const xs of ints) {{\n    const s = {name}.new(xs);\n    \
                     assert.deepEqual(items{name}(s.fmap((x) => x)), xs);\n    \
                     assert.deepEqual(items{name}(s.fmap((x) => g(f(x)))), items{name}(s.fmap(f).fmap(g)));\n    \
                     assert.equal(s.fmap(g).foldr(tree, \"\"), s.foldr((x, acc) => tree(g(x), acc), \"\"));\n  }}\n}});\n\n"
                ));
            }
            if cs.contains(&Class::Traversable) {
                o.push_str(&format!(
                    "test(\"traversable {name} laws\", () => {{\n  const f = (x: number) => x * 3 + 1;\n  const bad = (x: number) => x % 3 === 0;\n  \
                     for (const xs of ints) {{\n    const s = {name}.new(xs);\n    \
                     const id = s.traverse((x) => ({{ ok: true, value: x }}) as Result<number, never>);\n    \
                     assert.ok(id.ok && JSON.stringify(items{name}(id.value)) === JSON.stringify(xs));\n    \
                     const step = (x: number): Result<number, number> => (bad(x) ? {{ ok: false, error: x }} : {{ ok: true, value: f(x) }});\n    \
                     const got = s.traverse(step);\n    const first = xs.find(bad);\n    \
                     if (first === undefined) assert.ok(got.ok && JSON.stringify(items{name}(got.value)) === JSON.stringify(xs.map(f)));\n    \
                     else assert.ok(!got.ok && got.error === first);\n  }}\n}});\n\n"
                ));
            }
            if cs.contains(&Class::Monoid) {
                o.push_str(&format!(
                    "test(\"monoid {name} laws\", () => {{\n  for (const xs of ints) {{\n    const n = (v: number[]) => {name}.new(v);\n    \
                     assert.deepEqual(items{name}({name}.empty<number>().append(n(xs))), xs);\n    \
                     assert.deepEqual(items{name}(n(xs).append({name}.empty())), xs);\n    \
                     for (const ys of ints) {{\n      assert.deepEqual(items{name}(n(xs).append(n(ys))), [...xs, ...ys]);\n      \
                     for (const zs of ints) assert.deepEqual(items{name}(n(xs).append(n(ys)).append(n(zs))), items{name}(n(xs).append(n(ys).append(n(zs)))));\n    }}\n    \
                     assert.deepEqual(items{name}(n(xs).foldMap((x) => n([x]), {name}.empty<number>(), (a, b) => a.append(b))), xs);\n  }}\n}});\n\n"
                ));
            }
        }
        Some(o)
    }

    /// One sum as a tagged union with a constructor object, and (`wire`)
    /// `toJson`/`fromJson` (the default sum) or `<sum>ToJson`/`<sum>FromJson`.
    fn typescript_sum(&self, en: &str, ctors: &[Ctor], wire: bool, is_default: bool, o: &mut String) {
        let variants: Vec<String> = ctors
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
        for c in ctors {
            let params: Vec<String> = c.args.iter().enumerate().map(|(i, a)| format!("a{i}: {a}")).collect();
            let mut fields = format!("kind: \"{}\"", c.name);
            for i in 0..c.args.len() {
                fields.push_str(&format!(", a{i}"));
            }
            o.push_str(&format!("  {}: ({}): {en} => ({{ {fields} }}),\n", c.name, params.join(", ")));
        }
        o.push_str("};\n");
        if wire {
            let (to, from, to_v, from_v) = if is_default {
                ("toJson".to_string(), "fromJson".to_string(), "toValue".to_string(), "fromValue".to_string())
            } else {
                (format!("{}ToJson", camel(en)), format!("{}FromJson", camel(en)), format!("{}ToValue", camel(en)), format!("{}FromValue", camel(en)))
            };
            o.push_str(&format!("\nexport function {to_v}(v: {en}): unknown {{\n  switch (v.kind) {{\n"));
            for c in ctors {
                let vals: Vec<String> = c.args.iter().enumerate().map(|(i, t)| self.ts_wire_encode(t, &format!("v.a{i}"))).collect();
                o.push_str(&format!(
                    "    case \"{}\": return {{ c: \"{}\", a: [{}] }};\n",
                    c.name, c.name, vals.join(", ")
                ));
            }
            // One constructor is no union: TypeScript can't narrow it to never.
            if ctors.len() > 1 {
                o.push_str("    default: { const never: never = v; throw new Error(String(never)); }\n");
            }
            o.push_str(&format!(
                "  }}\n}}\n\nexport function {to}(v: {en}): string {{ return JSON.stringify({to_v}(v)); }}\n\n\
                 export function {from}(s: string): {en} {{ return {from_v}(JSON.parse(s)); }}\n\n\
                 export function {from_v}(x: unknown): {en} {{\n  const j = x as {{ c?: unknown; a?: unknown; error?: unknown }};\n  \
                 if (typeof j.error === \"string\") throw new Error(j.error);\n  \
                 if (!Array.isArray(j.a)) throw new Error(\"missing a\");\n  const a: unknown[] = j.a;\n  switch (j.c) {{\n"
            ));
            for c in ctors {
                let n = c.args.len();
                let args: Vec<String> = c.args.iter().enumerate().map(|(i, t)| self.ts_wire_decode(t, &format!("a[{i}]"))).collect();
                o.push_str(&format!(
                    "    case \"{}\":\n      if (a.length !== {n}) throw new Error(\"wrong argument count for {}\");\n      return {en}.{}({});\n",
                    c.name, c.name, c.name, args.join(", ")
                ));
            }
            o.push_str("    default: throw new Error(`unknown constructor ${String(j.c)}`);\n  }\n}\n");
        }
        o.push('\n');
    }

    /// Generated Kotlin: package `api.<name>`; a `sealed interface` per
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
        if self.types.iter().any(|t| !self.rules_of(t).is_empty()) {
            o.push_str(VIOLATION_KT);
        }
        for t in &self.types {
            let rules = self.rules_of(t);
            let guard: String = rules
                .iter()
                .map(|r| format!("    init {{ if (!({})) throw Violation(\"{}\", {}) }}\n", r.kotlin, t.name, js_str(&r.text)))
                .collect();
            if let Kind::Tuple(fs) = &t.kind {
                // A product is the components themselves: no tuple type needed.
                let params: Vec<String> = fs.iter().enumerate().map(|(i, f)| format!("private val v{}: {}", i + 1, kotlin_repr(f.repr))).collect();
                o.push_str(&format!("class {}({}) {{\n{guard}", t.name, params.join(", ")));
                if wire {
                    let enc: Vec<String> = fs.iter().enumerate().map(|(i, f)| kotlin_raw_encode(f.repr, &format!("v{}", i + 1))).collect();
                    let dec: Vec<String> = fs.iter().enumerate().map(|(i, f)| kotlin_raw_decode(f.repr, &format!("a[{i}]"))).collect();
                    o.push_str(&format!(
                        "    internal fun toValue(): Any? = listOf<Any?>({})\n    companion object {{\n        internal fun fromValue(x: Any?): {} {{\n            \
                         val a = x as? List<*> ?: throw IllegalArgumentException(\"expected list\")\n            \
                         require(a.size == {}) {{ \"wrong component count for {}\" }}\n            return {}({})\n        }}\n    }}\n",
                        enc.join(", "),
                        t.name,
                        fs.len(),
                        t.name,
                        t.name,
                        dec.join(", ")
                    ));
                }
                o.push_str("}\n\n");
                continue;
            }
            let (inner, enc, dec) = match &t.kind {
                Kind::Repr(r) => (kotlin_repr(*r).to_string(), kotlin_raw_encode(*r, "v"), kotlin_raw_decode(*r, "x")),
                Kind::Ref(a, ty) => (format!("{}.{ty}", kotlin_pkg(a)), "v.toValue()".to_string(), format!("{}.{ty}.fromValue(x)", kotlin_pkg(a))),
                Kind::Tuple(_) => unreachable!(),
            };
            o.push_str(&format!("class {}(private val v: {inner}) {{\n{guard}", t.name));
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
        for k in &self.tokens {
            let inner = kotlin_repr(k.repr);
            let versioned = matches!(k.kind, TokenKind::Versioned(_));
            let (params, args) = if versioned {
                (format!("private val v: {inner}, private val version: Long"), "v, version")
            } else {
                (format!("private val v: {inner}"), "v")
            };
            let mint_params = params.replace("private val ", "");
            o.push_str(&format!(
                "/** Evidence ({}): only the generator of `{}` mints one; a consumer calling `mint` is a lint finding. */\n\
                 class {} private constructor({params}) {{\n",
                k.kind.word(),
                self.api,
                k.name
            ));
            let bound = match k.repr {
                Repr::String if k.max_len > 0 => format!("    init {{ require(v.codePointCount(0, v.length) <= {}) {{ \"{} longer than {}\" }} }}\n", k.max_len, k.name, k.max_len),
                Repr::Bytes if k.max_len > 0 => format!("    init {{ require(v.size <= {}) {{ \"{} longer than {}\" }} }}\n", k.max_len, k.name, k.max_len),
                _ => String::new(),
            };
            o.push_str(&bound);
            if wire {
                let enc = kotlin_raw_encode(k.repr, "v");
                if versioned {
                    o.push_str(&format!("    internal fun toValue(): Any? = listOf<Any?>({enc}, version)\n"));
                } else {
                    o.push_str(&format!("    internal fun toValue(): Any? = {enc}\n"));
                }
            }
            o.push_str(&format!("    companion object {{\n        fun mint({mint_params}): {} = {}({args})\n", k.name, k.name));
            if wire {
                if versioned {
                    o.push_str(&format!(
                        "        internal fun fromValue(x: Any?): {} {{\n            \
                         val a = x as? List<*> ?: throw IllegalArgumentException(\"expected [id, version]\")\n            \
                         require(a.size == 2) {{ \"expected [id, version]\" }}\n            return {}({}, {})\n        }}\n",
                        k.name,
                        k.name,
                        kotlin_raw_decode(k.repr, "a[0]"),
                        kotlin_raw_decode(Repr::I64, "a[1]")
                    ));
                } else {
                    o.push_str(&format!("        internal fun fromValue(x: Any?): {} = {}({})\n", k.name, k.name, kotlin_raw_decode(k.repr, "x")));
                }
            }
            o.push_str("    }\n}\n\n");
        }
        for (name, ctors) in self.sum_list() {
            let is_default = name == en;
            o.push_str(&format!("sealed interface {name} {{\n"));
            for c in ctors {
                if c.args.is_empty() {
                    o.push_str(&format!("    object {} : {name}\n", c.name));
                } else {
                    let ps: Vec<String> = c.args.iter().enumerate().map(|(i, a)| format!("val a{i}: {a}")).collect();
                    o.push_str(&format!("    class {}({}) : {name}\n", c.name, ps.join(", ")));
                }
            }
            o.push_str("}\n");
            if wire {
                // Value-level codecs too: a sum may be another sum's argument.
                let from = if is_default { "fromJson".to_string() } else { format!("fromJson{name}") };
                o.push_str(&format!(
                    "\nfun toJson(v: {name}): String = Wire.write(toValue(v))\n\n\
                     fun {from}(s: String): {name} {{\n    val j = Wire.read(s)\n    \
                     ((j as? Map<*, *>)?.get(\"error\") as? String)?.let {{ throw IllegalArgumentException(it) }}\n    \
                     return fromValue{name}(j)\n}}\n\n\
                     internal fun toValue(v: {name}): Any? = when (v) {{\n"
                ));
                for c in ctors {
                    let vals: Vec<String> = c
                        .args
                        .iter()
                        .enumerate()
                        .map(|(i, t)| if self.is_sum(t) { format!("toValue(v.a{i})") } else { format!("v.a{i}.toValue()") })
                        .collect();
                    let pat = if c.args.is_empty() { format!("{name}.{}", c.name) } else { format!("is {name}.{}", c.name) };
                    o.push_str(&format!(
                        "    {pat} -> mapOf(\"c\" to \"{}\", \"a\" to listOf<Any?>({}))\n",
                        c.name, vals.join(", ")
                    ));
                }
                o.push_str(&format!(
                    "}}\n\ninternal fun fromValue{name}(x: Any?): {name} {{\n    val j = x as? Map<*, *> ?: throw IllegalArgumentException(\"expected object\")\n    \
                     val a = j[\"a\"] as? List<*> ?: throw IllegalArgumentException(\"missing a\")\n    return when (val c = j[\"c\"]) {{\n"
                ));
                for c in ctors {
                    let n = c.args.len();
                    let args: Vec<String> = c
                        .args
                        .iter()
                        .enumerate()
                        .map(|(i, t)| if self.is_sum(t) { format!("fromValue{t}(a[{i}])") } else { format!("{t}.fromValue(a[{i}])") })
                        .collect();
                    let build = if n == 0 { format!("{name}.{}", c.name) } else { format!("{name}.{}({})", c.name, args.join(", ")) };
                    o.push_str(&format!(
                        "        \"{}\" -> {{\n            require(a.size == {n}) {{ \"wrong argument count for {}\" }}\n            {build}\n        }}\n",
                        c.name, c.name
                    ));
                }
                o.push_str("        else -> throw IllegalArgumentException(\"unknown constructor $c\")\n    }\n}\n");
            }
            o.push('\n');
        }
        if !self.fns.is_empty() {
            o.push_str(&format!("/** The {} call surface. */\ninterface {en} {{\n", self.api));
            for f in &self.fns {
                o.push_str(&format!("    fun {}({}): {}\n", f.name, kotlin_params(&f.inputs), f.returns));
            }
            o.push_str("}\n\n");
        }
        if wire {
            o.push_str(include_str!("kotlin_wire.kt"));
        }
        o
    }
}

/// `json!(<expr>)` / hex for a raw value of repr `r`.
fn rust_raw_encode(r: Repr, expr: &str) -> String {
    match r {
        Repr::Bytes => format!("Value::String({expr}.iter().map(|b| format!(\"{{b:02x}}\")).collect())"),
        _ => format!("json!({expr})"),
    }
}

/// An expression decoding the `&Value` `v` into a raw value of repr `r`
/// (`Result`-propagating with `?`).
fn rust_raw_decode(r: Repr, v: &str) -> String {
    match r {
        Repr::Bytes => format!(
            "let s = {v}.as_str().ok_or(\"expected hex string\")?;\n        \
             if s.len() % 2 != 0 {{ return Err(\"odd hex length\".into()); }}\n        \
             let mut b = Vec::new();\n        \
             for i in (0..s.len()).step_by(2) {{ b.push(u8::from_str_radix(s.get(i..i + 2).ok_or(\"bad hex\")?, 16).map_err(|e| e.to_string())?); }}\n        \
             b"
        ),
        Repr::String => format!("{v}.as_str().ok_or(\"expected string\")?.to_string()"),
        Repr::I64 => format!("{v}.as_i64().ok_or(\"expected i64\")?"),
        Repr::F64 => format!("{v}.as_f64().ok_or(\"expected f64\")?"),
        Repr::Bool => format!("{v}.as_bool().ok_or(\"expected bool\")?"),
    }
}

fn kotlin_repr(r: Repr) -> &'static str {
    match r {
        Repr::String => "String",
        Repr::Bytes => "ByteArray",
        Repr::I64 => "Long",
        Repr::F64 => "Double",
        Repr::Bool => "Boolean",
    }
}

fn kotlin_raw_encode(r: Repr, expr: &str) -> String {
    match r {
        Repr::Bytes => format!("{expr}.joinToString(\"\") {{ \"%02x\".format(it) }}"),
        _ => expr.to_string(),
    }
}

/// An expression decoding the `Any?` `x` into a raw value of repr `r`.
fn kotlin_raw_decode(r: Repr, x: &str) -> String {
    match r {
        Repr::String => format!("{x} as? String ?: throw IllegalArgumentException(\"expected string\")"),
        Repr::Bytes => format!(
            "({x} as? String)?.takeIf {{ it.length % 2 == 0 && it.all {{ c -> c in \"0123456789abcdef\" }} }}?.chunked(2)?.map {{ it.toInt(16).toByte() }}?.toByteArray() ?: throw IllegalArgumentException(\"expected hex string\")"
        ),
        Repr::I64 => format!("{x} as? Long ?: throw IllegalArgumentException(\"expected integer\")"),
        Repr::F64 => format!("({x} as? Number)?.toDouble() ?: throw IllegalArgumentException(\"expected number\")"),
        Repr::Bool => format!("{x} as? Boolean ?: throw IllegalArgumentException(\"expected boolean\")"),
    }
}

/// A string literal valid in TypeScript and Kotlin (JSON escaping).
fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// One check of a smart constructor, rendered for each language, and the
/// rule text a `Violation` carries.
struct Rule {
    text: String,
    rust: String,
    ts: String,
    kotlin: String,
}

const VIOLATION_RS: &str = "\
/// A value that breaks a rule of the IDL: the type and the rule, as written.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub type_name: &'static str,
    pub rule: &'static str,
}
impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, \"{}: {}\", self.type_name, self.rule) }
}
impl std::error::Error for Violation {}

";

const VIOLATION_TS: &str = "\
/** A value that breaks a rule of the IDL: the type and the rule, as written. */
export class Violation extends Error {
  constructor(readonly typeName: string, readonly rule: string) { super(`${typeName}: ${rule}`); this.name = \"Violation\"; }
}

";

const VIOLATION_KT: &str = "\
/** A value that breaks a rule of the IDL: the type and the rule, as written. */
class Violation(val typeName: String, val rule: String) : IllegalArgumentException(\"$typeName: $rule\")

";

/// What an operand reads, so the literal on the other side renders in the
/// language's type (`20000.0`, `20000n`, `20000L`).
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Repr(Repr),
    Len,
}

impl Atom {
    fn slot(&self, comps: &[Field]) -> Option<Slot> {
        match self {
            Atom::V => comps.first().map(|f| Slot::Repr(f.repr)),
            Atom::Field(i) => comps.get((*i as usize).saturating_sub(1)).map(|f| Slot::Repr(f.repr)),
            Atom::Len => Some(Slot::Len),
            _ => None,
        }
    }

    fn is_literal(&self) -> bool {
        matches!(self, Atom::Num(_) | Atom::Str(_) | Atom::Bool(_))
    }

    fn check(&self, comps: &[Field]) -> Result<(), String> {
        match self {
            Atom::V if comps.len() != 1 => Err("a product is compared by component: v.1, v.2, …".into()),
            Atom::Field(i) if comps.len() < 2 => Err(format!("v.{i} is for a product type")),
            Atom::Field(i) if *i == 0 || *i as usize > comps.len() => Err(format!("v.{i}: the product has {} components", comps.len())),
            Atom::Len if comps.len() != 1 || !matches!(comps[0].repr, Repr::String | Repr::Bytes) => {
                Err("`.length` is for a string or bytes type".into())
            }
            _ => Ok(()),
        }
    }

    /// The Lean text.
    fn render(&self) -> String {
        match self {
            Atom::V => "v".into(),
            Atom::Field(i) => format!("v.{i}"),
            Atom::Len => "v.length".into(),
            Atom::Num(t) => t.clone(),
            Atom::Str(s) => format!("{s:?}"),
            Atom::Bool(b) => b.to_string(),
        }
    }

    /// `v.length` of a scalar, or `v.N.length` of a product's component.
    fn len_text(at: Option<usize>) -> String {
        match at {
            Some(i) => format!("v.{}.length", i + 1),
            None => "v.length".into(),
        }
    }

    fn rust_len(r: Repr, at: Option<usize>) -> String {
        let base = match at {
            Some(i) => format!("v.{i}"),
            None => "v".into(),
        };
        match r {
            Repr::String => format!("{base}.chars().count()"),
            _ => format!("{base}.len()"),
        }
    }

    fn ts_len(r: Repr, at: Option<usize>) -> String {
        let base = match at {
            Some(i) => format!("v[{i}]"),
            None => "v".into(),
        };
        match r {
            Repr::String => format!("[...{base}].length"),
            _ => format!("{base}.length"),
        }
    }

    fn kotlin_len(r: Repr, at: Option<usize>) -> String {
        let base = match at {
            Some(i) => format!("v{}", i + 1),
            None => "v".into(),
        };
        match r {
            Repr::String => format!("{base}.codePointCount(0, {base}.length)"),
            _ => format!("{base}.size"),
        }
    }

    fn rust(&self, comps: &[Field], slot: Option<Slot>) -> String {
        let read = |base: String, r: Repr| match r {
            Repr::String => format!("{base}.as_str()"),
            Repr::Bytes => format!("{base}.as_slice()"),
            _ => base,
        };
        match self {
            Atom::V => read("v".into(), comps[0].repr),
            Atom::Field(i) => read(format!("v.{}", i - 1), comps[*i as usize - 1].repr),
            Atom::Len => Atom::rust_len(comps[0].repr, None),
            Atom::Num(t) => match slot {
                Some(Slot::Repr(Repr::F64)) if !t.contains(['.', 'e', 'E']) => format!("{t}.0"),
                _ => t.clone(),
            },
            Atom::Str(s) => format!("{s:?}"),
            Atom::Bool(b) => b.to_string(),
        }
    }

    fn ts(&self, comps: &[Field], slot: Option<Slot>) -> String {
        match self {
            Atom::V => "v".into(),
            Atom::Field(i) => format!("v[{}]", i - 1),
            Atom::Len => Atom::ts_len(comps[0].repr, None),
            Atom::Num(t) => match slot {
                Some(Slot::Repr(Repr::I64)) => format!("{t}n"),
                _ => t.clone(),
            },
            Atom::Str(s) => js_str(s),
            Atom::Bool(b) => b.to_string(),
        }
    }

    fn kotlin(&self, comps: &[Field], slot: Option<Slot>) -> String {
        match self {
            Atom::V => "v".into(),
            Atom::Field(i) => format!("v{i}"),
            Atom::Len => Atom::kotlin_len(comps[0].repr, None),
            Atom::Num(t) => match slot {
                Some(Slot::Repr(Repr::I64)) => format!("{t}L"),
                Some(Slot::Repr(Repr::F64)) if !t.contains(['.', 'e', 'E']) => format!("{t}.0"),
                _ => t.clone(),
            },
            Atom::Str(s) => js_str(s),
            Atom::Bool(b) => b.to_string(),
        }
    }
}

impl Op {
    fn text(self) -> &'static str {
        match self {
            Op::Lt => "<",
            Op::Le => "≤",
            Op::Eq => "=",
            Op::Ne => "≠",
        }
    }

    /// `(Rust, TypeScript, Kotlin)` operators.
    fn code(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Op::Lt => ("<", "<", "<"),
            Op::Le => ("<=", "<=", "<="),
            Op::Eq => ("==", "===", "=="),
            Op::Ne => ("!=", "!==", "!="),
        }
    }
}

impl Pred {
    /// Comparisons and memberships in the predicate (the `max_rules` measure).
    pub fn leaves(&self) -> u64 {
        match self {
            Pred::Cmp(..) | Pred::Mem(..) => 1,
            Pred::And(p, q) | Pred::Or(p, q) => p.leaves() + q.leaves(),
            Pred::Not(p) => p.leaves(),
        }
    }

    /// Structural checks against the type's components (Lean has already
    /// type-checked the predicate; an export is trusted but not blindly).
    pub(crate) fn check(&self, comps: &[Field]) -> Result<(), String> {
        match self {
            Pred::Cmp(_, l, r) => {
                l.check(comps)?;
                r.check(comps)
            }
            Pred::Mem(a, ls) => {
                a.check(comps)?;
                if a.is_literal() || ls.is_empty() || !ls.iter().all(Atom::is_literal) {
                    return Err("`∈` compares v with a non-empty list of literals".into());
                }
                Ok(())
            }
            Pred::And(p, q) | Pred::Or(p, q) => {
                p.check(comps)?;
                q.check(comps)
            }
            Pred::Not(p) => p.check(comps),
        }
    }

    /// The predicate as written in the IDL (and as `Idl::lean` prints it).
    pub fn render(&self) -> String {
        self.render_at(0)
    }

    fn prec(&self) -> u32 {
        match self {
            Pred::Cmp(..) | Pred::Mem(..) => 50,
            Pred::And(..) => 35,
            Pred::Or(..) => 30,
            Pred::Not(_) => 1024,
        }
    }

    fn render_at(&self, min: u32) -> String {
        let s = match self {
            Pred::Cmp(op, l, r) => format!("{} {} {}", l.render(), op.text(), r.render()),
            Pred::Mem(a, ls) => format!("{} ∈ [{}]", a.render(), ls.iter().map(Atom::render).collect::<Vec<_>>().join(", ")),
            Pred::And(p, q) => format!("{} ∧ {}", p.render_at(36), q.render_at(35)),
            Pred::Or(p, q) => format!("{} ∨ {}", p.render_at(31), q.render_at(30)),
            Pred::Not(p) => format!("¬({})", p.render()),
        };
        if self.prec() < min { format!("({s})") } else { s }
    }

    fn code(&self, comps: &[Field], atom: &dyn std::ops::Fn(&Atom, Option<Slot>) -> String, ops: fn(Op) -> &'static str, mem: &dyn std::ops::Fn(String, Vec<String>) -> String, and: &str, or: &str) -> String {
        match self {
            Pred::Cmp(op, l, r) => {
                let slot = l.slot(comps).or_else(|| r.slot(comps));
                format!("({} {} {})", atom(l, slot), ops(*op), atom(r, slot))
            }
            Pred::Mem(a, ls) => {
                let slot = a.slot(comps);
                mem(atom(a, slot), ls.iter().map(|l| atom(l, slot)).collect())
            }
            Pred::And(p, q) => format!("({} {and} {})", p.code(comps, atom, ops, mem, and, or), q.code(comps, atom, ops, mem, and, or)),
            Pred::Or(p, q) => format!("({} {or} {})", p.code(comps, atom, ops, mem, and, or), q.code(comps, atom, ops, mem, and, or)),
            Pred::Not(p) => format!("(!{})", p.code(comps, atom, ops, mem, and, or)),
        }
    }

    /// The check as a Rust expression over `v`.
    pub fn rust(&self, comps: &[Field]) -> String {
        self.code(comps, &|a, s| a.rust(comps, s), |o| o.code().0, &|a, ls| format!("[{}].contains(&{a})", ls.join(", ")), "&&", "||")
    }

    /// The check as a TypeScript expression over `v`.
    pub fn ts(&self, comps: &[Field]) -> String {
        self.code(comps, &|a, s| a.ts(comps, s), |o| o.code().1, &|a, ls| format!("[{}].includes({a})", ls.join(", ")), "&&", "||")
    }

    /// The check as a Kotlin expression over `v` (`v1`, `v2`… of a product).
    pub fn kotlin(&self, comps: &[Field]) -> String {
        self.code(comps, &|a, s| a.kotlin(comps, s), |o| o.code().2, &|a, ls| format!("({a} in listOf({}))", ls.join(", ")), "&&", "||")
    }
}

fn ts_repr(r: Repr) -> &'static str {
    match r {
        Repr::String => "string",
        Repr::Bytes => "Uint8Array",
        Repr::I64 => "bigint",
        Repr::F64 => "number",
        Repr::Bool => "boolean",
    }
}

fn ts_raw_encode(r: Repr, expr: &str) -> String {
    match r {
        Repr::Bytes => format!("Array.from({expr}, (b) => b.toString(16).padStart(2, \"0\")).join(\"\")"),
        Repr::I64 => format!("Number({expr})"),
        _ => expr.to_string(),
    }
}

/// An expression decoding the unknown `v` into a raw value of repr `r`,
/// checking `max_len` (0: none); `what` names the field in errors.
fn ts_raw_decode(r: Repr, max_len: u64, v: &str, what: &str) -> String {
    let bound = |len: &str| if max_len > 0 { format!(" if ({len} > {max_len}) throw new Error(\"{what} longer than {max_len}\");") } else { String::new() };
    match r {
        Repr::String => format!("((x) => {{ if (typeof x !== \"string\") throw new Error(\"{what}: expected string\");{} return x; }})({v})", bound("[...x].length")),
        Repr::Bytes => format!(
            "((x) => {{ if (typeof x !== \"string\" || !/^([0-9a-f]{{2}})*$/.test(x)) throw new Error(\"{what}: expected hex string\"); \
             const b = Uint8Array.from(x.match(/../g) ?? [], (h) => parseInt(h, 16));{} return b; }})({v})",
            bound("b.length")
        ),
        Repr::I64 => format!("((x) => {{ if (typeof x !== \"number\" || !Number.isSafeInteger(x)) throw new Error(\"{what}: expected a safe integer\"); return BigInt(x); }})({v})"),
        Repr::F64 => format!("((x) => {{ if (typeof x !== \"number\") throw new Error(\"{what}: expected number\"); return x; }})({v})"),
        Repr::Bool => format!("((x) => {{ if (typeof x !== \"boolean\") throw new Error(\"{what}: expected boolean\"); return x; }})({v})"),
    }
}

/// Parameter names for a fn's inputs: the type's snake case, numbered when
/// a type repeats.
fn param_names(inputs: &[String], case: fn(&str) -> String) -> Vec<String> {
    let mut out = Vec::new();
    for (i, t) in inputs.iter().enumerate() {
        let base = case(t);
        let dup = inputs.iter().filter(|x| *x == t).count() > 1;
        out.push(if dup { format!("{base}{i}") } else { base });
    }
    out
}

fn rust_params(inputs: &[String]) -> String {
    param_names(inputs, snake).iter().zip(inputs).map(|(n, t)| format!(", {n}: {t}")).collect()
}

fn ts_param_names(inputs: &[String]) -> Vec<String> {
    param_names(inputs, camel)
}

fn ts_params(inputs: &[String]) -> String {
    ts_param_names(inputs).iter().zip(inputs).map(|(n, t)| format!("{n}: {t}")).collect::<Vec<_>>().join(", ")
}

fn kotlin_params(inputs: &[String]) -> String {
    ts_params(inputs)
}

/// `Written` -> `written`, `insert_product` -> `insertProduct`.
fn camel(s: &str) -> String {
    let p = pascal(s);
    let mut c = p.chars();
    match c.next() {
        Some(f) => f.to_ascii_lowercase().to_string() + c.as_str(),
        None => String::new(),
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

fn lean_ident(name: &str) -> String {
    crate::lean::ident(name)
}

fn lean_idents(names: &[String]) -> Vec<String> {
    names.iter().map(|n| lean_ident(n)).collect()
}

fn lean_prim(repr: Repr, max_len: u64) -> String {
    match (repr, max_len) {
        (Repr::String, 0) => "string".to_string(),
        (Repr::String, n) => format!("string {n}"),
        (Repr::Bytes, 0) => "bytes".to_string(),
        (Repr::Bytes, n) => format!("bytes {n}"),
        (Repr::I64, _) => "i64".to_string(),
        (Repr::F64, _) => "f64".to_string(),
        (Repr::Bool, _) => "bool".to_string(),
    }
}

fn lean_ctor(c: &Ctor) -> String {
    if c.args.is_empty() {
        format!("  | {}\n", lean_ident(&c.name))
    } else {
        format!("  | {} : {}\n", lean_ident(&c.name), lean_idents(&c.args).join(" → "))
    }
}

impl Idl {
    /// The IDL as `architecture/Apis/<api>.lean` (`dashboard arch migrate`),
    /// in the Lean DSL: the same declarations in the same order. Refs to
    /// other APIs become imports of their files.
    pub fn lean(&self) -> String {
        let mut imports: Vec<&str> = self.refs().into_iter().map(|(a, _)| a).collect();
        imports.extend(self.container_refs().into_iter().map(|(a, _)| a));
        imports.sort();
        imports.dedup();
        let mut o = String::from("import Architecture\n");
        for a in imports {
            o.push_str(&format!("import Apis.«{a}»\n"));
        }
        o.push_str(&format!("open Dashboard\n\nidl {} where\n", lean_ident(&self.api)));
        for t in &self.types {
            match &t.kind {
                Kind::Repr(r) => o.push_str(&format!("  type {} : {}", lean_ident(&t.name), lean_prim(*r, t.max_len))),
                Kind::Tuple(fs) => o.push_str(&format!(
                    "  type {} : {}",
                    lean_ident(&t.name),
                    fs.iter().map(|f| lean_prim(f.repr, f.max_len)).collect::<Vec<_>>().join(" × ")
                )),
                Kind::Ref(a, ty) => {
                    o.push_str(&format!("  type {} : ref {}.{}\n", lean_ident(&t.name), lean_ident(a), lean_ident(ty)));
                    continue;
                }
            }
            if t.display {
                o.push_str(" display");
            }
            if t.store {
                o.push_str(" store");
            }
            if let Some(p) = &t.pred {
                o.push_str(&format!(" where {}", p.render()));
            }
            o.push('\n');
        }
        for k in &self.tokens {
            let kind = match &k.kind {
                TokenKind::Permanent => "permanent".to_string(),
                TokenKind::Versioned(c) => format!("versioned stale={}", lean_ident(c)),
                TokenKind::Linear(c) => format!("linear spent={}", lean_ident(c)),
            };
            o.push_str(&format!("  token {} : {} {kind}", lean_ident(&k.name), lean_prim(k.repr, k.max_len)));
            if k.store {
                o.push_str(" store");
            }
            o.push('\n');
        }
        for e in &self.elements {
            o.push_str(&format!("  element {}\n", lean_ident(&e.name)));
            for v in &e.variants {
                if v.fields.is_empty() {
                    o.push_str(&format!("    | {}\n", lean_ident(&v.name)));
                } else {
                    let fs: Vec<String> = v.fields.iter().map(|f| lean_prim(f.repr, f.max_len)).collect();
                    o.push_str(&format!("    | {} : {}\n", lean_ident(&v.name), fs.join(" → ")));
                }
            }
        }
        for c in &self.containers {
            match &c.kind {
                ContainerKind::Generic(cs) => {
                    let names: Vec<&str> = cs.iter().map(|c| c.name()).collect();
                    o.push_str(&format!("  container {} : {}\n", lean_ident(&c.name), names.join(", ")));
                }
                ContainerKind::Of(of, a, t) => o.push_str(&format!("  container {} := {}.{} {}\n", lean_ident(&c.name), lean_ident(a), lean_ident(t), lean_ident(of))),
            }
        }
        for i in &self.instances {
            o.push_str(&format!("  instance {} for {}.{}\n", i.class.name(), lean_ident(&i.api), lean_ident(&i.container)));
        }
        for s in &self.sums {
            o.push_str(&format!("  sum {}\n", lean_ident(&s.name)));
            for c in &s.ctors {
                o.push_str(&format!("  {}", lean_ctor(c)));
            }
        }
        for f in &self.fns {
            let mut sig = lean_idents(&f.inputs);
            sig.push(lean_ident(&f.returns));
            o.push_str(&format!("  fn {} : {}\n", lean_ident(&f.name), sig.join(" → ")));
        }
        for c in &self.ctors {
            if c.args.is_empty() {
                o.push_str(&format!("  constructor {}\n", lean_ident(&c.name)));
            } else {
                o.push_str(&format!("  constructor {} : {}\n", lean_ident(&c.name), lean_idents(&c.args).join(" → ")));
            }
        }
        o
    }
}

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

    #[test]
    fn lists_every_problem_at_once() {
        let e = Idl::parse("api \"a\"\ntype \"T\" repr=\"string\"\ntype \"U\" repr=\"i64\"\nconstructor \"X\" \"Nope\"\nconstructor \"x\"").unwrap_err();
        let lines: Vec<&str> = e.lines().collect();
        assert_eq!(
            lines,
            [
                "constructor X: unknown type \"Nope\"",
                "constructor \"x\": not a CamelCase identifier, or duplicate",
                "type T: declared but never used",
                "type U: declared but never used",
            ]
        );
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
        let plain = idl.typescript(false, &BTreeMap::new(), &BTreeMap::new());
        assert!(plain.contains("export class UserId {\n  readonly #v: string;"));
        assert!(plain.contains("kind: \"Created\"; a0: UserId; a1: Payload"));
        assert!(plain.contains("toString()") && !plain.contains("toValue"));
        assert!(idl.typescript(true, &BTreeMap::new(), &BTreeMap::new()).contains("export function fromJson"));
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
    fn fns_and_sums_generate_traits_dispatch_and_clients() {
        let idl: Idl = serde_json::from_str(
            r#"{"api":"nutrition-db","types":[{"name":"Name","kind":{"repr":"string"},"max_len":64},
                {"name":"Id","kind":{"repr":"i64"},"display":true,"store":true}],
                "sums":[{"name":"Written","ctors":[{"name":"Written","args":["Id"]},{"name":"Tick"}]}],
                "fns":[{"name":"insertProduct","inputs":["Name","Name"],"returns":"Written"}]}"#,
        )
        .unwrap();
        let idl = Idl::from_export(idl).unwrap();
        assert_eq!((idl.fn_count(), idl.inputs(), idl.constructors(), idl.slots(), idl.longest()), (1, 2, 2, 1, (64, 0)));
        let rs = idl.rust(true, &[]);
        assert!(rs.contains("pub trait NutritionDb {\n    fn insert_product(&mut self, name0: Name, name1: Name) -> Written;\n}"), "{rs}");
        assert!(rs.contains("pub fn dispatch(impl_: &mut dyn NutritionDb, req: &str) -> String"), "{rs}");
        assert!(rs.contains("impl rusqlite::ToSql for Id"), "{rs}");
        assert!(rs.contains("pub fn new(v: String) -> Result<Self, Violation>") && rs.contains("rule: \"v.length ≤ 64\""), "{rs}");
        assert!(rs.contains("pub enum Written {\n    Written(Id),\n    Tick,\n}"), "{rs}");
        assert!(!idl.rust(false, &[]).contains("dispatch"));
        let ts = idl.typescript(true, &BTreeMap::new(), &BTreeMap::new());
        assert!(ts.contains("export interface NutritionDb {\n  insertProduct(name0: Name, name1: Name): Promise<Written>;\n}"), "{ts}");
        assert!(ts.contains("export class NutritionDbClient implements NutritionDb"), "{ts}");
        assert!(ts.contains("export function writtenFromJson") && ts.contains("return writtenFromJson(await this.send("), "{ts}");
        assert!(idl.typescript(false, &BTreeMap::new(), &BTreeMap::new()).contains("insertProduct(name0: Name, name1: Name): Written;"));
        let kt = idl.kotlin(true);
        assert!(kt.contains("interface NutritionDb {\n    fun insertProduct(name0: Name, name1: Name): Written\n}"), "{kt}");
        assert!(kt.contains("fun fromJsonWritten(s: String): Written"), "{kt}");
        // Validation: a fn must return a sum, every sum must be returned.
        let bad = |fix: &str| {
            let mut v: serde_json::Value = serde_json::from_str(&format!(r#"{{"api":"x","types":[{{"name":"Id","kind":{{"repr":"i64"}}}}],
                "sums":[{{"name":"S","ctors":[{{"name":"C","args":["Id"]}}]}}],"fns":[{{"name":"f","inputs":[],"returns":"S"}}]}}"#)).unwrap();
            let (k, j) = fix.split_once('=').unwrap();
            v[k] = serde_json::from_str(j).unwrap();
            Idl::from_export(serde_json::from_value(v).unwrap()).unwrap_err()
        };
        assert!(bad(r#"fns=[{"name":"f","inputs":[],"returns":"Id"}]"#).contains("not a sum"));
        assert!(bad(r#"fns=[]"#).contains("no fn returns or takes it"));
        assert!(bad(r#"fns=[{"name":"F","inputs":[],"returns":"S"}]"#).contains("camelCase"));
        assert!(bad(r#"sums=[{"name":"S","ctors":[{"name":"C","args":["Id"]},{"name":"C"}]}]"#).contains("duplicate"));
    }

    const PROOFS: &str = r#"{"api":"nutrition-db","types":[
        {"name":"Grams","kind":{"repr":"f64"},"pred":{"and":[{"cmp":["<",{"num":"0"},"v"]},{"cmp":["≤","v",{"num":"20000"}]}]}},
        {"name":"Day","kind":{"repr":"string"},"max_len":10,"pred":{"cmp":["=","len",{"num":"10"}]}},
        {"name":"Span","kind":{"tuple":[{"repr":"i64"},{"repr":"i64"}]},"pred":{"cmp":["<",{"field":1},{"field":2}]}},
        {"name":"Unit","kind":{"repr":"string"},"max_len":8,"pred":{"or":[{"mem":["v",[{"str":"g"},{"str":"ml"}]]},{"not":{"cmp":["≠","len",{"num":"0"}]}}]}},
        {"name":"Id","kind":{"repr":"i64"},"display":true,"store":true}],
        "tokens":[{"name":"ProductRef","repr":"i64","kind":"permanent","store":true},
        {"name":"EventTypeRef","repr":"i64","kind":{"versioned":"Retired"},"store":true},
        {"name":"Funds","repr":"i64","kind":{"linear":"Spent"}}],
        "sums":[{"name":"Written","ctors":[{"name":"Written","args":["Id"]},{"name":"Retired"},{"name":"Spent"}]},
        {"name":"Resolved","ctors":[{"name":"Resolved","args":["ProductRef"]},{"name":"Unknown"}]}],
        "fns":[{"name":"resolveProduct","inputs":["Day"],"returns":"Resolved"},
        {"name":"log","inputs":["ProductRef","Grams","Span","Unit"],"returns":"Written"},
        {"name":"tick","inputs":["EventTypeRef"],"returns":"Written"},
        {"name":"apply","inputs":["Funds"],"returns":"Written"}]}"#;

    #[test]
    fn refinements_and_tokens_generate_checks_and_mint() {
        let idl = Idl::from_export(serde_json::from_str(PROOFS).unwrap()).unwrap();
        assert_eq!((idl.rules(), idl.token_count(), idl.slots(), idl.inputs()), (6, 3, 2, 7));
        assert_eq!(idl.type_decl("Unit").unwrap().pred.as_ref().unwrap().render(), "v ∈ [\"g\", \"ml\"] ∨ ¬(v.length ≠ 0)");
        let rs = idl.rust(true, &[]);
        assert!(rs.contains("pub fn new(v: f64) -> Result<Self, Violation>"), "{rs}");
        assert!(rs.contains("if !(((0.0 < v) && (v <= 20000.0))) { return Err(Violation { type_name: \"Grams\", rule: \"0 < v ∧ v ≤ 20000\" }); }"), "{rs}");
        assert!(rs.contains("if !(v.chars().count() <= 10)") && rs.contains("(v.chars().count() == 10)"), "{rs}");
        assert!(rs.contains("pub struct Span((i64, i64));") && rs.contains("(v.0 < v.1)") && rs.contains("Value::Array(vec![json!(self.0.0), json!(self.0.1)])"), "{rs}");
        assert!(rs.contains("([\"g\", \"ml\"].contains(&v.as_str()) || (!(v.chars().count() != 0)))"), "{rs}");
        assert!(rs.contains("#[derive(Clone)]\npub struct ProductRef(i64);\nimpl ProductRef {\n    pub fn mint(v: i64) -> Self { Self(v) }"), "{rs}");
        assert!(rs.contains("pub struct EventTypeRef(i64, i64);") && rs.contains("pub fn mint(v: i64, version: i64)") && rs.contains("pub fn version(&self)"), "{rs}");
        assert!(rs.contains("/// Evidence (linear)") && rs.contains("pub struct Funds(i64);") && !rs.contains("#[derive(Clone)]\npub struct Funds"), "{rs}");
        assert!(!rs.contains("impl ProductRef {\n    pub fn new") && rs.contains("impl rusqlite::ToSql for ProductRef"), "{rs}");
        assert!(rs.contains("fn apply(&mut self, funds: Funds) -> Written;"), "{rs}");
        let ts = idl.typescript(true, &BTreeMap::new(), &BTreeMap::new());
        assert!(ts.contains("export class Violation extends Error"), "{ts}");
        assert!(ts.contains("if (!(((0 < v) && (v <= 20000)))) throw new Violation(\"Grams\", \"0 < v ∧ v ≤ 20000\");"), "{ts}");
        assert!(ts.contains("readonly #v: [bigint, bigint];") && ts.contains("(v[0] < v[1])"), "{ts}");
        assert!(ts.contains("[\"g\", \"ml\"].includes(v)") && ts.contains("([...v].length <= 8)"), "{ts}");
        assert!(ts.contains("private constructor(v: bigint, version: bigint)") && ts.contains("static mint(v: bigint): ProductRef"), "{ts}");
        let kt = idl.kotlin(true);
        assert!(kt.contains("class Violation(val typeName: String, val rule: String)"), "{kt}");
        assert!(kt.contains("class Span(private val v1: Long, private val v2: Long) {\n    init { if (!((v1 < v2))) throw Violation(\"Span\""), "{kt}");
        assert!(kt.contains("(v in listOf(\"g\", \"ml\"))") && kt.contains("((0.0 < v) && (v <= 20000.0))"), "{kt}");
        assert!(kt.contains("class ProductRef private constructor(private val v: Long)") && kt.contains("fun mint(v: Long, version: Long): EventTypeRef"), "{kt}");
        let lean = idl.lean();
        assert!(lean.contains("  type Grams : f64 where 0 < v ∧ v ≤ 20000\n"), "{lean}");
        assert!(lean.contains("  type Span : i64 × i64 where v.1 < v.2\n") && lean.contains("  type Id : i64 display store\n"), "{lean}");
        assert!(lean.contains("  token ProductRef : i64 permanent store\n  token EventTypeRef : i64 versioned stale=Retired store\n  token Funds : i64 linear spent=Spent\n"), "{lean}");
        // Validation.
        let bad = |from: &str, to: &str| Idl::from_export(serde_json::from_str(&PROOFS.replace(from, to)).unwrap()).unwrap_err();
        assert!(bad(r#"{"name":"Retired"},"#, "").contains("fn tick: takes EventTypeRef (versioned), so Written must have a `Retired` constructor"));
        assert!(bad(r#"{"name":"Spent"}"#, r#"{"name":"Used"}"#).contains("fn apply: takes Funds (linear)"));
        assert!(bad(r#"{"field":2}"#, r#"{"field":3}"#).contains("v.3: the product has 2 components"));
        assert!(bad(r#"{"cmp":["=","len",{"num":"10"}]}"#, r#"{"cmp":["=","v",{"num":"10"}]}}, {"name":"Bad","kind":{"repr":"i64"},"pred":{"cmp":["=","len",{"num":"1"}]}"#).contains("`.length` is for a string"));
        assert!(bad(r#""tuple":[{"repr":"i64"},{"repr":"i64"}]},"#, r#""tuple":[{"repr":"i64"},{"repr":"i64"}]},"display":true,"#).contains("display and store are for scalar"));
        assert!(bad(r#"{"name":"apply","inputs":["Funds"],"returns":"Written"}"#, r#"{"name":"apply","inputs":[],"returns":"Written"}"#).contains("token Funds: declared but never used"));
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
