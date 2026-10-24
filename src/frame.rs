// A module's frame (architect-v3.md, "Frames"): the functions its code is
// made of, declared in `architecture/Frames/<module>.lean` with their
// signatures, the callees each body may call and the sums a body branches
// on. `dashboard arch gen` renders a pinned skeleton under `<src>/frame/`
// (one callee-context trait per fn, the `Frame` struct routing each call,
// the entry fns doing every declared `match`) and a starter for each missing
// hole under `<src>/holes/`, the bodies a worker fills. The build enforces
// the call graph and the arms: a hole receives `&mut dyn <Fn>Ctx`, so it
// sees its declared callees only, and a new constructor breaks the
// generated `match`. `lint_hole` keeps a hole from reaching around its
// context (`crate::`, statics, I/O). Rust only so far.

use crate::config::ArchConfig;
use crate::idl::{self, Idl};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

/// `architecture/Frames/<module>.lean`: the frame's Lean file.
pub fn lean_file(module: &str) -> String {
    format!("{}/Frames/{module}.lean", crate::config::ARCH_DIR)
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FrameFn {
    pub name: String,
    pub inputs: Vec<String>,
    pub returns: String,
    pub calls: Vec<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Record {
    pub name: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Frame {
    pub module: String,
    /// The source directory the skeleton goes under.
    pub src: String,
    /// The Rust path the module's crate mounts its APIs' bindings under.
    pub bindings: String,
    pub fns: Vec<FrameFn>,
    pub records: Vec<Record>,
    pub sums: Vec<idl::Sum>,
    /// The declaring Lean module (`Frames/alpha`).
    pub file: String,
}

impl Frame {
    /// `<src>/frame/`: the generated skeleton, pinned.
    pub fn gen_dir(&self) -> String {
        format!("{}/frame", self.src.trim_end_matches('/'))
    }

    /// `<src>/holes/<fn>.rs`: the worker's bodies of `f`.
    pub fn hole_file(&self, f: &str) -> String {
        format!("{}/holes/{f}.rs", self.src.trim_end_matches('/'))
    }
}

/// A resolved type of a frame signature.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Ty {
    Prim(&'static str),
    Local(String),
    /// `(api, name)`: a type of an API's binding.
    Api(String, String),
}

impl Ty {
    fn rust(&self) -> String {
        match self {
            Ty::Prim(p) => p.to_string(),
            Ty::Local(n) | Ty::Api(_, n) => n.clone(),
        }
    }
}

fn prim(word: &str) -> Option<&'static str> {
    Some(match word {
        "i64" => "i64",
        "f64" => "f64",
        "bool" => "bool",
        "string" => "String",
        "bytes" => "Vec<u8>",
        _ => return None,
    })
}

/// Every type name an API's binding declares.
fn api_type_names(idl: &Idl) -> Vec<String> {
    let mut out: Vec<String> = idl.types.iter().map(|t| t.name.clone()).collect();
    out.extend(idl.tokens.iter().map(|t| t.name.clone()));
    out.extend(idl.sum_list().into_iter().map(|(n, _)| n));
    out.extend(idl.elements.iter().map(|e| e.name.clone()));
    // A generic container takes a type argument: only an instantiated one
    // names a type.
    out.extend(idl.containers.iter().filter(|c| matches!(c.kind, idl::ContainerKind::Of(..))).map(|c| c.name.clone()));
    out
}

/// A callee of a frame fn, resolved.
#[derive(Debug, Clone)]
enum Callee {
    Frame(String),
    /// `(api, fn)`.
    Api(String, idl::Fn),
}

/// A frame checked against the architecture: every name resolved.
struct Plan<'a> {
    frame: &'a Frame,
    /// Rust type name -> resolved type, for every name the frame uses.
    types: BTreeMap<String, Ty>,
    /// Per frame fn: its callees, by context method name.
    callees: BTreeMap<String, Vec<(String, Callee)>>,
    /// The APIs the module consumes and calls: `(api, field, trait)`.
    consumed: Vec<(String, String, String)>,
    /// The APIs the module generates whose every fn is a frame fn.
    provided: Vec<(String, Idl)>,
    idls: &'a BTreeMap<String, Idl>,
}

fn rust_ident_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && !RUST_KEYWORDS.contains(&s)
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct",
    "super", "trait", "true", "type", "unsafe", "use", "where", "while", "gen", "try", "yield", "macro", "box", "do", "final",
    "abstract", "become", "override", "priv", "typeof", "unsized", "virtual", "skeleton", "frame", "holes", "prelude", "stand_in",
];

