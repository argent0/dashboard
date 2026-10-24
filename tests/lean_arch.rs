// The Lean architecture (handover §0): a project's `architecture/` package
// is exported through `lake` and loaded. These tests run the real Lean
// toolchain (`lake` must be on the host, as on the player's).

mod common;

use common::*;
use dashboard::arch::{self, Loaded, check, sync_bindings};
use dashboard::config::Covers;
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
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", LEAN_AB_IDL)], &[]);
    let loaded = arch::load(d.path()).expect("load");
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
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
    write_lean_arch(d.path(), &LEAN_AB_MANIFEST.replace("api ab : a → b", "api ab : a → nope"), &[("ab", LEAN_AB_IDL)], &[]);
    let e = arch::load(d.path()).unwrap_err();
    assert!(e.contains("nope"), "{e}");
    write_lean_arch(d.path(), &LEAN_AB_MANIFEST.replace("max_ast = 100", "max_apples = 100"), &[("ab", LEAN_AB_IDL)], &[]);
    assert!(arch::load(d.path()).unwrap_err().contains("max_apples"));
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", &LEAN_AB_IDL.replace("constructor X : Id", "constructor X : Nope"))], &[]);
    assert!(arch::load(d.path()).unwrap_err().contains("Nope"));
    // Lean can't know a type is unused or a string unbounded: the tool does.
    // Every problem at once, not one per run.
    let unused = LEAN_AB_IDL.replace("constructor X : Id", "constructor X").replace("  type Id", "  type Extra : i64\n  type Id");
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", &unused)], &[]);
    let e = arch::load(d.path()).unwrap_err();
    assert!(e.contains("Apis/ab.lean: type Extra: declared but never used\n") && e.contains("Apis/ab.lean: type Id: declared but never used"), "{e}");
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", &LEAN_AB_IDL.replace("string 16", "string"))], &[]);
    let loaded = arch::load(d.path()).unwrap();
    let (problems, _) = report(d.path(), &loaded);
    assert!(problems.iter().any(|p| p.contains("type Id: a string or bytes type needs a max_len")), "{problems:?}");
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", &LEAN_AB_IDL.replace("string 16", "string 300"))], &[]);
    let (problems, _) = report(d.path(), &arch::load(d.path()).unwrap());
    assert!(problems.iter().any(|p| p.contains("string max_len 300 exceeds the api's max_string_len 256")), "{problems:?}");
    let raised = LEAN_AB_MANIFEST.replace("paths = [\"api\"]", "paths = [\"api\"]\n  max_string_len = 400");
    write_lean_arch(d.path(), &raised, &[("ab", &LEAN_AB_IDL.replace("string 16", "string 300"))], &[]);
    let (problems, headroom) = report(d.path(), &arch::load(d.path()).unwrap());
    assert!(!problems.iter().any(|p| p.contains("max_string_len")), "{problems:?}");
    assert!(headroom.iter().any(|h| h.contains("string max_len 300/400")), "{headroom:?}");
}

const MANIFEST3: &str = "import Architect\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n\
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
    write_lean_arch(d.path(), MANIFEST3, &[("foldable-db", FOLDABLE_DB), ("nutrition-db", NUTRITION_DB)], &[]);
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
    write_lean_arch(d.path(), &m, &[("foldable-db", FOLDABLE_DB), ("nutrition-db", NUTRITION_DB)], &[]);
    let (problems, _) = report(d.path(), &arch::load(d.path()).unwrap());
    assert!(problems.iter().any(|p| p.contains("generator db must consume foldable-db")), "{problems:?}");
    // A container only returned is the consumer's to fold: no demand on db.
    let returned = NUTRITION_DB.replace("fn log : Consumptions → Written", "fn log : Name → Written");
    write_lean_arch(d.path(), &m, &[("foldable-db", FOLDABLE_DB), ("nutrition-db", &returned)], &[]);
    let (problems, _) = report(d.path(), &arch::load(d.path()).unwrap());
    assert!(!problems.iter().any(|p| p.contains("must consume")), "{problems:?}");
    // Taken inside a sum, it is still an input.
    let in_sum = NUTRITION_DB.replace("fn log : Consumptions → Written", "fn log : Rows → Written");
    write_lean_arch(d.path(), &m, &[("foldable-db", FOLDABLE_DB), ("nutrition-db", &in_sum)], &[]);
    let (problems, _) = report(d.path(), &arch::load(d.path()).unwrap());
    assert!(problems.iter().any(|p| p.contains("generator db must consume foldable-db")), "{problems:?}");
}

