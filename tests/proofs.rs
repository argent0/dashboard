// Proofs supplied by the caller, compiled for real (architect-v3.md): a
// refined T's `new` refuses a value that breaks its rule, a linear token
// cannot be used twice (the compiler says so), a permanent one clones.
// Needs `rustc`.

use dashboard::idl::Idl;
use std::path::Path;
use std::process::Command;

const IDL: &str = r#"{"api":"ledger","types":[
    {"name":"Grams","kind":{"repr":"f64"},"pred":{"and":[{"cmp":["<",{"num":"0"},"v"]},{"cmp":["≤","v",{"num":"20000"}]}]}},
    {"name":"Day","kind":{"repr":"string"},"max_len":10,"pred":{"cmp":["=","len",{"num":"10"}]}},
    {"name":"Span","kind":{"tuple":[{"repr":"i64"},{"repr":"i64"}]},"pred":{"cmp":["<",{"field":1},{"field":2}]}},
    {"name":"Unit","kind":{"repr":"string"},"max_len":8,"pred":{"mem":["v",[{"str":"g"},{"str":"ml"}]]}},
    {"name":"Id","kind":{"repr":"i64"}}],
    "tokens":[{"name":"Account","repr":"i64","kind":"permanent"},{"name":"Funds","repr":"i64","kind":{"linear":"AlreadyApplied"}}],
    "sums":[{"name":"Done","ctors":[{"name":"Done","args":["Id"]},{"name":"AlreadyApplied"}]},
    {"name":"Reserved","ctors":[{"name":"Reserved","args":["Funds"]},{"name":"Insufficient"}]}],
    "fns":[{"name":"reserve","inputs":["Account","Grams","Day","Span","Unit"],"returns":"Reserved"},
    {"name":"withdraw","inputs":["Funds"],"returns":"Done"}]}"#;

const LIB: &str = r#"
#[path = "binding.rs"]
mod binding;
use binding::*;

struct Ledger;
impl LedgerApi for Ledger {
    fn reserve(&mut self, _a: Account, _g: Grams, _d: Day, _s: Span, _u: Unit) -> Reserved { Reserved::Reserved(Funds::mint(1)) }
    fn withdraw(&mut self, _f: Funds) -> Done { Done::Done(Id::new(1)) }
}

/// A permanent token is evidence that stays true: it clones.
fn both(a: Account) -> (Account, Account) { (a.clone(), a) }

#[test]
fn rules_hold_at_the_constructor() {
    assert_eq!(Grams::new(0.0).err().unwrap().rule, "0 < v ∧ v ≤ 20000");
    assert!(Grams::new(20000.0).is_ok() && Grams::new(20000.5).is_err());
    assert!(Day::new("2026-10-17".into()).is_ok() && Day::new("x".into()).is_err());
    assert_eq!(Span::new((2, 1)).err().unwrap().to_string(), "Span: v.1 < v.2");
    assert!(Span::new((1, 2)).is_ok());
    assert!(Unit::new("g".into()).is_ok() && Unit::new("kg".into()).is_err());
    assert_eq!(Unit::new("kilogram!".into()).err().unwrap().rule, "v.length ≤ 8");
    let (_, _) = both(Account::mint(3));
    let r = Ledger.reserve(Account::mint(3), Grams::new(1.0).unwrap(), Day::new("2026-10-17".into()).unwrap(), Span::new((1, 2)).unwrap(), Unit::new("g".into()).unwrap());
    match r {
        Reserved::Reserved(f) => { Ledger.withdraw(f); }
        Reserved::Insufficient => panic!(),
    }
}
"#;

const TWICE: &str = r#"
fn twice(f: Funds) { Ledger.withdraw(f); Ledger.withdraw(f); }
"#;

fn rustc(dir: &Path, test: bool) -> (bool, String) {
    let mut c = Command::new("rustc");
    c.current_dir(dir).args(["--edition", "2024", "-A", "warnings", "lib.rs", "-o", "out"]);
    c.arg(if test { "--test" } else { "--crate-type=lib" });
    let o = c.output().expect("rustc");
    let mut text = String::from_utf8_lossy(&o.stderr).into_owned();
    if o.status.success() && test {
        let r = Command::new(dir.join("out")).arg("-q").output().unwrap();
        text.push_str(&String::from_utf8_lossy(&r.stdout));
        text.push_str(&String::from_utf8_lossy(&r.stderr));
        return (r.status.success(), text);
    }
    (o.status.success(), text)
}

#[test]
fn refined_types_refuse_and_linear_tokens_move() {
    let idl = Idl::from_export(serde_json::from_str(IDL).unwrap()).unwrap();
    let d = tempfile::tempdir().unwrap();
    // The IDL's api is `ledger`: its trait is `Ledger`, which the impl type here is named after too.
    std::fs::write(d.path().join("binding.rs"), idl.rust(false, &[]).replace("pub trait Ledger ", "pub trait LedgerApi ").replace("dyn Ledger", "dyn LedgerApi")).unwrap();
    std::fs::write(d.path().join("lib.rs"), LIB).unwrap();
    let (ok, out) = rustc(d.path(), true);
    assert!(ok, "{out}");
    std::fs::write(d.path().join("lib.rs"), format!("{LIB}{TWICE}")).unwrap();
    let (ok, out) = rustc(d.path(), false);
    assert!(!ok && out.contains("use of moved value: `f`"), "{out}");
}