impl<'a> Plan<'a> {
    fn new(frame: &'a Frame, cfg: &ArchConfig, idls: &'a BTreeMap<String, Idl>) -> Result<Plan<'a>, Vec<String>> {
        let who = format!("frame {}", frame.module);
        let mut errs = Vec::new();
        let Some(m) = cfg.modules.iter().find(|m| m.name == frame.module) else {
            return Err(vec![format!("{who}: {} is no module", frame.module)]);
        };
        // Lean's rules (`Frame.placement`, `Manifest.frameProblems`) held the
        // module's language, `src`, `bindings`, every name, call and branch;
        // what stays here is Rust's naming. Resolution errors below are guards.
        // Types: frame-local first, then every API the module sees.
        let mut types: BTreeMap<String, Ty> = BTreeMap::new();
        let mut local: Vec<&str> = frame.records.iter().map(|r| r.name.as_str()).collect();
        local.extend(frame.sums.iter().map(|s| s.name.as_str()));
        let visible: Vec<&crate::config::ApiConfig> = crate::arch::view_apis(cfg, idls, &m.name);
        let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for a in &visible {
            if let Some(idl) = idls.get(&a.name) {
                for n in api_type_names(idl) {
                    by_name.entry(n).or_default().push(a.name.clone());
                }
            }
        }
        let resolve = |name: &str| -> Result<Ty, String> {
            if let Some(p) = prim(name) {
                return Ok(Ty::Prim(p));
            }
            if local.contains(&name) {
                return Ok(Ty::Local(name.to_string()));
            }
            if let Some((api, ty)) = name.rsplit_once('.') {
                let Some(a) = visible.iter().find(|a| a.name == api) else {
                    return Err(format!("{name}: module {} sees no api {api}", m.name));
                };
                return match idls.get(&a.name) {
                    Some(idl) if api_type_names(idl).iter().any(|n| n == ty) => Ok(Ty::Api(api.to_string(), ty.to_string())),
                    _ => Err(format!("{name}: {api} declares no type {ty}")),
                };
            }
            match by_name.get(name).map(|v| v.as_slice()) {
                Some([one]) => Ok(Ty::Api(one.clone(), name.to_string())),
                Some(many) => Err(format!("{name} is declared by {}: write api.{name}", many.join(" and "))),
                None => Err(format!("unknown type {name}: a prim (i64, f64, bool, string, bytes), a type of the frame, or of an api {} sees", m.name)),
            }
        };
        let note = |name: &str, types: &mut BTreeMap<String, Ty>, errs: &mut Vec<String>| match resolve(name) {
            Ok(t) => {
                let key = t.rust();
                match types.get(&key) {
                    Some(prev) if *prev != t => errs.push(format!("{who}: two types named {key}: {prev:?} and {t:?}")),
                    _ => {
                        types.insert(key, t);
                    }
                }
            }
            Err(e) => errs.push(format!("{who}: {e}")),
        };
        for r in &frame.records {
            r.fields.iter().for_each(|f| note(f, &mut types, &mut errs));
        }
        for s in &frame.sums {
            s.ctors.iter().flat_map(|c| &c.args).for_each(|a| note(a, &mut types, &mut errs));
        }
        for f in &frame.fns {
            if !rust_ident_ok(&f.name) {
                errs.push(format!("{who}: fn {:?} must be a snake_case Rust name", f.name));
            }
            f.inputs.iter().chain(std::iter::once(&f.returns)).for_each(|t| note(t, &mut types, &mut errs));
        }
        // Callees: frame fns, or fns of an API the module consumes.
        let mut callees: BTreeMap<String, Vec<(String, Callee)>> = BTreeMap::new();
        let mut consumed: Vec<(String, String, String)> = Vec::new();
        for f in &frame.fns {
            let mut list: Vec<(String, Callee)> = Vec::new();
            for c in &f.calls {
                let (method, callee) = if let Some(g) = frame.fns.iter().find(|g| g.name == *c) {
                    (g.name.clone(), Callee::Frame(g.name.clone()))
                } else if let Some((api, name)) = c.rsplit_once('.') {
                    let Some(a) = cfg.apis.iter().find(|a| a.name == api && a.consumer == m.name) else {
                        errs.push(format!("{who}: fn {} calls {c}: {} consumes no api {api}", f.name, m.name));
                        continue;
                    };
                    let Some(idl_fn) = idls.get(&a.name).and_then(|i| i.fns.iter().find(|x| x.name == name)) else {
                        errs.push(format!("{who}: fn {} calls {c}: {api} has no fn {name}", f.name));
                        continue;
                    };
                    for t in idl_fn.inputs.iter().chain(std::iter::once(&idl_fn.returns)) {
                        note(&format!("{api}.{t}"), &mut types, &mut errs);
                    }
                    let field = api.replace('-', "_");
                    if !consumed.iter().any(|(x, _, _)| x == api) {
                        consumed.push((api.to_string(), field, idl::pascal(api)));
                    }
                    (idl::snake(name), Callee::Api(api.to_string(), idl_fn.clone()))
                } else {
                    errs.push(format!("{who}: fn {} calls {c}: no frame fn {c} (an api fn is written api.fn)", f.name));
                    continue;
                };
                if list.iter().any(|(x, _)| *x == method) {
                    errs.push(format!("{who}: fn {} has two callees named {method}", f.name));
                    continue;
                }
                list.push((method, callee));
            }
            callees.insert(f.name.clone(), list);
        }
        // Each ctor arg of a branched sum is a hole parameter: name its type.
        for f in &frame.fns {
            if let Some(Ok(t)) = f.branch.as_ref().map(|b| resolve(b)) {
                for (_, args) in plan_sum(frame, idls, &t).unwrap_or_default() {
                    for a in args {
                        let q = match &t {
                            Ty::Api(api, _) if prim(&a).is_none() => format!("{api}.{a}"),
                            _ => a,
                        };
                        note(&q, &mut types, &mut errs);
                    }
                }
            }
        }
        // A hole builds the sums it returns: every type a constructor of a
        // sum in the frame takes is in the prelude too.
        loop {
            let mut more: Vec<String> = Vec::new();
            for t in types.values() {
                if let Ty::Api(api, n) = t
                    && let Some(ctors) = plan_sum(frame, idls, t)
                {
                    let _ = n;
                    for (_, args) in ctors {
                        for a in args {
                            let q = if prim(&a).is_some() { a } else { format!("{api}.{a}") };
                            if resolve(&q).is_ok_and(|r| !types.contains_key(&r.rust())) {
                                more.push(q);
                            }
                        }
                    }
                }
            }
            if more.is_empty() {
                break;
            }
            for q in more {
                note(&q, &mut types, &mut errs);
            }
        }
        // Names the skeleton defines or imports itself.
        let mut reserved: Vec<String> = ["Frame", "Q", "VecDeque", "Box", "Vec", "String", "Option", "Result", "PhantomData"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for f in &frame.fns {
            reserved.push(pascal_ctx(&f.name));
            reserved.push(format!("{}StandIn", pascal_ctx(&f.name)));
        }
        reserved.extend(consumed.iter().map(|(_, _, t)| t.clone()));
        reserved.extend(cfg.apis.iter().filter(|a| a.generator == m.name).map(|a| idl::pascal(&a.name)));
        for name in types.keys() {
            if reserved.contains(name) {
                errs.push(format!("{who}: type {name} collides with a name the skeleton defines (Frame, the context traits, stand-ins, API traits, Q)"));
            }
        }
        for (f, list) in &callees {
            for (method, _) in list {
                if ["default", "drop", "calls", "clone"].contains(&method.as_str()) {
                    errs.push(format!("{who}: fn {f} calls {method}: the name is taken by the stand-in"));
                }
            }
        }
        // A generated API is provided by the frame when its fns are frame fns.
        let mut provided = Vec::new();
        for a in cfg.apis.iter().filter(|a| a.generator == m.name) {
            let Some(idl) = idls.get(&a.name) else { continue };
            if idl.fns.is_empty() {
                continue;
            }
            let matching: Vec<&idl::Fn> = idl.fns.iter().filter(|x| frame.fns.iter().any(|f| f.name == idl::snake(&x.name))).collect();
            if matching.is_empty() {
                continue;
            }
            // Provided only when every fn of the API is a frame fn.
            if matching.len() < idl.fns.len() {
                continue;
            }
            let mut ok = true;
            for x in &idl.fns {
                match frame.fns.iter().find(|f| f.name == idl::snake(&x.name)) {
                    None => ok = false,
                    Some(f) => {
                        let want: Vec<Result<Ty, String>> =
                            x.inputs.iter().chain(std::iter::once(&x.returns)).map(|t| resolve(&format!("{}.{t}", a.name))).collect();
                        let have: Vec<Result<Ty, String>> = f.inputs.iter().chain(std::iter::once(&f.returns)).map(|t| resolve(t)).collect();
                        // Lean's rules hold the signature; a mismatch provides nothing.
                        if want != have {
                            ok = false;
                        }
                    }
                }
            }
            if ok {
                provided.push((a.name.clone(), idl.clone()));
            }
        }
        if errs.is_empty() { Ok(Plan { frame, types, callees, consumed, provided, idls }) } else { Err(errs) }
    }

    fn ty(&self, name: &str) -> String {
        if let Some(p) = prim(name) {
            return p.to_string();
        }
        name.rsplit('.').next().unwrap_or(name).to_string()
    }

