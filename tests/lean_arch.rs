// The Lean architecture (handover §0): a project's `architecture/` package
// is exported through `lake` and loaded like the TOML + KDL one; `dashboard
// arch migrate` converts the latter into it. These tests run the real Lean
// toolchain (`lake` must be on the host, as on the player's).

mod common;

use common::*;
use dashboard::arch::{self, Loaded, check, sync_bindings};
use dashboard::config::arch_config_from_str;
use dashboard::idl::Idl;
use dashboard::lean;
use dashboard::sandbox::Sandbox;
use std::path::Path;

fn report(root: &Path, loaded: &Loaded) -> (Vec<String>, Vec<String>) {
    let scratch = tempfile::tempdir().unwrap();
    let rep = check(root, loaded, &Sandbox::off(), scratch.path());
    (rep.problems, rep.headroom)
}

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    assert!(lean::available(), "these tests need `lake` (install elan: https://lean-lang.org)");
    write_file(d.path(), "a/lib.rs", "pub fn f() -> u32 { 1 }\n");
    write_file(d.path(), "b/lib.rs", "pub fn g() {}\n");
    write_file(d.path(), "api/.keep", "");
    d
}

#[test]
fn loads_a_lean_architecture_generates_bindings_and_checks_it() {
    let d = project();
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", LEAN_AB_IDL)]);
    let loaded = arch::load(d.path()).expect("load");
    assert!(loaded.lean && loaded.errors.is_empty(), "{:?}", loaded.errors);
    assert_eq!(loaded.cfg.modules.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(loaded.cfg.modules[0].max_ast, 100);
    let ab = &loaded.idls["ab"];
    assert_eq!((ab.constructors(), ab.slots()), (2, 1));
    assert_eq!(ab.type_decl("Id").unwrap().max_len, 16);
    let (wrote, errs) = sync_bindings(d.path(), &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(wrote, ["api/gen/ab.rs"]);
    assert!(std::fs::read_to_string(d.path().join("api/gen/ab.rs")).unwrap().contains("pub enum Ab {\n    X(Id),\n    Y,\n}"));
    let (problems, headroom) = report(d.path(), &loaded);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(headroom.iter().any(|h| h == "api ab: constructors 2, T slots 1, string max_len 16/256"), "{headroom:?}");
    // A vendored file that differs (here: another toolchain, which `lake`
    // would download) stops the load until `dashboard arch gen` restores it.
    write_file(d.path(), "architecture/lean-toolchain", "leanprover/lean4:v4.0.0\n");
    let e = arch::load(d.path()).unwrap_err();
    assert!(e.starts_with("architecture/lean-toolchain: vendored"), "{e}");
    let (wrote, _) = sync_bindings(d.path(), &loaded, true);
    assert_eq!(wrote, ["architecture/lean-toolchain"]);
    assert!(arch::load(d.path()).is_ok());
}

#[test]
fn lean_type_checks_the_manifest_and_the_tool_checks_the_rest() {
    let d = project();
    // An undeclared module, an unknown field, a ref to a type that isn't there.
    write_lean_arch(d.path(), &LEAN_AB_MANIFEST.replace("api ab : a → b", "api ab : a → nope"), &[("ab", LEAN_AB_IDL)]);
    let e = arch::load(d.path()).unwrap_err();
    assert!(e.contains("nope"), "{e}");
    write_lean_arch(d.path(), &LEAN_AB_MANIFEST.replace("max_ast = 100", "max_apples = 100"), &[("ab", LEAN_AB_IDL)]);
    assert!(arch::load(d.path()).unwrap_err().contains("max_apples"));
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", &LEAN_AB_IDL.replace("constructor X : Id", "constructor X : Nope"))]);
    assert!(arch::load(d.path()).unwrap_err().contains("Nope"));
    // Lean can't know a type is unused or a string unbounded: the tool does.
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", &LEAN_AB_IDL.replace("constructor X : Id", "constructor X"))]);
    assert!(arch::load(d.path()).unwrap_err().contains("never used"));
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", &LEAN_AB_IDL.replace("string 16", "string"))]);
    let loaded = arch::load(d.path()).unwrap();
    let (problems, _) = report(d.path(), &loaded);
    assert!(problems.iter().any(|p| p.contains("type Id: a string or bytes type needs a max_len")), "{problems:?}");
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", &LEAN_AB_IDL.replace("string 16", "string 300"))]);
    let (problems, _) = report(d.path(), &arch::load(d.path()).unwrap());
    assert!(problems.iter().any(|p| p.contains("string max_len 300 exceeds the api's max_string_len 256")), "{problems:?}");
    let raised = LEAN_AB_MANIFEST.replace("paths = [\"api\"]", "paths = [\"api\"]\n  max_string_len = 400");
    write_lean_arch(d.path(), &raised, &[("ab", &LEAN_AB_IDL.replace("string 16", "string 300"))]);
    let (problems, headroom) = report(d.path(), &arch::load(d.path()).unwrap());
    assert!(!problems.iter().any(|p| p.contains("max_string_len")), "{problems:?}");
    assert!(headroom.iter().any(|h| h.contains("string max_len 300/400")), "{headroom:?}");
}

const MANIFEST3: &str = "import Dashboard\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n\
module foldable where\n  lang = rust\n  paths = [\"foldable\"]\n\n\
module db where\n  lang = rust\n  paths = [\"db\"]\n\n\
module nutrition where\n  lang = rust\n  paths = [\"nutrition\"]\n\n\
api «foldable-db» : foldable → db where\n  paths = [\"apis/foldable-db\"]\n  impl = [\"foldable/seq.rs\"]\n\n\
api «nutrition-db» : db → nutrition where\n  paths = [\"apis/nutrition-db\"]\n  max_fns = 3\n  max_constructors = 4\n";

const FOLDABLE_DB: &str = "import Architecture\nopen Dashboard\n\nidl «foldable-db» where\n  container Seq : Foldable\n";

const NUTRITION_DB: &str = "import Architecture\nimport Apis.«foldable-db»\nopen Dashboard\n\nidl «nutrition-db» where\n  \
type Name : string 64\n  type Grams : f64\n  type Id : i64 display store\n  type Reason : string 200 display\n  \
element Consumption\n    | Consumption : i64 → f64 → string 16\n    | Empty\n  \
container Consumptions := «foldable-db».Seq Consumption\n  \
sum Written\n    | Written : Id\n    | Rejected : Reason\n    | Tick\n  \
sum Rows\n    | Rows : Consumptions\n  \
fn insertProduct : Name → Grams → Written\n  fn log : Consumptions → Written\n  fn rowsForDay : Rows\n";

#[test]
fn fns_sums_elements_and_containers_export_with_their_measures() {
    let d = project();
    for m in ["foldable", "db", "nutrition"] {
        write_file(d.path(), &format!("{m}/lib.rs"), "");
    }
    write_file(d.path(), "foldable/seq.rs", "use super::Foldable;\npub struct Seq<A>(Vec<A>);\nimpl<A> Foldable<A> for Seq<A> { fn foldr<B>(&self, mut f: impl FnMut(&A, B) -> B, z: B) -> B { self.0.iter().rev().fold(z, |acc, x| f(x, acc)) } }\n");
    write_file(d.path(), "apis/foldable-db/.keep", "");
    write_file(d.path(), "apis/nutrition-db/.keep", "");
    write_lean_arch(d.path(), MANIFEST3, &[("foldable-db", FOLDABLE_DB), ("nutrition-db", NUTRITION_DB)]);
    let loaded = arch::load(d.path()).expect("load");
    let idl = &loaded.idls["nutrition-db"];
    assert_eq!((idl.fn_count(), idl.inputs(), idl.constructors(), idl.slots()), (3, 3, 4, 3));
    assert_eq!(idl.sums.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Written", "Rows"]);
    assert_eq!(idl.fns[2].returns, "Rows");
    assert!(idl.type_decl("Id").unwrap().store && idl.type_decl("Id").unwrap().display);
    assert_eq!(idl.elements[0].variants[0].fields[2].max_len, 16);
    assert_eq!(idl.container_refs(), [("foldable-db", "Seq")]);
    let (_, errs) = sync_bindings(d.path(), &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
    let (problems, headroom) = report(d.path(), &loaded);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(headroom.iter().any(|h| h.contains("api nutrition-db: constructors 4/4, T slots 3, fns 3/3, inputs 3")), "{headroom:?}");
    // A fn folding a container means the generator consumes that class API:
    // here `db` does. Point the class API at `nutrition` instead and it doesn't.
    let m = MANIFEST3.replace("api «foldable-db» : foldable → db", "api «foldable-db» : foldable → nutrition");
    write_lean_arch(d.path(), &m, &[("foldable-db", FOLDABLE_DB), ("nutrition-db", NUTRITION_DB)]);
    let (problems, _) = report(d.path(), &arch::load(d.path()).unwrap());
    assert!(problems.iter().any(|p| p.contains("generator db must consume foldable-db")), "{problems:?}");
}

const TOML3: &str = "max_modules = 4\n\
[[module]]\nname = \"foldable\"\nlang = \"rust\"\npaths = [\"foldable\"]\n\
[[module]]\nname = \"monoid\"\nlang = \"rust\"\npaths = [\"monoid\"]\n\
[[module]]\nname = \"db\"\nlang = \"rust\"\npaths = [\"db\"]\nmax_bytes = 5000\n\
[[module]]\nname = \"nutrition\"\nlang = \"rust\"\npaths = [\"nutrition\"]\ntest_cmd = \"true\"\n\
[[api]]\nname = \"foldable-nutrition\"\ngenerator = \"foldable\"\nconsumer = \"nutrition\"\npaths = [\"apis/foldable-nutrition\"]\nimpl = [\"foldable/seq.rs\"]\n\
[[api]]\nname = \"monoid-nutrition\"\ngenerator = \"monoid\"\nconsumer = \"nutrition\"\npaths = [\"apis/monoid-nutrition\"]\nimpl = [\"monoid/seq.rs\"]\n\
[[api]]\nname = \"db-nutrition\"\ngenerator = \"db\"\nconsumer = \"nutrition\"\npaths = [\"apis/db-nutrition\"]\nmax_elements = 4\nmax_fields = 9\n";

const KDLS: [(&str, &str); 3] = [
    ("foldable-nutrition", "api \"foldable-nutrition\"\ncontainer \"Seq\" classes=\"Foldable\"\n"),
    ("monoid-nutrition", "api \"monoid-nutrition\"\ninstance \"Monoid\" for=\"foldable-nutrition.Seq\"\n"),
    (
        "db-nutrition",
        "api \"db-nutrition\"\nelement \"Row\" {\n    variant \"Meal\" \"f64\" \"string\"\n    variant \"Note\" \"string\"\n}\n\
         container \"Rows\" of=\"Row\" ref=\"foldable-nutrition.Seq\"\ntype \"Reason\" repr=\"string\" display=#true\ntype \"Id\" repr=\"i64\"\n\
         constructor \"Success\" \"Rows\"\nconstructor \"Failure\" \"Reason\" \"Id\"\nconstructor \"Nothing\"\n",
    ),
];

#[test]
fn migrate_round_trips_the_toml_and_kdl_architecture() {
    let d = project();
    for m in ["foldable", "monoid", "db", "nutrition"] {
        write_file(d.path(), &format!("{m}/lib.rs"), "");
    }
    write_file(d.path(), "architecture.toml", TOML3);
    for (name, kdl) in KDLS {
        write_file(d.path(), &format!("apis/{name}/{name}.api.kdl"), kdl);
    }
    let before = arch::load(d.path()).expect("toml load");
    assert!(!before.lean && before.errors.is_empty(), "{:?}", before.errors);
    let wrote = lean::migrate(d.path(), &before.cfg, &before.idls).expect("migrate");
    assert!(wrote.contains(&"architecture/Architecture.lean".to_string()) && wrote.contains(&"architecture/Apis/db-nutrition.lean".to_string()), "{wrote:?}");
    assert!(arch::load(d.path()).unwrap_err().contains("one architecture"));
    std::fs::remove_file(d.path().join("architecture.toml")).unwrap();
    for (name, _) in KDLS {
        std::fs::remove_file(d.path().join(format!("apis/{name}/{name}.api.kdl"))).unwrap();
    }
    let after = arch::load(d.path()).expect("lean load");
    assert!(after.lean);
    for (name, _) in KDLS {
        assert_eq!(after.idls[name], before.idls[name], "{name}");
    }
    let names = |c: &dashboard::config::ArchConfig| c.modules.iter().map(|m| (m.name.clone(), m.max_bytes, m.test_cmd.clone())).collect::<Vec<_>>();
    assert_eq!(names(&after.cfg), names(&before.cfg));
    let apis = |c: &dashboard::config::ArchConfig| {
        c.apis.iter().map(|a| (a.name.clone(), a.generator.clone(), a.consumer.clone(), a.paths.clone(), a.impls.clone(), a.max_elements, a.max_fields)).collect::<Vec<_>>()
    };
    assert_eq!(apis(&after.cfg), apis(&before.cfg));
    assert_eq!(after.cfg.max_modules, 4);
    // The TOML printers are the inverse of the parsers on this fixture too.
    assert_eq!(arch_config_from_str(TOML3).unwrap().lean(), before.cfg.lean());
    assert_eq!(Idl::parse(KDLS[2].1).unwrap().lean(), before.idls["db-nutrition"].lean());
    // The migrated IDL carries no bounds: the check says so until the
    // director adds them.
    let (problems, _) = report(d.path(), &after);
    assert!(problems.iter().any(|p| p.contains("api db-nutrition: type Reason: a string or bytes type needs a max_len")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("element Row: variant Meal: field 1 needs a max_len")), "{problems:?}");
}

const PROOFS_MANIFEST: &str = "import Dashboard\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n\
module db where\n  lang = rust\n  paths = [\"db\"]\n\n\
module nutrition where\n  lang = rust\n  paths = [\"nutrition\"]\n\n\
module web where\n  lang = rust\n  paths = [\"web\"]\n\n\
api «nutrition-db» : db → nutrition where\n  paths = [\"apis/nutrition-db\"]\n  max_rules = 9\n  max_tokens = 3\n\n\
api «nutrition-web» : nutrition → web where\n  paths = [\"apis/nutrition-web\"]\n";

/// Refinements, products and tokens (architect-v3.md, "Proofs supplied by
/// the caller").
const PROOFS_IDL: &str = "import Architecture\nopen Dashboard\n\nidl «nutrition-db» where\n  \
type Grams : f64 where 0 < v ∧ v ≤ 20000\n  \
type Day : string 10 where v.length = 10\n  \
type Reps : i64 where 1 ≤ v ∧ v ≤ 1000\n  \
type Span : i64 × i64 where v.1 < v.2\n  \
type Unit : string 8 where v ∈ [\"g\", \"ml\"]\n  \
type Pair : string 4 × f64 where v.1 ≠ \"\" ∧ v.2 < 1.5e3\n  \
type Id : i64 display store\n  \
token ProductRef : i64 permanent store\n  \
token EventTypeRef : i64 versioned stale=Retired\n  \
token Funds : i64 linear spent=AlreadyApplied\n  \
sum Kind | Food | Supplement | Recipe\n  \
sum Resolved | Resolved : ProductRef | Unknown\n  \
sum Written | Written : Id | Retired | AlreadyApplied\n  \
fn resolveProduct : Day → Resolved\n  \
fn log : ProductRef → Grams → Day → Reps → Span → Unit → Pair → Kind → Written\n  \
fn tick : EventTypeRef → Written\n  \
fn apply : Funds → Written\n";

const PROOFS_WEB: &str = "import Architecture\nimport Apis.«nutrition-db»\nopen Dashboard\n\nidl «nutrition-web» where\n  \
type EventTypeRef : ref «nutrition-db».EventTypeRef\n  \
sum Shown | Shown | Retired\n  \
fn render : EventTypeRef → Shown\n";

#[test]
fn refinements_and_tokens_are_elaborated_exported_budgeted_and_linted() {
    let d = project();
    let r = d.path();
    for m in ["db", "nutrition", "web"] {
        write_file(r, &format!("{m}/lib.rs"), "");
    }
    write_file(r, "apis/nutrition-db/.keep", "");
    write_file(r, "apis/nutrition-web/.keep", "");
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", PROOFS_IDL), ("nutrition-web", PROOFS_WEB)]);
    let loaded = arch::load(r).expect("load");
    let idl = &loaded.idls["nutrition-db"];
    assert_eq!((idl.rules(), idl.token_count(), idl.constructors(), idl.slots(), idl.inputs()), (9, 3, 8, 2, 11));
    assert_eq!(idl.type_decl("Pair").unwrap().pred.as_ref().unwrap().render(), "v.1 ≠ \"\" ∧ v.2 < 1500");
    assert_eq!(idl.token("EventTypeRef").unwrap().kind, dashboard::idl::TokenKind::Versioned("Retired".into()));
    let (_, errs) = sync_bindings(r, &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
    let binding = std::fs::read_to_string(r.join("apis/nutrition-db/gen/nutrition_db.rs")).unwrap();
    assert!(binding.contains("rule: \"0 < v ∧ v ≤ 20000\"") && binding.contains("(v.0.as_str() != \"\")") && binding.contains("(v.1 < 1500.0)"), "{binding}");
    let (problems, headroom) = report(r, &loaded);
    assert!(problems.is_empty(), "{problems:?}");
    assert!(headroom.iter().any(|h| h.contains("api nutrition-db: constructors 8, T slots 2, fns 4, inputs 11, string max_len 10/256, rules 9/9, tokens 3/3")), "{headroom:?}");
    // The fragment is checked at elaboration: Lean's own type check, and
    // the DSL's reading of `v`.
    for (from, to, want) in [
        ("where 0 < v ∧ v ≤ 20000", "where v.length = 3", "`.length` is for a string or bytes type"),
        ("where 0 < v ∧ v ≤ 20000", "where w < 3", "expected v, v.N, v.length or a literal"),
        ("where 0 < v ∧ v ≤ 20000", "where v.foo = 1", "expected v, v.N, v.length or a literal"),
        ("where 0 < v ∧ v ≤ 20000", "where v < \"x\"", "mismatch"),
        ("where 0 < v ∧ v ≤ 20000", "where v.1 < 3", "v.N is for a product type"),
        ("where v.1 < v.2", "where v.3 < 1", "v.3: the product has 2 components"),
        ("where v.1 < v.2", "where v < 1", "a product is compared by component"),
        ("i64 × i64 where v.1 < v.2", "i64 × i64 × i64 × i64 × i64 where v.1 < v.2", "two to four components"),
    ] {
        write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", &PROOFS_IDL.replace(from, to)), ("nutrition-web", PROOFS_WEB)]);
        let e = arch::load(r).unwrap_err();
        assert!(e.contains(want), "{to}: {e}");
    }
    // A versioned or linear token's failure must be in the signature of
    // every fn taking it: here, and through a re-export.
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", &PROOFS_IDL.replace("| Written : Id | Retired", "| Written : Id")), ("nutrition-web", PROOFS_WEB)]);
    let e = arch::load(r).unwrap_err();
    assert!(e.contains("fn tick: takes EventTypeRef (versioned), so Written must have a `Retired` constructor"), "{e}");
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", PROOFS_IDL), ("nutrition-web", &PROOFS_WEB.replace("| Shown | Retired", "| Shown"))]);
    let (problems, _) = report(r, &arch::load(r).unwrap());
    assert!(problems.iter().any(|p| p.contains("api nutrition-web: fn render: takes EventTypeRef (nutrition-db.EventTypeRef, versioned), so Shown must have a `Retired` constructor")), "{problems:?}");
    // Budgets, and the mint lint: a consumer minting evidence is caught, the
    // generator is not.
    write_lean_arch(r, &PROOFS_MANIFEST.replace("max_rules = 9", "max_rules = 8").replace("max_tokens = 3", "max_tokens = 2"), &[("nutrition-db", PROOFS_IDL), ("nutrition-web", PROOFS_WEB)]);
    write_file(r, "nutrition/lib.rs", "fn f() { let _p = ProductRef::mint(1); }\n");
    write_file(r, "db/lib.rs", "fn f() { let _p = ProductRef::mint(1); }\n");
    let (problems, _) = report(r, &arch::load(r).unwrap());
    assert!(problems.iter().any(|p| p == "api nutrition-db: refinement rules 9 exceeds budget 8"), "{problems:?}");
    assert!(problems.iter().any(|p| p == "api nutrition-db: tokens 3 exceeds budget 2"), "{problems:?}");
    assert!(problems.iter().any(|p| p == "nutrition/lib.rs:1: mints the evidence token ProductRef; only the generator of its api may"), "{problems:?}");
    assert!(!problems.iter().any(|p| p.starts_with("db/lib.rs")), "{problems:?}");
}
