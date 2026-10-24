// Elements and containers end to end: one module per class (foldable,
// functor, traversable, monoid) with a hand-written generic instance, a data
// API instantiating the container with its element, and a consumer folding
// over it. Every module's `test_cmd` compiles its jail view with `rustc
// --test`, so the generated law tests run against the real instances.

use dashboard::arch::{Loaded, check, sync_bindings};
use dashboard::config::arch_config_from_json;
use dashboard::idl::Idl;
use dashboard::sandbox::Sandbox;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn test_cmd(module: &str) -> String {
    format!("rustc --edition 2024 --test -A warnings engine/{module}/lib.rs -o view.test && ./view.test -q")
}

fn manifest() -> Value {
    let modules: Vec<Value> = ["foldable", "functor", "traversable", "monoid", "db", "nutrition"]
        .iter()
        .map(|m| json!({"name": m, "lang": "rust", "paths": [format!("engine/{m}")], "test_cmd": test_cmd(m)}))
        .collect();
    let mut apis: Vec<Value> = [
        ("foldable", "seq.rs"),
        ("functor", "seq_functor.rs"),
        ("traversable", "seq_traversable.rs"),
        ("monoid", "seq_monoid.rs"),
    ]
    .iter()
    .map(|(class, impl_file)| {
        json!({"name": format!("{class}-nutrition"), "generator": class, "consumer": "nutrition",
               "paths": [format!("engine/api/{class}-nutrition")], "impl": [format!("engine/{class}/{impl_file}")]})
    })
    .collect();
    apis.push(json!({"name": "db-nutrition", "generator": "db", "consumer": "nutrition",
                     "paths": ["engine/api/db-nutrition"], "max_elements": 2, "max_fields": 4}));
    json!({"module": modules, "api": apis})
}

/// Every API's IDL, as the Lean export holds it.
fn idls() -> Vec<Value> {
    let mut out = vec![json!({"api": "foldable-nutrition", "containers": [{"name": "Seq", "kind": {"generic": ["Foldable"]}}]})];
    for class in ["Functor", "Traversable", "Monoid"] {
        out.push(json!({"api": format!("{}-nutrition", class.to_lowercase()),
                        "instances": [{"class": class, "api": "foldable-nutrition", "container": "Seq"}]}));
    }
    let string = json!({"repr": "string", "max_len": 64});
    out.push(json!({
        "api": "db-nutrition",
        "elements": [{"name": "Row", "variants": [
            {"name": "Consumption", "fields": [{"repr": "f64"}, string]},
            {"name": "Product", "fields": [{"repr": "i64"}, string]}]}],
        "containers": [{"name": "Rows", "kind": {"of": ["Row", "foldable-nutrition", "Seq"]}}],
        "types": [{"name": "Reason", "kind": {"repr": "string"}, "max_len": 64}],
        "ctors": [{"name": "Success", "args": ["Rows"]}, {"name": "Failure", "args": ["Reason"]}]
    }));
    out
}

fn loaded(manifest: Value) -> Loaded {
    let mut l = Loaded { cfg: arch_config_from_json(manifest).unwrap(), ..Default::default() };
    for v in idls() {
        let idl: Idl = serde_json::from_value(v).unwrap();
        l.api_dirs.insert(idl.api.clone(), PathBuf::from(format!("engine/api/{}", idl.api)));
        l.idls.insert(idl.api.clone(), idl);
    }
    l
}

/// `#[path]` mounts of the generated bindings a module's crate uses.
fn mounts(apis: &[&str]) -> String {
    apis.iter()
        .map(|a| format!("#[path = \"../api/{a}/gen/{}.rs\"]\npub mod {};\n", a.replace('-', "_"), a.replace('-', "_")))
        .collect()
}

const SEQ: &str = r#"
use super::Foldable;
pub struct Seq<A>(Vec<A>);
impl<A> Seq<A> {
    pub fn new(v: Vec<A>) -> Self { Seq(v) }
}
impl<A> Foldable<A> for Seq<A> {
    fn foldr<B>(&self, mut f: impl FnMut(&A, B) -> B, z: B) -> B {
        self.0.iter().rev().fold(z, |acc, x| f(x, acc))
    }
}
"#;