    fn sig_params(&self, inputs: &[String]) -> Vec<(String, String)> {
        let short: Vec<String> = inputs.iter().map(|t| self.ty(t)).collect();
        // Named after the frame's own word (`bytes`, not `Vec<u8>`).
        let words: Vec<String> = inputs.iter().map(|t| t.rsplit('.').next().unwrap_or(t).to_string()).collect();
        param_names(&words).into_iter().zip(short).collect()
    }

    /// The arms of `f`'s branch: `(ctor, arg types)`, and the input index.
    fn arms(&self, f: &FrameFn) -> Option<(usize, String, Vec<(String, Vec<String>)>)> {
        let b = f.branch.as_ref()?;
        let bty = self.ty(b);
        let idx = f.inputs.iter().position(|i| self.ty(i) == bty)?;
        let t = self.types.get(&bty)?;
        let ctors = plan_sum(self.frame, self.idls, t)?;
        Some((idx, bty, ctors.into_iter().map(|(c, args)| (c, args.iter().map(|a| self.ty(a)).collect())).collect()))
    }

    /// The hole fn names of `f`.
    fn holes_of(&self, f: &FrameFn) -> Vec<String> {
        match self.arms(f) {
            Some((_, _, arms)) => arms.iter().map(|(c, _)| format!("on_{}", idl::snake(c))).collect(),
            None => vec![f.name.clone()],
        }
    }

