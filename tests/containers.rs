// Elements and containers end to end: one module per class (foldable,
// functor, traversable, monoid) with a hand-written generic instance, a data
// API instantiating the container with its element, and a consumer folding
// over it. Every module's `test_cmd` compiles its jail view with `rustc
// --test`, so the generated law tests run against the real instances.

use dashboard::arch::{Loaded, check, sync_bindings};
use dashboard::config::arch_config_from_str;
use dashboard::sandbox::Sandbox;
use std::path::Path;

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn test_cmd(module: &str) -> String {
    format!("rustc --edition 2024 --test -A warnings engine/{module}/lib.rs -o view.test && ./view.test -q")
}

fn manifest() -> String {
    let mut m = String::new();
    for module in ["foldable", "functor", "traversable", "monoid", "db", "nutrition"] {
        m.push_str(&format!(
            "[[module]]\nname = \"{module}\"\nlang = \"rust\"\npaths = [\"engine/{module}\"]\ntest_cmd = \"{}\"\n\n",
            test_cmd(module)
        ));
    }
    for (class, impl_file) in [
        ("foldable", "seq.rs"),
        ("functor", "seq_functor.rs"),
        ("traversable", "seq_traversable.rs"),
        ("monoid", "seq_monoid.rs"),
    ] {
        m.push_str(&format!(
            "[[api]]\nname = \"{class}-nutrition\"\ngenerator = \"{class}\"\nconsumer = \"nutrition\"\n\
             paths = [\"engine/api/{class}-nutrition\"]\nimpl = [\"engine/{class}/{impl_file}\"]\n\n"
        ));
    }
    m.push_str(
        "[[api]]\nname = \"db-nutrition\"\ngenerator = \"db\"\nconsumer = \"nutrition\"\n\
         paths = [\"engine/api/db-nutrition\"]\nmax_elements = 2\nmax_fields = 4\n",
    );
    m
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
    write(r, "engine/api/foldable-nutrition/foldable-nutrition.api.kdl", "api \"foldable-nutrition\"\ncontainer \"Seq\" classes=\"Foldable\"\n");
    for class in ["Functor", "Traversable", "Monoid"] {
        let l = class.to_lowercase();
        write(
            r,
            &format!("engine/api/{l}-nutrition/{l}-nutrition.api.kdl"),
            &format!("api \"{l}-nutrition\"\ninstance \"{class}\" for=\"foldable-nutrition.Seq\"\n"),
        );
    }
    write(
        r,
        "engine/api/db-nutrition/db-nutrition.api.kdl",
        "api \"db-nutrition\"\nelement \"Row\" {\n    variant \"Consumption\" \"f64\" \"string\"\n    variant \"Product\" \"i64\" \"string\"\n}\n\
         container \"Rows\" of=\"Row\" ref=\"foldable-nutrition.Seq\"\ntype \"Reason\" repr=\"string\"\n\
         constructor \"Success\" \"Rows\"\nconstructor \"Failure\" \"Reason\"\n",
    );
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
    let cfg = Loaded::from_toml(r, arch_config_from_str(&manifest()).unwrap());
    let (_, errs) = sync_bindings(r, &cfg, true);
    assert!(errs.is_empty(), "{errs:?}");
    d
}

fn problems(root: &Path) -> String {
    let cfg = Loaded::from_toml(root, arch_config_from_str(&manifest()).unwrap());
    let scratch = tempfile::tempdir().unwrap();
    let rep = check(root, &cfg, &Sandbox::off(), scratch.path());
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
fn element_budgets_and_bad_impls() {
    let d = fixture();
    let m = manifest().replace("max_elements = 2\nmax_fields = 4", "max_elements = 1\nmax_fields = 2");
    let cfg = Loaded::from_toml(d.path(), arch_config_from_str(&m).unwrap());
    let scratch = tempfile::tempdir().unwrap();
    let rep = check(d.path(), &cfg, &Sandbox::off(), scratch.path());
    let all = rep.problems.join("\n");
    assert!(all.contains("api db-nutrition: element variants 2 exceeds budget 1"), "{all}");
    assert!(all.contains("element fields 4 exceeds budget 2"), "{all}");
    assert!(rep.headroom.iter().any(|h| h.contains("element variants 2/1, fields 4/2")), "{:?}", rep.headroom);
    let m = manifest().replace("impl = [\"engine/functor/seq_functor.rs\"]", "impl = [\"engine/foldable/seq.rs\"]");
    assert!(arch_config_from_str(&m).unwrap_err().contains("not under generator module functor's paths"));
}