const FUNCTOR: &str = r#"
use super::{Foldable, Functor, Seq};
impl<A> Functor<A> for Seq<A> {
    type Of<B> = Seq<B>;
    fn fmap<B>(&self, mut f: impl FnMut(&A) -> B) -> Seq<B> {
        let mut v = self.foldr(|x, mut acc: Vec<B>| { acc.push(f(x)); acc }, Vec::new());
        v.reverse();
        Seq::new(v)
    }
}
"#;

const TRAVERSABLE: &str = r#"
use super::{Foldable, Traversable, Seq};
impl<A> Traversable<A> for Seq<A> {
    type Of<B> = Seq<B>;
    fn traverse<B, E>(&self, mut f: impl FnMut(&A) -> Result<B, E>) -> Result<Seq<B>, E> {
        // Right to left: an earlier element's failure replaces a later one's.
        let r = self.foldr(
            |x, acc: Result<Vec<B>, E>| match (f(x), acc) {
                (Err(e), _) | (Ok(_), Err(e)) => Err(e),
                (Ok(b), Ok(mut v)) => { v.push(b); Ok(v) }
            },
            Ok(Vec::new()),
        );
        r.map(|mut v| { v.reverse(); Seq::new(v) })
    }
}
"#;

const MONOID: &str = r#"
use super::{Foldable, Monoid, Seq};
fn items<A: Clone>(s: &Seq<A>) -> Vec<A> {
    let mut v = s.foldr(|x, mut acc: Vec<A>| { acc.push(x.clone()); acc }, Vec::new());
    v.reverse();
    v
}
impl<A: Clone> Monoid for Seq<A> {
    fn empty() -> Self { Seq::new(Vec::new()) }
    fn append(self, other: Self) -> Self {
        let mut v = items(&self);
        v.extend(items(&other));
        Seq::new(v)
    }
}
"#;

const CONSUMER: &str = r#"
use crate::db_nutrition::{DbNutrition, Row, Rows};
use crate::foldable_nutrition::Foldable;
use crate::functor_nutrition::Functor;
use crate::monoid_nutrition::{Monoid, fold_map};

struct Kcal(f64);
impl Monoid for Kcal {
    fn empty() -> Self { Kcal(0.0) }
    fn append(self, other: Self) -> Self { Kcal(self.0 + other.0) }
}

fn kcal(r: &Row) -> f64 {
    match r {
        Row::Consumption(k, _) => *k,
        Row::Product(..) => 0.0,
    }
}

pub fn total(rows: &Rows) -> f64 {
    rows.foldr(|r, acc| acc + kcal(r), 0.0)
}

pub fn report(x: &DbNutrition) -> f64 {
    match x {
        DbNutrition::Success(rows) => {
            let doubled = rows.fmap(|r| kcal(r) * 2.0);
            doubled.foldr(|k, acc| acc + k, 0.0) + fold_map(rows, |r| Kcal(kcal(r))).0
        }
        DbNutrition::Failure(_) => -1.0,
    }
}

#[test]
fn folds_over_rows() {
    let rows = Rows::new(vec![
        Row::Consumption(500.0, "2026-10-16".to_string()),
        Row::Product(3, "oats".to_string()),
        Row::Consumption(250.0, "2026-10-16".to_string()),
    ]);
    assert_eq!(total(&rows), 750.0);
    assert_eq!(report(&DbNutrition::Success(rows)), 2250.0);
}
"#;

fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "engine/foldable/seq.rs", SEQ);
    write(r, "engine/foldable/lib.rs", &mounts(&["foldable-nutrition"]));
    write(r, "engine/functor/seq_functor.rs", FUNCTOR);
    write(r, "engine/functor/lib.rs", &mounts(&["foldable-nutrition", "functor-nutrition"]));
    write(r, "engine/traversable/seq_traversable.rs", TRAVERSABLE);
    write(r, "engine/traversable/lib.rs", &mounts(&["foldable-nutrition", "traversable-nutrition"]));
    write(r, "engine/monoid/seq_monoid.rs", MONOID);
    write(r, "engine/monoid/lib.rs", &mounts(&["foldable-nutrition", "monoid-nutrition"]));
    write(
        r,
        "engine/db/lib.rs",
        &format!(
            "{}\npub fn query() -> db_nutrition::DbNutrition {{\n    use db_nutrition::*;\n    \
             DbNutrition::Success(Rows::new(vec![Row::Product(1, \"oats\".to_string())]))\n}}\n",
            mounts(&["foldable-nutrition", "db-nutrition"])
        ),
    );
    let all = ["foldable-nutrition", "functor-nutrition", "traversable-nutrition", "monoid-nutrition", "db-nutrition"];
    write(r, "engine/nutrition/lib.rs", &format!("{}\nmod consumer;\n", mounts(&all)));
    write(r, "engine/nutrition/consumer.rs", CONSUMER);
    let (_, errs) = sync_bindings(r, &loaded(manifest()), true);
    assert!(errs.is_empty(), "{errs:?}");
    d
}