    /// `(hole, params after ctx, returns)` for each hole of `f`.
    fn hole_sigs(&self, f: &FrameFn) -> Vec<(String, Vec<(String, String)>, String)> {
        let ret = self.ty(&f.returns);
        let params = self.sig_params(&f.inputs);
        match self.arms(f) {
            None => vec![(f.name.clone(), params, ret)],
            Some((idx, _, arms)) => arms
                .into_iter()
                .map(|(c, args)| {
                    let mut ps: Vec<(String, String)> = params.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, p)| p.clone()).collect();
                    let names = param_names(&args);
                    for (n, t) in names.into_iter().zip(args) {
                        let n = if ps.iter().any(|(x, _)| *x == n) { format!("{n}_{}", idl::snake(&c)) } else { n };
                        ps.push((n, t));
                    }
                    (format!("on_{}", idl::snake(&c)), ps, ret.clone())
                })
                .collect(),
        }
    }

    fn derivable(&self, name: &str, depth: usize) -> bool {
        if depth > 16 {
            return false;
        }
        match self.types.get(name) {
            Some(Ty::Prim(_)) => true,
            Some(Ty::Local(l)) => local_parts_all(self.frame, l).iter().all(|p| prim(p).is_some() || self.derivable(p, depth + 1)),
            Some(Ty::Api(..)) | None => prim(name).is_some(),
        }
    }

    /// The prelude's names: what a hole may name.
    fn prelude_names(&self) -> Vec<String> {
        self.types.values().filter(|t| !matches!(t, Ty::Prim(_))).map(|t| t.rust()).collect()
    }

    fn render(&self) -> Vec<(String, String)> {
        let fr = self.frame;
        let gen_dir = fr.gen_dir();
        let header = format!(
            "// @generated by `dashboard arch gen` from {}. Do not edit.\n",
            lean_file(&fr.module)
        );
        let mut o = header.clone();
        o.push_str("#![allow(dead_code, unused_imports, unused_variables)]\nuse prelude::*;\n");
        for (api, _, tr) in &self.consumed {
            o.push_str(&format!("use {}::{}::{tr};\n", fr.bindings, api.replace('-', "_")));
        }
        for (api, _) in &self.provided {
            o.push_str(&format!("use {}::{}::{};\n", fr.bindings, api.replace('-', "_"), idl::pascal(api)));
        }
        // The prelude: binding types (never a call-surface trait) and the
        // frame's own records and sums.
        o.push_str("\n/// What every hole sees: the frame's types, never a call surface.\npub mod prelude {\n");
        let mut by_api: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for t in self.types.values() {
            if let Ty::Api(a, n) = t {
                by_api.entry(a.as_str()).or_default().push(n.as_str());
            }
        }
        for (a, ns) in &by_api {
            o.push_str(&format!("    pub use {}::{}::{{{}}};\n", fr.bindings, a.replace('-', "_"), ns.join(", ")));
        }
        for r in &fr.records {
            let fields: Vec<String> = r.fields.iter().map(|f| format!("pub {}", self.ty(f))).collect();
            if self.derivable(&r.name, 0) {
                o.push_str("    #[derive(Clone, Debug, PartialEq)]\n");
            }
            o.push_str(&format!("    pub struct {}({});\n", r.name, fields.join(", ")));
        }
        for s in &fr.sums {
            if self.derivable(&s.name, 0) {
                o.push_str("    #[derive(Clone, Debug, PartialEq)]\n");
            }
            o.push_str(&format!("    pub enum {} {{\n", s.name));
            for c in &s.ctors {
                if c.args.is_empty() {
                    o.push_str(&format!("        {},\n", c.name));
                } else {
                    let args: Vec<String> = c.args.iter().map(|a| self.ty(a)).collect();
                    o.push_str(&format!("        {}({}),\n", c.name, args.join(", ")));
                }
            }
            o.push_str("    }\n");
        }
        o.push_str("}\n");
        // Context traits.
        for f in &fr.fns {
            o.push_str(&format!("\n/// The callees `{}` declares: exactly these.\npub trait {} {{\n", f.name, pascal_ctx(&f.name)));
            for (method, c) in &self.callees[&f.name] {
                let (ins, ret) = self.callee_sig(c);
                let ps: String = self.sig_params(&ins).iter().map(|(n, t)| format!(", {n}: {t}")).collect();
                o.push_str(&format!("    fn {method}(&mut self{ps}) -> {};\n", self.ty(&ret)));
            }
            o.push_str("}\n");
        }
        // The frame: the consumed APIs a composition root hands in.
        o.push_str("\n/// The consumed APIs, handed in by a composition root.\npub struct Frame<'a> {\n");
        if self.consumed.is_empty() {
            o.push_str("    _apis: std::marker::PhantomData<&'a mut ()>,\n");
        }
        for (_, field, tr) in &self.consumed {
            o.push_str(&format!("    {field}: &'a mut dyn {tr},\n"));
        }
        o.push_str("}\n\nimpl<'a> Frame<'a> {\n");
        let args: Vec<String> = self.consumed.iter().map(|(_, f, t)| format!("{f}: &'a mut dyn {t}")).collect();
        let fields: Vec<String> = self.consumed.iter().map(|(_, f, _)| f.clone()).collect();
        let init = if self.consumed.is_empty() { "_apis: std::marker::PhantomData".to_string() } else { fields.join(", ") };
        o.push_str(&format!("    pub fn new({}) -> Self {{\n        Frame {{ {init} }}\n    }}\n}}\n", args.join(", ")));
        for f in &fr.fns {
            o.push_str(&format!("\nimpl {} for Frame<'_> {{\n", pascal_ctx(&f.name)));
            for (method, c) in &self.callees[&f.name] {
                let (ins, ret) = self.callee_sig(c);
                let ps = self.sig_params(&ins);
                let decl: String = ps.iter().map(|(n, t)| format!(", {n}: {t}")).collect();
                let names: Vec<&str> = ps.iter().map(|(n, _)| n.as_str()).collect();
                let body = match c {
                    Callee::Frame(g) => format!("{g}(self{})", names.iter().map(|n| format!(", {n}")).collect::<String>()),
                    Callee::Api(api, x) => format!("self.{}.{}({})", api.replace('-', "_"), idl::snake(&x.name), names.join(", ")),
                };
                o.push_str(&format!("    fn {method}(&mut self{decl}) -> {} {{ {body} }}\n", self.ty(&ret)));
            }
            o.push_str("}\n");
        }
        // Entry fns: narrow to the context, then the declared match.
        o.push_str("\n// A hole receives `&mut dyn <Fn>Ctx`: its own callees only.\n");
        for f in &fr.fns {
            let ps = self.sig_params(&f.inputs);
            let decl: String = ps.iter().map(|(n, t)| format!(", {n}: {t}")).collect();
            let ret = self.ty(&f.returns);
            o.push_str(&format!(
                "pub fn {}(__f: &mut Frame<'_>{decl}) -> {ret} {{\n    let __ctx: &mut dyn {} = __f;\n",
                f.name,
                pascal_ctx(&f.name)
            ));
            match self.arms(f) {
                None => {
                    let names: String = ps.iter().map(|(n, _)| format!(", {n}")).collect();
                    o.push_str(&format!("    super::holes::{}::{}(__ctx{names})\n", f.name, f.name));
                }
                Some((idx, sum, arms)) => {
                    let rest: Vec<&str> = ps.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, (n, _))| n.as_str()).collect();
                    o.push_str(&format!("    match {} {{\n", ps[idx].0));
                    let sigs = self.hole_sigs(f);
                    for ((c, args), (hole, hps, _)) in arms.iter().zip(&sigs) {
                        let binds: Vec<String> = hps[rest.len()..].iter().map(|(n, _)| n.clone()).collect();
                        let pat = if args.is_empty() { format!("{sum}::{c}") } else { format!("{sum}::{c}({})", binds.join(", ")) };
                        let call: Vec<&str> = rest.iter().copied().chain(binds.iter().map(|b| b.as_str())).collect();
                        let call: String = call.iter().map(|n| format!(", {n}")).collect();
                        o.push_str(&format!("        {pat} => super::holes::{}::{hole}(__ctx{call}),\n", f.name));
                    }
                    o.push_str("    }\n");
                }
            }
            o.push_str("}\n");
        }
        for (api, idl) in &self.provided {
            o.push_str(&format!("\n/// The provided api {api}: its fns are frame fns.\nimpl {} for Frame<'_> {{\n", idl::pascal(api)));
            for x in &idl.fns {
                let ps = self.sig_params(&x.inputs);
                let decl: String = ps.iter().map(|(n, t)| format!(", {n}: {t}")).collect();
                let names: String = ps.iter().map(|(n, _)| format!(", {n}")).collect();
                o.push_str(&format!("    fn {}(&mut self{decl}) -> {} {{ {}(self{names}) }}\n", idl::snake(&x.name), x.returns, idl::snake(&x.name)));
            }
            o.push_str("}\n");
        }
        o.push_str(&self.stand_ins());
        // Hole mounts: a sibling of `skeleton`, so `Frame`'s fields stay hidden.
        let mut h = header.clone();
        h.push_str("// Mounts each hole file in a module that sees the prelude and its own\n// context trait only.\n");
        for f in &fr.fns {
            let ctx = pascal_ctx(&f.name);
            h.push_str(&format!(
                "#[forbid(unsafe_code)]\npub mod {name} {{\n    #[allow(unused_imports)]\n    use super::super::skeleton::{{prelude::*, {ctx}}};\n    \
                 #[cfg(test)]\n    #[allow(unused_imports)]\n    use super::super::skeleton::stand_in::{ctx}StandIn;\n    \
                 include!(\"../holes/{name}.rs\");\n}}\n",
                name = f.name
            ));
        }
        let m = format!("{header}//! The frame of {}: `skeleton` is generated, `holes` mounts the bodies.\npub mod skeleton;\nmod holes;\npub use skeleton::*;\n", fr.module);
        vec![(format!("{gen_dir}/mod.rs"), m), (format!("{gen_dir}/skeleton.rs"), o), (format!("{gen_dir}/holes.rs"), h)]
    }

    fn callee_sig(&self, c: &Callee) -> (Vec<String>, String) {
        match c {
            Callee::Frame(g) => {
                let f = self.frame.fns.iter().find(|f| f.name == *g).unwrap();
                (f.inputs.clone(), f.returns.clone())
            }
            Callee::Api(api, x) => {
                let q = |t: &String| if prim(t).is_some() { t.clone() } else { format!("{api}.{t}") };
                (x.inputs.iter().map(q).collect(), q(&x.returns))
            }
        }
    }

    /// Per context trait, a `#[cfg(test)]` stand-in: one queue of answers
    /// per callee, a log of calls, and a check at drop that every queued
    /// answer was used.
    fn stand_ins(&self) -> String {
        let mut o = String::from(
            "\n/// Stand-ins: one queue of answers per callee, for a hole's unit tests.\n#[cfg(test)]\npub mod stand_in {\n    \
             use super::prelude::*;\n    use std::collections::VecDeque;\n    \
             pub type Q<A, R> = VecDeque<Box<dyn FnMut(A) -> R>>;\n    \
             fn next<A, R>(q: &mut Q<A, R>, name: &str, a: A) -> R {\n        \
             let mut f = q.pop_front().unwrap_or_else(|| panic!(\"stand-in: no answer left for {name}\"));\n        f(a)\n    }\n",
        );
        for f in &self.frame.fns {
            let ctx = pascal_ctx(&f.name);
            let list = &self.callees[&f.name];
            o.push_str(&format!("\n    #[derive(Default)]\n    pub struct {ctx}StandIn {{\n"));
            let mut methods = String::new();
            for (method, c) in list {
                let (ins, ret) = self.callee_sig(c);
                let ps = self.sig_params(&ins);
                let arg_ty = match ps.len() {
                    1 => ps[0].1.clone(),
                    _ => format!("({})", ps.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(", ")),
                };
                let arg = match ps.len() {
                    1 => ps[0].0.clone(),
                    _ => format!("({})", ps.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")),
                };
                o.push_str(&format!("        pub {method}: Q<{arg_ty}, {}>,\n", self.ty(&ret)));
                let decl: String = ps.iter().map(|(n, t)| format!(", {n}: {t}")).collect();
                methods.push_str(&format!(
                    "        fn {method}(&mut self{decl}) -> {} {{\n            self.calls.push(\"{method}\");\n            next(&mut self.{method}, \"{method}\", {arg})\n        }}\n",
                    self.ty(&ret)
                ));
            }
            o.push_str("        pub calls: Vec<&'static str>,\n    }\n");
            let empties: Vec<String> = list.iter().map(|(m, _)| format!("self.{m}.is_empty()")).collect();
            if !empties.is_empty() {
                o.push_str(&format!(
                    "    impl Drop for {ctx}StandIn {{\n        fn drop(&mut self) {{\n            if !std::thread::panicking() {{\n                \
                     assert!({}, \"{ctx}StandIn: unused answers\");\n            }}\n        }}\n    }}\n",
                    empties.join(" && ")
                ));
            }
            o.push_str(&format!("    impl super::{ctx} for {ctx}StandIn {{\n{methods}    }}\n"));
        }
        o.push_str("}\n");
        o
    }

    /// The starter of a missing hole file: each hole a `todo!()`.
    fn starter(&self, f: &FrameFn) -> String {
        let ctx = pascal_ctx(&f.name);
        let mut o = format!(
            "// The body of `{}` (frame {}): yours to fill. It sees its parameters, the frame's\n\
             // prelude and `ctx`, whose methods are its declared callees. Private helpers may\n\
             // live here; only the holes are `pub`.\n",
            f.name, self.frame.module
        );
        for (hole, ps, ret) in self.hole_sigs(f) {
            let decl: String = ps.iter().map(|(n, t)| format!(", _{n}: {t}")).collect();
            o.push_str(&format!("\npub fn {hole}(_ctx: &mut dyn {ctx}{decl}) -> {ret} {{\n    todo!(\"{}: {hole}\")\n}}\n", f.name));
        }
        o
    }
}

