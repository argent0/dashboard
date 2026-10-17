// Generator gaps (handover d, f). A single-variant element's TypeScript wire
// codec type-checks. A named sum as a constructor argument: sums nest, the
// consumer matches the inner one in turn. Compiled for real: Rust with
// `rustc --test`, TypeScript with `deno test` (which type-checks); Kotlin is
// checked as text. Needs `rustc` and `deno`.

use dashboard::idl::Idl;
use std::collections::BTreeMap;
use std::process::Command;

const IDL: &str = r#"{"api":"desk","types":[{"name":"Id","kind":{"repr":"i64"}}],
    "sums":[{"name":"Reason","ctors":[{"name":"Missing"},{"name":"Late","args":["Id"]}]},
    {"name":"Outcome","ctors":[{"name":"Accepted","args":["Id"]},{"name":"Refused","args":["Reason","Id"]}]}],
    "fns":[{"name":"decide","inputs":["Id"],"returns":"Outcome"}]}"#;

const LIB: &str = r#"
#[path = "binding.rs"]
mod binding;
use binding::*;

struct D;
impl Desk for D {
    fn decide(&mut self, id: Id) -> Outcome { Outcome::Refused(Reason::Late(id), Id::new(2)) }
}

#[test]
fn the_consumer_matches_the_inner_sum() {
    let n = match D.decide(Id::new(1)) {
        Outcome::Accepted(_) => 0,
        Outcome::Refused(r, _) => match r {
            Reason::Missing => 1,
            Reason::Late(_) => 2,
        },
    };
    assert_eq!(n, 2);
}
"#;

const TS_TEST: &str = r#"
import { test } from "node:test";
import assert from "node:assert/strict";
import { Id, Outcome, Reason, outcomeFromJson, outcomeToJson } from "./desk.ts";

test("a nested sum round-trips on the wire", () => {
  const o = Outcome.Refused(Reason.Late(new Id(1n)), new Id(2n));
  const s = outcomeToJson(o);
  assert.equal(s, '{"c":"Refused","a":[{"c":"Late","a":[1]},2]}');
  const back = outcomeFromJson(s);
  assert.equal(back.kind, "Refused");
  if (back.kind === "Refused") assert.equal(back.a0.kind, "Late");
  assert.throws(() => outcomeFromJson('{"c":"Refused","a":[{"c":"Nope","a":[]},2]}'));
});
"#;

fn idl() -> Idl {
    Idl::from_export(serde_json::from_str(IDL).unwrap()).unwrap()
}

#[test]
fn rust_nests_the_sum() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("binding.rs"), idl().rust(false, &[])).unwrap();
    std::fs::write(d.path().join("lib.rs"), LIB).unwrap();
    let o = Command::new("rustc").current_dir(d.path()).args(["--edition", "2024", "-A", "warnings", "--test", "lib.rs", "-o", "out"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r = Command::new(d.path().join("out")).arg("-q").output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stdout));
    // On the wire, the inner sum is its own value.
    let wire = idl().rust(true, &[]);
    assert!(wire.contains("Outcome::Refused(a0, a1) => json!({\"c\": \"Refused\", \"a\": [a0.to_value(), a1.to_value()]})"), "{wire}");
    assert!(wire.contains("Outcome::Refused(Reason::from_value(&args[0])?, Id::from_value(&args[1])?)"), "{wire}");
}

#[test]
fn typescript_round_trips_the_nested_sum() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("desk.ts"), idl().typescript(true, &BTreeMap::new())).unwrap();
    std::fs::write(d.path().join("desk.test.ts"), TS_TEST).unwrap();
    let o = Command::new("deno").current_dir(d.path()).args(["test", "--quiet", "desk.test.ts"]).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
}

#[test]
fn kotlin_has_value_level_sum_codecs() {
    let kt = idl().kotlin(true);
    assert!(kt.contains("is Outcome.Refused -> mapOf(\"c\" to \"Refused\", \"a\" to listOf<Any?>(toValue(v.a0), v.a1.toValue()))"), "{kt}");
    assert!(kt.contains("Outcome.Refused(fromValueReason(a[0]), Id.fromValue(a[1]))"), "{kt}");
    assert!(kt.contains("fun fromJsonOutcome(s: String): Outcome"), "{kt}");
}

#[test]
fn the_default_sum_takes_a_named_one() {
    let idl = Idl::from_export(serde_json::from_str(r#"{"api":"b","sums":[{"name":"Reason","ctors":[{"name":"Missing"}]}],"ctors":[{"name":"Why","args":["Reason"]}]}"#).unwrap()).unwrap();
    assert!(idl.rust(false, &[]).contains("    Why(Reason),"));
}

#[test]
fn a_sum_never_contains_itself() {
    let bad = |sums: &str| Idl::from_export(serde_json::from_str(&format!(r#"{{"api":"a","sums":{sums},"fns":[{{"name":"f","inputs":[],"returns":"A"}}]}}"#)).unwrap()).unwrap_err();
    let e = bad(r#"[{"name":"A","ctors":[{"name":"X","args":["A"]}]}]"#);
    assert!(e.contains("sum A: constructor X: sum A must be declared before A"), "{e}");
    let e = bad(r#"[{"name":"A","ctors":[{"name":"X","args":["B"]}]},{"name":"B","ctors":[{"name":"Y"}]}]"#);
    assert!(e.contains("sum A: constructor X: sum B must be declared before A"), "{e}");
}


const ONE_VARIANT: &str = r#"{"api":"memo","elements":[{"name":"Note","variants":[{"name":"Name","fields":[{"repr":"string","max_len":64}]}]}],
    "containers":[{"name":"Notes","kind":{"of":["Note","f","Seq"]}}],
    "sums":[{"name":"Got","ctors":[{"name":"Got","args":["Notes"]}]}],
    "fns":[{"name":"notes","inputs":[],"returns":"Got"}]}"#;

const ONE_VARIANT_TEST: &str = r#"
import { test } from "node:test";
import assert from "node:assert/strict";
import { Notes, notesFromValue, notesToValue } from "./notes.ts";

test("a one-variant element round-trips", () => {
  const v = notesToValue(Notes.new([{ v: "Name", f0: "x" }]));
  assert.deepEqual(v, [{ v: "Name", f: ["x"] }]);
  assert.deepEqual(notesToValue(notesFromValue(v)), v);
});
"#;

#[test]
fn a_one_variant_element_type_checks_in_typescript() {
    let idl = Idl::from_export(serde_json::from_str(ONE_VARIANT).unwrap()).unwrap();
    let d = tempfile::tempdir().unwrap();
    let ts = idl.typescript(true, &BTreeMap::new());
    assert!(!ts.contains("const never: never = e"), "{ts}");
    std::fs::write(d.path().join("notes.ts"), ts).unwrap();
    std::fs::write(d.path().join("notes.test.ts"), ONE_VARIANT_TEST).unwrap();
    let o = Command::new("deno").current_dir(d.path()).args(["test", "--quiet", "notes.test.ts"]).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
}