const PROOFS_MANIFEST: &str = "import Architect\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n\
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
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", PROOFS_IDL), ("nutrition-web", PROOFS_WEB)], &[]);
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
        write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", &PROOFS_IDL.replace(from, to)), ("nutrition-web", PROOFS_WEB)], &[]);
        let e = arch::load(r).unwrap_err();
        assert!(e.contains(want), "{to}: {e}");
    }
    // A versioned or linear token's failure must be in the signature of
    // every fn taking it: here, and through a re-export.
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", &PROOFS_IDL.replace("| Written : Id | Retired", "| Written : Id")), ("nutrition-web", PROOFS_WEB)], &[]);
    let e = arch::load(r).unwrap_err();
    assert!(e.contains("fn tick: takes EventTypeRef (versioned), so Written must have a `Retired` constructor"), "{e}");
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", PROOFS_IDL), ("nutrition-web", &PROOFS_WEB.replace("| Shown | Retired", "| Shown"))], &[]);
    let (problems, _) = report(r, &arch::load(r).unwrap());
    assert!(problems.iter().any(|p| p.contains("api nutrition-web: fn render: takes EventTypeRef (nutrition-db.EventTypeRef, versioned), so Shown must have a `Retired` constructor")), "{problems:?}");
    // Budgets, and the mint lint: a consumer minting evidence is caught, the
    // generator is not.
    write_lean_arch(r, &PROOFS_MANIFEST.replace("max_rules = 9", "max_rules = 8").replace("max_tokens = 3", "max_tokens = 2"), &[("nutrition-db", PROOFS_IDL), ("nutrition-web", PROOFS_WEB)], &[]);
    write_file(r, "nutrition/lib.rs", "fn f() { let _p = ProductRef::mint(1); }\n");
    write_file(r, "db/lib.rs", "fn f() { let _p = ProductRef::mint(1); }\n");
    let (problems, _) = report(r, &arch::load(r).unwrap());
    assert!(problems.iter().any(|p| p == "api nutrition-db: refinement rules 9 exceeds budget 8"), "{problems:?}");
    assert!(problems.iter().any(|p| p == "api nutrition-db: tokens 3 exceeds budget 2"), "{problems:?}");
    assert!(problems.iter().any(|p| p == "nutrition/lib.rs:1: mints the evidence token ProductRef; only the generator of its api may"), "{problems:?}");
    assert!(!problems.iter().any(|p| p.starts_with("db/lib.rs")), "{problems:?}");
    // A task check's files stand in for the generator and may mint; a
    // worker's own check exempts nothing; production code reaching a test
    // file, or naming `mint` bare, is caught.
    let task = format!("{}  files = [\"nutrition/tests/stand.rs\"]\n", lean_task_tests("stand", "true", "[nutrition]"));
    let own = format!("{}  files = [\"nutrition/own.rs\"]\n", lean_task_tests("own", "true", "[nutrition]"));
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", PROOFS_IDL), ("nutrition-web", PROOFS_WEB)], &[("Tests/t", &task), ("Tests/t.worker", &own)]);
    write_file(r, "nutrition/tests/stand.rs", "fn f() { let _p = ProductRef::mint(1); }\n");
    write_file(r, "nutrition/own.rs", "fn f() { let _p = ProductRef::mint(1); }\n");
    write_file(r, "nutrition/lib.rs", "#[path = \"tests/stand.rs\"]\nmod stand;\nconst ISSUE: fn(i64) -> ProductRef = ProductRef::mint;\n");
    let (problems, _) = report(r, &arch::load(r).unwrap());
    assert!(!problems.iter().any(|p| p.starts_with("nutrition/tests/stand.rs")), "{problems:?}");
    assert!(problems.iter().any(|p| p == "nutrition/own.rs:1: mints the evidence token ProductRef; only the generator of its api may"), "{problems:?}");
    assert!(problems.iter().any(|p| p == "nutrition/lib.rs:2: production code reaches the test file nutrition/tests/stand.rs; a test's minted evidence stays in the test"), "{problems:?}");
    assert!(problems.iter().any(|p| p == "nutrition/lib.rs:3: mints the evidence token ProductRef; only the generator of its api may"), "{problems:?}");
    for f in ["architecture/Tests/t.lean", "architecture/Tests/t.worker.lean", "nutrition/tests/stand.rs", "nutrition/own.rs"] {
        std::fs::remove_file(r.join(f)).unwrap();
    }
    // A ref's `display` and `store` forward the wrapped type's, which must
    // declare them.
    write_file(r, "nutrition/lib.rs", "");
    write_file(r, "db/lib.rs", "");
    let web = PROOFS_WEB.replace("  sum Shown | Shown | Retired", "  type Label : ref «nutrition-db».Id display store\n  sum Shown | Shown : Label | Retired");
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", PROOFS_IDL), ("nutrition-web", &web)], &[]);
    let loaded = arch::load(r).expect("load");
    let (_, errs) = sync_bindings(r, &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
    let binding = std::fs::read_to_string(r.join("apis/nutrition-web/gen/nutrition_web.rs")).unwrap();
    assert!(binding.contains("impl std::fmt::Display for Label {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, \"{}\", self.0) }"), "{binding}");
    assert!(binding.contains("impl rusqlite::ToSql for Label"), "{binding}");
    let (problems, _) = report(r, &loaded);
    assert!(!problems.iter().any(|p| p.contains("forwards")), "{problems:?}");
    write_lean_arch(r, PROOFS_MANIFEST, &[("nutrition-db", PROOFS_IDL), ("nutrition-web", &web.replace("ref «nutrition-db».Id", "ref «nutrition-db».Grams"))], &[]);
    let (problems, _) = report(r, &arch::load(r).unwrap());
    for flag in ["display", "store"] {
        let want = format!("api nutrition-web: type Label: {flag} forwards nutrition-db.Grams's, which declares no {flag}");
        assert!(problems.contains(&want), "{problems:?}");
    }
}