/// Parameter names for types: snake case, numbered when a type repeats, a
/// Rust keyword suffixed with `_`.
fn param_names(types: &[String]) -> Vec<String> {
    idl::param_names(types, idl::snake)
        .into_iter()
        .map(|n| if RUST_KEYWORDS.contains(&n.as_str()) { format!("{n}_") } else { n })
        .collect()
}

fn pascal_ctx(f: &str) -> String {
    format!("{}Ctx", idl::pascal(f))
}

/// The type names a frame-local type holds directly (records' fields, sums' args).
fn local_parts_all(frame: &Frame, name: &str) -> Vec<String> {
    if let Some(r) = frame.records.iter().find(|r| r.name == name) {
        return r.fields.clone();
    }
    if let Some(s) = frame.sums.iter().find(|s| s.name == name) {
        return s.ctors.iter().flat_map(|c| c.args.clone()).collect();
    }
    Vec::new()
}

/// The constructors of the sum `t` names, with their arg type names.
fn plan_sum(frame: &Frame, idls: &BTreeMap<String, Idl>, t: &Ty) -> Option<Vec<(String, Vec<String>)>> {
    match t {
        Ty::Local(n) => frame.sums.iter().find(|s| s.name == *n).map(|s| s.ctors.iter().map(|c| (c.name.clone(), c.args.clone())).collect()),
        Ty::Api(api, n) => idls
            .get(api)?
            .sum_list()
            .into_iter()
            .find(|(s, _)| s == n)
            .map(|(_, cs)| cs.iter().map(|c| (c.name.clone(), c.args.clone())).collect()),
        Ty::Prim(_) => None,
    }
}