fn problems(root: &Path) -> String {
    let scratch = tempfile::tempdir().unwrap();
    let rep = check(root, &loaded(manifest()), &Sandbox::off(), scratch.path());
    rep.problems.join("\n")
}

#[test]
fn class_modules_pass_their_laws_and_the_consumer_folds() {
    let d = fixture();
    let all = problems(d.path());
    assert!(all.is_empty(), "{all}");
    let out = std::fs::read_to_string(d.path().join("engine/api/db-nutrition/gen/db_nutrition.rs")).unwrap();
    assert!(out.contains("pub enum Row {\n    Consumption(f64, String),"), "{out}");
    assert!(out.contains("pub type Rows = super::foldable_nutrition::Seq<Row>;"), "{out}");
    let out = std::fs::read_to_string(d.path().join("engine/api/monoid-nutrition/gen/monoid_nutrition.rs")).unwrap();
    assert!(out.contains("#[path = \"../../../../engine/monoid/seq_monoid.rs\"]"), "{out}");
    assert!(out.contains("pub trait Monoid") && out.contains("fn monoid_seq_laws()"), "{out}");
}

#[test]
fn a_broken_instance_fails_its_laws() {
    let d = fixture();
    // fmap that drops the first element: compiles, breaks `fmap id == id`.
    write(d.path(), "engine/functor/seq_functor.rs", &FUNCTOR.replace("v.reverse();", "v.reverse(); if !v.is_empty() { v.remove(0); }"));
    let all = problems(d.path());
    assert!(all.contains("module functor: tests in its jail view"), "{all}");
    assert!(all.contains("fmap id == id"), "{all}");
    // Append that loses the right side breaks the monoid laws.
    let d = fixture();
    write(d.path(), "engine/monoid/seq_monoid.rs", &MONOID.replace("v.extend(items(&other));", "let _ = other;"));
    let all = problems(d.path());
    assert!(all.contains("module monoid: tests in its jail view"), "{all}");
}

#[test]
fn a_missing_instance_is_reported() {
    let d = fixture();
    write(d.path(), "engine/traversable/seq_traversable.rs", "// nothing yet\n");
    let all = problems(d.path());
    assert!(all.contains("api traversable-nutrition: class Traversable declared for Seq, but no `impl Traversable for Seq`"), "{all}");
}

#[test]
fn the_consumer_cannot_reach_into_the_container_and_must_be_total() {
    for (bad, want) in [
        ("rows.0.len() as f64", "tests in its jail view"),
        ("rows[0]", "tests in its jail view"),
        ("rows.len() as f64", "tests in its jail view"),
    ] {
        let d = fixture();
        let src = CONSUMER.replace("rows.foldr(|r, acc| acc + kcal(r), 0.0)", bad);
        write(d.path(), "engine/nutrition/consumer.rs", &src);
        let all = problems(d.path());
        assert!(all.contains(&format!("module nutrition: {want}")), "{bad}: {all}");
    }
    let d = fixture();
    let src = CONSUMER.replace("Row::Product(..) => 0.0,", "_ => 0.0,");
    write(d.path(), "engine/nutrition/consumer.rs", &src);
    let all = problems(d.path());
    assert!(all.contains("engine/nutrition/consumer.rs:16: wildcard arm in a match over Row: an algebra must be total in Row"), "{all}");
}

#[test]
fn element_budgets_show_as_headroom() {
    let d = fixture();
    let mut m = manifest();
    m["api"][4]["max_elements"] = json!(1);
    m["api"][4]["max_fields"] = json!(2);
    let scratch = tempfile::tempdir().unwrap();
    let rep = check(d.path(), &loaded(m), &Sandbox::off(), scratch.path());
    assert!(rep.headroom.iter().any(|h| h.contains("element variants 2/1, fields 4/2")), "{:?}", rep.headroom);
}