const REFS_MANIFEST: &str = "import Architect\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n\
module testing where\n  lang = typescript\n  paths = [\"testing\"]\n\n\
module approval where\n  lang = typescript\n  paths = [\"approval\"]\n\n\
module «store» where\n  lang = typescript\n  paths = [\"store\"]\n\n\
api «test-report» : testing → «store» where\n  paths = [\"apis/test-report\"]\n\n\
api «test-approval» : testing → approval where\n  paths = [\"apis/test-approval\"]\n\n\
api «store-approval» : «store» → approval where\n  paths = [\"apis/store-approval\"]\n";

const REFS_REPORT: &str = "import Architecture\nopen Dashboard\n\nidl «test-report» where\n  \
token PassedSha : string 40 permanent\n  \
sum Tested | Passed : PassedSha | Failed\n  \
fn run : Tested\n";

const REFS_TEST_APPROVAL: &str = "import Architecture\nimport Apis.«test-report»\nopen Dashboard\n\nidl «test-approval» where\n  \
type PassedSha : ref «test-report».PassedSha\n  \
sum Approved | Approved : PassedSha | Refused\n  \
fn approve : PassedSha → Approved\n";

const REFS_STORE_APPROVAL: &str = "import Architecture\nimport Apis.«test-approval»\nopen Dashboard\n\nidl «store-approval» where\n  \
type PassedSha : ref «test-approval».PassedSha\n  \
sum Stored | Stored : PassedSha | Refused\n  \
fn keep : PassedSha → Stored\n";