/// What the frames of an architecture generate: the skeleton files (pinned),
/// the hole files with their starters and declared hole names, and every
/// problem, one line each.
#[derive(Debug, Default)]
pub struct Rendered {
    pub skeleton: Vec<(PathBuf, String)>,
    /// `(hole file, starter, hole fn names, prelude names, ctx trait)`.
    pub holes: Vec<Hole>,
    pub errors: Vec<String>,
    /// `frame <module>: fns 3, holes 5, …` lines.
    pub headroom: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Hole {
    pub file: PathBuf,
    pub starter: String,
    pub holes: Vec<String>,
    pub prelude: Vec<String>,
}

pub fn render(cfg: &ArchConfig, idls: &BTreeMap<String, Idl>) -> Rendered {
    let mut out = Rendered::default();
    let mut seen = BTreeSet::new();
    for fr in &cfg.frames {
        if !seen.insert(fr.module.as_str()) {
            out.errors.push(format!("frame {}: declared twice", fr.module));
            continue;
        }
        let plan = match Plan::new(fr, cfg, idls) {
            Ok(p) => p,
            Err(e) => {
                out.errors.extend(e);
                continue;
            }
        };
        out.skeleton.extend(plan.render().into_iter().map(|(p, t)| (PathBuf::from(p), t)));
        let mut prelude = plan.prelude_names();
        let mut holes = 0;
        for f in &fr.fns {
            let names = plan.holes_of(f);
            holes += names.len();
            let mut pre = prelude.clone();
            pre.push(pascal_ctx(&f.name));
            pre.push(format!("{}StandIn", pascal_ctx(&f.name)));
            out.holes.push(Hole { file: PathBuf::from(fr.hole_file(&f.name)), starter: plan.starter(f), holes: names, prelude: pre });
        }
        prelude.sort();
        // Reach from the provided APIs: a frame fn none reaches is dead
        // unless a test drives it.
        let mut reached: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<String> = plan.provided.iter().flat_map(|(_, i)| i.fns.iter().map(|x| idl::snake(&x.name))).collect();
        while let Some(f) = stack.pop() {
            if reached.insert(f.clone()) {
                for (_, c) in plan.callees.get(&f).into_iter().flatten() {
                    if let Callee::Frame(g) = c {
                        stack.push(g.clone());
                    }
                }
            }
        }
        let mut line = format!("frame {}: fns {}, holes {holes}", fr.module, fr.fns.len());
        let calls: usize = plan.callees.values().map(|v| v.iter().filter(|(_, c)| matches!(c, Callee::Api(..))).count()).sum();
        line.push_str(&format!(", api calls {calls}"));
        if !plan.provided.is_empty() {
            let unreached: Vec<&str> = fr.fns.iter().map(|f| f.name.as_str()).filter(|f| !reached.contains(*f)).collect();
            line.push_str(&format!(", provides {}", plan.provided.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>().join(", ")));
            if !unreached.is_empty() {
                line.push_str(&format!(", unreached from its apis: {}", unreached.join(", ")));
            }
        }
        out.headroom.push(line);
    }
    out
}

/// The call graph of the frames, as `module.fn -> callee` edges (an api
/// callee as `api.fn`), for `dashboard arch frames`.
pub fn edges(cfg: &ArchConfig) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for fr in &cfg.frames {
        for f in &fr.fns {
            for c in &f.calls {
                let to = if fr.fns.iter().any(|g| g.name == *c) { format!("{}.{c}", fr.module) } else { c.clone() };
                out.push((format!("{}.{}", fr.module, f.name), to));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The hole lint
// ---------------------------------------------------------------------------

const ROOTS_STD: &[&str] = &["std", "core", "alloc"];
/// Modules of std that reach the world or hold ambient state.
const DENIED_MODS: &[&str] = &["fs", "net", "io", "process", "env", "thread", "os", "time", "path", "ffi", "arch", "panic", "backtrace"];
const STD_PRELUDE: &[&str] = &[
    "Vec", "String", "Box", "Option", "Some", "None", "Result", "Ok", "Err", "ToString", "ToOwned", "Iterator",
    "IntoIterator", "From", "Into", "Default", "Clone", "Copy", "PartialEq", "Eq", "PartialOrd", "Ord", "Drop", "Fn",
    "FnMut", "FnOnce", "AsRef", "AsMut", "Self", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
    "u128", "usize", "f32", "f64", "bool", "char", "str",
];
const MACROS: &[&str] = &[
    "vec", "format", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne", "matches",
    "panic", "unreachable", "todo", "unimplemented", "write", "writeln", "concat", "stringify",
];
/// The attributes a hole may carry: lints, tests, codegen hints, plain docs
/// and std derives. Anything else may be a proc macro or a linker hook.
const ALLOWED_ATTRS: &[&str] =
    &["allow", "warn", "deny", "expect", "forbid", "test", "should_panic", "ignore", "inline", "cold", "must_use", "track_caller", "derive", "doc", "cfg"];
const STD_DERIVES: &[&str] = &["Clone", "Copy", "Debug", "Default", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash"];

struct Lint<'s> {
    src: &'s [u8],
    holes: BTreeSet<String>,
    roots: BTreeSet<String>,
    prelude: BTreeSet<String>,
    /// A glob of a std module is imported: an unknown capitalized root may
    /// be one of its items.
    std_glob: bool,
    out: Vec<(usize, String)>,
}

fn text<'a>(n: Node, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

impl<'s> Lint<'s> {
    fn flag(&mut self, n: Node, msg: String) {
        self.out.push((n.start_position().row + 1, msg));
    }

    /// The first segment of a path node, "" for a leading `::`.
    fn root(&self, mut n: Node) -> String {
        loop {
            match n.kind() {
                "scoped_identifier" | "scoped_type_identifier" | "scoped_use_list" => match n.child_by_field_name("path") {
                    Some(p) => n = p,
                    None => return String::new(),
                },
                "generic_type" => match n.child_by_field_name("type") {
                    Some(t) => n = t,
                    None => return text(n, self.src).to_string(),
                },
                _ => return text(n, self.src).to_string(),
            }
        }
    }

    fn check_root(&mut self, n: Node, whole: &str, root: &str, second: Option<&str>, what: &str) {
        match root {
            "" => self.flag(n, format!("`{what}{whole}`: a leading `::` names an extern crate")),
            "crate" | "super" => self.flag(n, format!("`{what}{whole}`: a hole reaches only its ctx; `{root}::` leaves the hole")),
            r if ROOTS_STD.contains(&r) => {
                if let Some(s) = second
                    && DENIED_MODS.contains(&s)
                {
                    self.flag(n, format!("`{what}{whole}`: `{r}::{s}` reaches the world or ambient state"));
                }
            }
            "self" | "Self" => {}
            r if self.roots.contains(r) || STD_PRELUDE.contains(&r) => {}
            r if self.std_glob && r.chars().next().is_some_and(|c| c.is_ascii_uppercase()) => {}
            r => self.flag(n, format!("`{what}{whole}`: `{r}` is neither the prelude, std, nor an item of this file")),
        }
    }

    fn check_path(&mut self, n: Node) {
        let root = self.root(n);
        // `<T as Trait>::f`: the types inside the brackets are what it names.
        if root.starts_with('<') {
            let mut c = n.walk();
            let kids: Vec<Node> = n.children(&mut c).collect();
            for k in kids {
                self.node(k);
            }
            return;
        }
        let whole = text(n, self.src).to_string();
        let second = whole.split("::").nth(1).map(|s| s.trim().trim_start_matches('{').trim().to_string());
        self.check_root(n, &whole, &root, second.as_deref(), "");
    }

    fn is_cfg_test(&self, attr: Node) -> bool {
        let t: String = text(attr, self.src).chars().filter(|c| !c.is_whitespace()).collect();
        t == "#[cfg(test)]" || t.starts_with("#[cfg(all(test,")
    }

    fn top(&mut self, root: Node) {
        let mut c = root.walk();
        let items: Vec<Node> = root.named_children(&mut c).collect();
        let mut skip_next = false;
        let mut seen = BTreeSet::new();
        for it in items {
            if it.kind() == "attribute_item" {
                if self.is_cfg_test(it) {
                    skip_next = true;
                } else {
                    self.node(it);
                }
                continue;
            }
            if std::mem::take(&mut skip_next) {
                continue;
            }
            let vis = {
                let mut c = it.walk();
                it.named_children(&mut c).find(|x| x.kind() == "visibility_modifier")
            };
            let name = it.child_by_field_name("name").map(|n| text(n, self.src).to_string()).unwrap_or_default();
            if it.kind() == "function_item" && self.holes.contains(&name) {
                if vis.is_none() {
                    self.flag(it, format!("hole `{name}` must be `pub`"));
                }
                seen.insert(name.clone());
            } else if let Some(v) = vis {
                self.flag(v, format!("`{name}`: only the declared holes are `pub`; a helper stays private to its hole"));
            }
            self.node(it);
        }
        for h in self.holes.clone() {
            if !seen.contains(&h) {
                self.flag(root, format!("declared hole `{h}` is missing"));
            }
        }
    }

    fn node(&mut self, n: Node) {
        match n.kind() {
            "static_item" => self.flag(n, "`static`: a hole holds no global state".into()),
            "extern_crate_declaration" => self.flag(n, "`extern crate`: a hole links nothing".into()),
            "foreign_mod_item" => self.flag(n, "`extern` block: a hole links nothing".into()),
            "unsafe_block" => self.flag(n, "`unsafe` block".into()),
            "function_modifiers" | "impl_item" | "trait_item" if text(n, self.src).starts_with("unsafe") => {
                self.flag(n, "`unsafe` item".into())
            }
            "mod_item" if n.child_by_field_name("body").is_none() => self.flag(n, "`mod x;`: a hole is one file".into()),
            "macro_definition" => self.flag(n, "`macro_rules!`: a hole defines no macros".into()),
            "inner_attribute_item" => {
                self.flag(n, "an inner attribute (`#![…]`): a hole file is included into its module, so it holds none".into());
                return;
            }
            "line_comment" | "block_comment" => {
                let t = text(n, self.src);
                if t.starts_with("//!") || t.starts_with("/*!") {
                    self.flag(n, "an inner doc comment (`//!`): a hole file is included into its module, so it holds none".into());
                }
                return;
            }
            "attribute_item" => {
                let t: String = text(n, self.src).chars().filter(|c| !c.is_whitespace()).collect();
                let body = t.trim_start_matches('#').trim_start_matches('[').trim_end_matches(']');
                let name: String = body.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
                if !ALLOWED_ATTRS.contains(&name.as_str()) {
                    self.flag(n, format!("`#[{name}]`: a hole carries lint, test, inline, doc and std derive attributes only"));
                } else if name == "derive" {
                    let inner = body.trim_start_matches("derive").trim_start_matches('(').trim_end_matches(')');
                    if let Some(d) = inner.split(',').find(|d| !d.is_empty() && !STD_DERIVES.contains(d)) {
                        self.flag(n, format!("`#[derive({d})]`: a hole derives std traits only"));
                    }
                } else if name == "doc" && !body.starts_with("doc=\"") {
                    self.flag(n, "`#[doc = …]`: a plain string only".into());
                } else if name == "cfg" && body != "cfg(test)" {
                    self.flag(n, "`#[cfg(…)]`: a hole is the same code in every build (only `cfg(test)`)".into());
                }
                return;
            }
            "impl_item" => {
                if let Some(ty) = n.child_by_field_name("type") {
                    let r = self.root(ty);
                    if !self.roots.contains(&r) || self.prelude.contains(&r) {
                        self.flag(n, format!("`impl` for `{}`: a hole implements traits for its own types only", text(ty, self.src)));
                    }
                }
            }
            "macro_invocation" => {
                let m = n.child_by_field_name("macro").map(|m| text(m, self.src).to_string()).unwrap_or_default();
                if !MACROS.contains(&m.as_str()) {
                    self.flag(n, format!("`{m}!`: a macro outside the allowed set"));
                }
                // A macro's tokens are unparsed: scan them as text.
                if let Some(tt) = n.named_child(n.named_child_count().saturating_sub(1) as u32) {
                    self.tokens(tt);
                }
                return;
            }
            "token_tree" => {
                self.tokens(n);
                return;
            }
            "scoped_identifier" | "scoped_type_identifier" | "scoped_use_list" => {
                self.check_path(n);
                return;
            }
            "use_declaration" => {
                if let Some(arg) = n.child_by_field_name("argument") {
                    let mut leaves = Vec::new();
                    self.expand(arg, String::new(), &mut leaves);
                    for (path, bound) in leaves {
                        let segs: Vec<&str> = path.split("::").map(str::trim).collect();
                        self.check_root(arg, &path, segs[0], segs.get(1).copied(), "use ");
                        // An alias of a std crate would hide the modules it reaches.
                        if ROOTS_STD.contains(&segs[0]) && (segs.len() == 1 || (segs.len() == 2 && segs[1] == "self")) {
                            self.flag(arg, format!("`use {path}`: a hole names std's modules directly, never through an alias"));
                        }
                        if ROOTS_STD.contains(&segs[0]) && segs.last() == Some(&"*") {
                            self.std_glob = true;
                        }
                        if let Some(b) = bound {
                            self.roots.insert(b);
                        }
                    }
                }
                return;
            }
            "type_parameters" => {
                let mut c = n.walk();
                for p in n.named_children(&mut c) {
                    let id = p.child_by_field_name("name").unwrap_or(p);
                    self.roots.insert(text(id, self.src).to_string());
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        let kids: Vec<Node> = n.children(&mut c).collect();
        for k in kids {
            self.node(k);
        }
    }

    /// `use a::{b, c::d as e}`: each leaf's full path, and the name it binds.
    fn expand(&self, n: Node, prefix: String, out: &mut Vec<(String, Option<String>)>) {
        let join = |p: &str, x: &str| if p.is_empty() { x.to_string() } else { format!("{p}::{x}") };
        match n.kind() {
            "use_as_clause" => {
                let p = n.child_by_field_name("path").map(|p| text(p, self.src)).unwrap_or("");
                let a = n.child_by_field_name("alias").map(|a| text(a, self.src).to_string());
                out.push((join(&prefix, p), a));
            }
            "scoped_use_list" => {
                let p = n.child_by_field_name("path").map(|p| text(p, self.src)).unwrap_or("");
                let pre = join(&prefix, p);
                if let Some(list) = n.child_by_field_name("list") {
                    let mut c = list.walk();
                    for k in list.named_children(&mut c) {
                        self.expand(k, pre.clone(), out);
                    }
                }
            }
            "use_list" => {
                let mut c = n.walk();
                for k in n.named_children(&mut c) {
                    self.expand(k, prefix.clone(), out);
                }
            }
            "use_wildcard" => out.push((join(&prefix, text(n, self.src).trim_end_matches("::*")) + "::*", None)),
            _ => {
                let t = text(n, self.src);
                let last = t.rsplit("::").next().unwrap_or(t).to_string();
                let bound = if last == "self" { prefix.rsplit("::").next().map(String::from) } else { Some(last) };
                out.push((join(&prefix, t), bound));
            }
        }
    }

    /// A macro's tokens are unparsed: every path in them and every macro
    /// they call goes through the same rules, read as text.
    fn tokens(&mut self, tt: Node) {
        let t = text(tt, self.src).to_string();
        let chars: Vec<char> = t.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // Skip string and char literals: their text names nothing.
            if chars[i] == '"' {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == ':') {
                i += 1;
            }
            if i == start {
                i += 1;
                continue;
            }
            let w: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let is_macro = j < chars.len() && chars[j] == '!' && chars.get(j + 1) != Some(&'=');
            let preceded_by_dot = start > 0 && chars[start - 1] == '.';
            let segs: Vec<&str> = w.split("::").collect();
            if is_macro && !preceded_by_dot && !MACROS.contains(&w.as_str()) {
                self.flag(tt, format!("`{w}!` in macro tokens: a macro outside the allowed set"));
            }
            if segs.len() < 2 || preceded_by_dot {
                continue;
            }
            if w.starts_with("::") {
                self.flag(tt, format!("`{w}` in macro tokens: a leading `::` names an extern crate"));
            } else if segs[0] == "crate" || segs[0] == "super" {
                self.flag(tt, format!("`{w}` in macro tokens leaves the hole"));
            } else if ROOTS_STD.contains(&segs[0]) {
                if segs.get(1).is_some_and(|s| DENIED_MODS.contains(s)) {
                    self.flag(tt, format!("`{w}` in macro tokens reaches the world"));
                }
            } else if !(self.roots.contains(segs[0]) || STD_PRELUDE.contains(&segs[0]) || segs[0] == "self" || segs[0] == "Self")
                && !(self.std_glob && segs[0].chars().next().is_some_and(|c| c.is_ascii_uppercase()))
            {
                self.flag(tt, format!("`{w}` in macro tokens: `{}` is neither the prelude, std, nor an item of this file", segs[0]));
            }
        }
    }
}

/// The problems of one hole file, `(line, message)`: it defines exactly its
/// declared holes as `pub` fns and reaches only its parameters, its `ctx`,
/// the frame's prelude (`prelude`), std minus I/O and ambient state, and
/// items it defines itself. `#[cfg(test)]` items are its tests: skipped.
pub fn lint_hole(src: &[u8], holes: &[String], prelude: &[String]) -> Vec<(usize, String)> {
    let mut parser = Parser::new();
    if parser.set_language(&tree_sitter_rust::LANGUAGE.into()).is_err() {
        return vec![(0, "no Rust grammar".into())];
    }
    let Some(tree) = parser.parse(src, None) else { return vec![(0, "parse failed".into())] };
    let mut l =
        Lint { src, holes: holes.iter().cloned().collect(), roots: BTreeSet::new(), prelude: BTreeSet::new(), std_glob: false, out: Vec::new() };
    let root = tree.root_node();
    // Items the file defines, at any depth (a fn-local enum too).
    fn items(n: Node, src: &[u8], out: &mut BTreeSet<String>) {
        if matches!(n.kind(), "function_item" | "struct_item" | "enum_item" | "type_item" | "trait_item" | "const_item" | "union_item" | "mod_item")
            && let Some(name) = n.child_by_field_name("name")
        {
            out.insert(text(name, src).to_string());
        }
        let mut c = n.walk();
        for k in n.named_children(&mut c) {
            items(k, src, out);
        }
    }
    items(root, src, &mut l.roots);
    let local = l.roots.clone();
    l.prelude = prelude.iter().filter(|p| !local.contains(*p)).cloned().collect();
    l.roots.extend(prelude.iter().cloned());
    l.top(root);
    if root.has_error() {
        l.out.push((0, "syntax error".into()));
    }
    l.out.sort();
    l.out.dedup();
    l.out
}

/// Every problem of the frames' files under `root`: a missing hole file, a
/// lint finding, a file under a skeleton directory the frame does not
/// generate, or a hole file of a fn the frame no longer declares.
pub fn check_holes(root: &Path, cfg: &ArchConfig, rendered: &Rendered) -> Vec<String> {
    let mut out = Vec::new();
    let listed = |dir: &str| crate::arch::expand(root, &[dir.to_string()]).unwrap_or_default();
    for fr in &cfg.frames {
        let dir = fr.gen_dir();
        if root.join(&dir).exists() {
            for f in listed(&dir).into_iter().filter(|f| !rendered.skeleton.iter().any(|(p, _)| p == f)) {
                out.push(format!("{}: no file of frame {}'s skeleton: delete it", f.display(), fr.module));
            }
        }
        let holes = format!("{}/holes", fr.src.trim_end_matches('/'));
        if root.join(&holes).exists() {
            for f in listed(&holes).into_iter().filter(|f| !rendered.holes.iter().any(|h| h.file == *f)) {
                out.push(format!("{}: a hole file of no fn of frame {}: delete it", f.display(), fr.module));
            }
        }
    }
    for h in &rendered.holes {
        let Ok(src) = std::fs::read(root.join(&h.file)) else {
            out.push(format!("{}: missing hole file (run `dashboard arch gen` for a starter)", h.file.display()));
            continue;
        };
        for (line, msg) in lint_hole(&src, &h.holes, &h.prelude) {
            out.push(format!("{}:{line}: {msg}", h.file.display()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lint(src: &str) -> Vec<String> {
        lint_hole(src.as_bytes(), &["on_go".to_string(), "on_stop".to_string()], &["Plan".into(), "DecideCtx".into(), "Score".into()])
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    const OK: &str = "use std::collections::BTreeMap;\n\
        pub fn on_go(ctx: &mut dyn DecideCtx, s: Score) -> Score { helper(s) }\n\
        pub fn on_stop(ctx: &mut dyn DecideCtx) -> Score { let m: BTreeMap<i64, i64> = BTreeMap::new(); Score::new(m.len() as i64) }\n\
        fn helper(s: Score) -> Score { s }\nstruct Local(i64);\nimpl Clone for Local { fn clone(&self) -> Self { Local(self.0) } }\n\
        #[cfg(test)]\nmod tests { use super::*; static X: i64 = 1; }\n";

    #[test]
    fn a_hole_reaches_its_context_and_std_only() {
        assert_eq!(lint(OK), Vec::<String>::new());
        for (escape, why) in [
            ("fn x() { crate::frame::decide(); }", "`crate::` leaves the hole"),
            ("fn x() { super::super::skeleton::score(); }", "`super::` leaves the hole"),
            ("fn x() { std::fs::read(\"/etc\"); }", "`std::fs` reaches the world"),
            ("use std::{io as q};", "`std::io` reaches the world"),
            ("fn x() { println!(\"hi\"); }", "`println!`: a macro outside"),
            ("static N: i64 = 0;", "`static`"),
            ("fn x() { unsafe { } }", "`unsafe` block"),
            ("pub fn helper2() {}", "only the declared holes are `pub`"),
            ("impl Clone for Score { fn clone(&self) -> Self { todo!() } }", "its own types only"),
            ("macro_rules! m { () => {} }", "defines no macros"),
            ("fn x() { format!(\"{}\", crate::X); }", "in macro tokens leaves the hole"),
            ("extern crate std as s2;", "`extern crate`"),
            ("#[cfg_attr(all(), path = \"x.rs\")] mod y {}", "`#[cfg_attr]`"),
            ("fn x() { ::other::f(); }", "a leading `::`"),
            ("fn x() { thread_local!(static T: i64 = 0); }", "`thread_local!`"),
            ("fn x() { other_mod::f(); }", "neither the prelude"),
            ("fn x() { let _ = vec![include!(\"../evil.txt\")]; }", "`include!` in macro tokens"),
            ("fn x() { let _ = format!(\"{}\", env!(\"HOME\")); }", "`env!` in macro tokens"),
            ("use std as s;", "never through an alias"),
            ("use std::{self as s};", "never through an alias"),
            ("fn x() { let _ = format!(\"{:?}\", tokio::fs::read(\"/x\")); }", "`tokio` is neither"),
            ("#[derive(serde::Serialize)] struct S;", "derives std traits only"),
            ("#[tokio::main] fn y() {}", "`#[tokio::main]`"),
            ("#[doc = include_str!(\"/etc/passwd\")] fn y() {}", "a plain string only"),
            ("#[cfg(testing)] fn y() {}", "`#[cfg(…)]`"),
            ("//! docs\n", "inner doc comment"),
            ("#![allow(dead_code)]\n", "inner attribute"),
        ] {
            let found = lint(&format!("{OK}{escape}\n")).join("\n");
            assert!(found.contains(why), "{escape}: {found}");
        }
        // Ordinary code stays clean.
        let fine = format!(
            "{OK}fn y() -> i64 {{ enum E {{ A }} let _ = <i64 as Clone>::clone(&1); match E::A {{ E::A => 1 }} }}\n\
             use std::collections::*;\nfn z() {{ let _: HashMap<i64, i64> = HashMap::new(); let v = vec![Score::new(1)]; let _ = v.len(); }}\n\
             #[derive(Clone, Debug)]\nstruct T(i64);\n#[inline]\nfn w(t: &T) -> T {{ t.clone() }}\n"
        );
        assert_eq!(lint(&fine), Vec::<String>::new());
        assert!(lint("pub fn on_go() {}\nfn on_stop() {}\n").join("\n").contains("hole `on_stop` must be `pub`"));
        assert!(lint("pub fn on_go() {}\n").join("\n").contains("declared hole `on_stop` is missing"));
    }
}