#[test]
fn typescript_refs_import_the_target_gen_file() {
    let d = project();
    let r = d.path();
    for a in ["test-report", "test-approval", "store-approval"] {
        write_file(r, &format!("apis/{a}/.keep"), "");
    }
    write_lean_arch(r, REFS_MANIFEST, &[("test-report", REFS_REPORT), ("test-approval", REFS_TEST_APPROVAL), ("store-approval", REFS_STORE_APPROVAL)], &[]);
    let loaded = arch::load(r).expect("load");
    let (_, errs) = sync_bindings(r, &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
    let store = std::fs::read_to_string(r.join("apis/store-approval/gen/store_approval.ts")).unwrap();
    assert!(store.contains("import * as test_approval from \"../../test-approval/gen/test_approval\";"), "{store}");
    let approval = std::fs::read_to_string(r.join("apis/test-approval/gen/test_approval.ts")).unwrap();
    assert!(approval.contains("import * as test_report from \"../../test-report/gen/test_report\";"), "{approval}");
    // Extensionless, as under `moduleResolution: "bundler"`.
    let o = std::process::Command::new("deno")
        .current_dir(r)
        .args(["check", "--quiet", "--unstable-sloppy-imports", "apis/store-approval/gen/store_approval.ts"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
}

/// The manifest of `LEAN_AB_MANIFEST` with a `test_cmd` on `a` and a root
/// `app`: what the suite tests below cover.
fn ab_with_tests() -> String {
    LEAN_AB_MANIFEST.replace("max_ast = 100\n", "max_ast = 100\n  test_cmd = \"cargo test -p a\"\n")
        + "\nroot app where\n  lang = rust\n  paths = [\"src\"]\n"
}

fn pkg(d: &tempfile::TempDir) -> std::path::PathBuf {
    d.path().join("architecture")
}

#[test]
fn checks_and_the_suite_block_export_with_their_sources() {
    let d = project();
    let worker = lean_task_tests("wcheck", "true", "[a, app]");
    write_lean_arch(d.path(), &ab_with_tests(), &[("ab", LEAN_AB_IDL)], &[("Tests", LEAN_AB_TESTS), ("Tests/x.worker", &worker)]);
    let e = lean::export(&pkg(&d), &Sandbox::off()).expect("export");
    let s = &e.suite;
    assert_eq!(s.test_paths, ["stories"]);
    assert_eq!(s.checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["ab_story", "arch", "wcheck", "a"]);
    let story = s.check("ab_story").unwrap();
    assert_eq!((story.covers.clone(), story.files.clone()), (Covers::Only(vec!["a".to_string(), "b".into()]), vec!["stories".to_string()]));
    assert_eq!(story.source.as_deref(), Some("architecture/Tests.lean"));
    assert_eq!(s.check("arch").unwrap().covers, Covers::All);
    assert_eq!(s.check("wcheck").unwrap().source.as_deref(), Some("architecture/Tests/x.worker.lean"));
    assert_eq!(s.from_file("architecture/Tests/x.worker.lean").len(), 1);
    // The implicit module check: `a`'s test_cmd, covering `a`.
    let a = s.check("a").unwrap();
    assert_eq!((a.cmd.as_str(), a.covers.clone(), a.source.as_deref()), ("cargo test -p a", Covers::Only(vec!["a".to_string()]), Some("architecture/Architecture.lean")));
    assert_eq!(s.files_of(&["ab_story", "arch"]), ["stories"]);
}

#[test]
fn build_cmd_exports_for_modules_and_roots() {
    let d = project();
    let manifest = ab_with_tests()
        .replace("test_cmd = \"cargo test -p a\"\n", "test_cmd = \"cargo test -p a\"\n  build_cmd = \"cargo build -p a\"\n")
        + "  build_cmd = \"cargo build -p app\"\n";
    write_lean_arch(d.path(), &manifest, &[("ab", LEAN_AB_IDL)], &[("Tests", LEAN_AB_TESTS)]);
    let e = lean::export(&pkg(&d), &Sandbox::off()).expect("export");
    assert_eq!(e.arch.modules[0].build_cmd.as_deref(), Some("cargo build -p a"));
    assert_eq!(e.arch.modules[1].build_cmd, None);
    assert_eq!(e.arch.roots[0].build_cmd.as_deref(), Some("cargo build -p app"));
    assert_eq!(e.json["arch"]["module"][0]["build_cmd"], "cargo build -p a");
    assert!(e.json["arch"]["module"][1]["build_cmd"].is_null());
}

#[test]
fn impacted_is_the_run_set_of_the_named_modules() {
    let d = project();
    write_lean_arch(d.path(), &ab_with_tests(), &[("ab", LEAN_AB_IDL)], &[("Tests", LEAN_AB_TESTS)]);
    let run = |ms: &[&str]| lean::impacted(&pkg(&d), &Sandbox::off(), &ms.iter().map(|m| m.to_string()).collect::<Vec<_>>());
    assert_eq!(run(&["a"]).unwrap(), ["a", "ab_story", "arch"]);
    assert_eq!(run(&["b"]).unwrap(), ["ab_story", "arch"]);
    assert_eq!(run(&["app"]).unwrap(), ["arch"]);
    let e = run(&["nope"]).unwrap_err();
    assert!(e.contains("nope"), "{e}");
}

#[test]
fn bad_checks_are_lean_errors() {
    let d = project();
    let with = |extra: &str| {
        write_lean_arch(d.path(), &ab_with_tests(), &[("ab", LEAN_AB_IDL)], &[("Tests", &format!("{LEAN_AB_TESTS}\n{extra}"))]);
        lean::export(&pkg(&d), &Sandbox::off()).map(|_| ()).unwrap_err()
    };
    let e = with(&lean_check("a", "true", "[a]"));
    assert!(e.contains("already been declared"), "{e}");
    let e = with(&lean_check("z", "true", "[zzz]"));
    assert!(e.contains("zzz"), "{e}");
    let e = with(&lean_check("z", "true", "3"));
    assert!(e.contains("covers is `all` or a list of modules and roots"), "{e}");
    let e = with("check z where\n  cmd = \"true\"\n");
    assert!(e.contains("covers is empty"), "{e}");
}

#[test]
fn a_fresh_package_builds_from_the_lake_cache() {
    let d = project();
    write_lean_arch(d.path(), LEAN_AB_MANIFEST, &[("ab", LEAN_AB_IDL)], &[("Tests", LEAN_AB_TESTS)]);
    lean::export(&pkg(&d), &Sandbox::off()).expect("export");
    let cache = lean::lake_cache_dir().unwrap().join("build");
    assert!(cache.is_dir(), "{}", cache.display());
    let olean = "lib/lean/Architect.olean";
    let e = project();
    write_lean_arch(e.path(), LEAN_AB_MANIFEST, &[("ab", LEAN_AB_IDL)], &[("Tests", LEAN_AB_TESTS)]);
    lean::export(&pkg(&e), &Sandbox::off()).expect("export from the cache");
    let mtime = |p: std::path::PathBuf| std::fs::metadata(&p).and_then(|m| m.modified()).unwrap();
    assert_eq!(mtime(pkg(&e).join(".lake/build").join(olean)), mtime(cache.join(olean)), "Architect.olean was rebuilt, not seeded");
}
