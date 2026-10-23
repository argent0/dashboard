// Function-shaped APIs across the wire (handover §1, §3): a Rust generator
// serves `dispatch`, a TypeScript consumer calls through the generated
// client, and rows cross as a container of elements. The generated bindings
// are compiled and run for real in each module's jail view: `cargo test`
// for the Rust side, `deno test` for the TypeScript side (the generated laws
// included). Needs `lake`, `cargo` (serde_json in the offline registry) and
// `deno` on the host.

mod common;

use common::*;
use dashboard::arch::{self, check, sync_bindings};
use dashboard::sandbox::Sandbox;

const MANIFEST: &str = "import Architect\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n  shared = [\"engine/Cargo.toml\"]\n\n\
module foldable where\n  lang = rust\n  paths = [\"engine/instances\"]\n\n\
module db where\n  lang = rust\n  paths = [\"engine/src\"]\n  \
test_cmd = \"cd engine && CARGO_TARGET_DIR=$DASHBOARD_ROOT/engine/target cargo test --offline -q 2>&1 | tail -30\"\n\n\
module web where\n  lang = typescript\n  paths = [\"web\"]\n  \
test_cmd = \"deno test --quiet --unstable-sloppy-imports apis/db-web/gen/ web/\"\n\n\
api «foldable-db» : foldable → db where\n  paths = [\"apis/foldable-db\"]\n  impl = [\"engine/instances/foldable.rs\"]\n\n\
api «db-web» : db → web where\n  paths = [\"apis/db-web\"]\n  max_fns = 5\n  max_constructors = 7\n  max_rules = 2\n";

const FOLDABLE_DB: &str = "import Architecture\nopen Dashboard\n\nidl «foldable-db» where\n  container Seq : Foldable, Monoid\n";

const DB_WEB: &str = "import Architecture\nimport Apis.«foldable-db»\nopen Dashboard\n\nidl «db-web» where\n  \
type Id : i64 display\n  type Reason : string 200 display\n  type Name : string 32\n  \
type Grams : f64 where 0 < v ∧ v ≤ 20000\n  \
token ProductRef : i64 permanent\n  token EventTypeRef : i64 versioned stale=Retired\n  \
element Row\n    | Meal : f64 → string 16\n    | Note : string 32\n  \
container Rows := «foldable-db».Seq Row\n  \
sum Written\n    | Written : Id\n    | Rejected : Reason\n    | Retired\n  \
sum Query\n    | Rows : Rows\n  \
sum Resolved\n    | Resolved : ProductRef\n    | Unknown\n  \
sum Current\n    | Current : EventTypeRef\n  \
fn log : Rows → Written\n  fn rowsForDay : Query\n  \
fn resolveProduct : Name → Resolved\n  fn currentEvent : Current\n  \
fn weigh : ProductRef → Grams → EventTypeRef → Written\n";

const SEQ: &str = r#"
use super::{Foldable, Monoid};
pub struct Seq<A>(Vec<A>);
impl<A> Seq<A> {
    pub fn new(v: Vec<A>) -> Self { Seq(v) }
}
impl<A> Foldable<A> for Seq<A> {
    fn foldr<B>(&self, mut f: impl FnMut(&A, B) -> B, z: B) -> B {
        self.0.iter().rev().fold(z, |acc, x| f(x, acc))
    }
}
impl<A: Clone> Monoid for Seq<A> {
    fn empty() -> Self { Seq(Vec::new()) }
    fn append(self, other: Self) -> Self {
        let mut v = self.0;
        v.extend(other.0);
        Seq(v)
    }
}
"#;

const ENGINE: &str = r##"
#[path = "../../apis/foldable-db/gen/foldable_db.rs"]
pub mod foldable_db;
#[path = "../../apis/db-web/gen/db_web.rs"]
pub mod db_web;

use db_web::*;
use foldable_db::Foldable;

/// The generator: implements the call surface.
pub struct Db;

impl DbWeb for Db {
    fn log(&mut self, rows: Rows) -> Written {
        let n = rows.foldr(|_, acc| acc + 1, 0);
        if n > 5 { Written::Rejected(Reason::new("too many".into()).unwrap()) } else { Written::Written(Id::new(n)) }
    }
    fn rows_for_day(&mut self) -> Query {
        Query::Rows(Rows::new(vec![Row::Meal(1.5, "egg".into()), Row::Note("x".into())]))
    }
    fn resolve_product(&mut self, _name: Name) -> Resolved {
        // Evidence: the generator alone mints it.
        Resolved::Resolved(ProductRef::mint(7))
    }
    fn current_event(&mut self) -> Current {
        Current::Current(EventTypeRef::mint(3, 2))
    }
    fn weigh(&mut self, _product: ProductRef, _grams: Grams, _event: EventTypeRef) -> Written {
        // No `if`: the proofs came in with the arguments; a stale event
        // would be the store's `Retired`.
        Written::Written(Id::new(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_round_trips_calls_and_containers() {
        let req = r#"{"fn":"log","args":[[{"v":"Meal","f":[1.5,"egg"]},{"v":"Note","f":["hi"]}]]}"#;
        assert_eq!(dispatch(&mut Db, req), r#"{"a":[2],"c":"Written"}"#);
        let reply = dispatch(&mut Db, r#"{"fn":"rowsForDay","args":[]}"#);
        assert_eq!(reply, r#"{"a":[[{"f":[1.5,"egg"],"v":"Meal"},{"f":["x"],"v":"Note"}]],"c":"Rows"}"#);
        // Bounds hold on the way in; unknown fns and bad shapes are errors, not panics.
        let long = r#"{"fn":"log","args":[[{"v":"Meal","f":[1.5,"a-name-longer-than-sixteen"]}]]}"#;
        assert!(dispatch(&mut Db, long).contains("longer than 16"), "{}", dispatch(&mut Db, long));
        assert!(dispatch(&mut Db, r#"{"fn":"nope","args":[]}"#).contains("unknown fn"));
        assert!(dispatch(&mut Db, r#"{"fn":"log","args":[]}"#).contains("wrong argument count"));
        assert!(dispatch(&mut Db, "not json").contains("error"));
        // A sum round-trips through its own codec.
        let w = Written::from_json(r#"{"c":"Rejected","a":["no"]}"#).unwrap();
        assert_eq!(w.to_json(), r#"{"a":["no"],"c":"Rejected"}"#);
    }

    #[test]
    fn proofs_are_checked_at_the_constructor_and_on_the_wire() {
        assert_eq!(Grams::new(-1.0).unwrap_err().to_string(), "Grams: 0 < v ∧ v ≤ 20000");
        assert!(Grams::new(20000.0).is_ok());
        let bad = r#"{"fn":"weigh","args":[7, 30000, [3, 2]]}"#;
        assert!(dispatch(&mut Db, bad).contains("Grams: 0 < v ∧ v ≤ 20000"), "{}", dispatch(&mut Db, bad));
        assert_eq!(dispatch(&mut Db, r#"{"fn":"weigh","args":[7, 30, [3, 2]]}"#), r#"{"a":[1],"c":"Written"}"#);
        assert_eq!(dispatch(&mut Db, r#"{"fn":"resolveProduct","args":["egg"]}"#), r#"{"a":[7],"c":"Resolved"}"#);
        assert_eq!(dispatch(&mut Db, r#"{"fn":"currentEvent","args":[]}"#), r#"{"a":[[3,2]],"c":"Current"}"#);
        assert!(dispatch(&mut Db, r#"{"fn":"weigh","args":[7, 30, [3]]}"#).contains("expected [id, version]"));
        // A versioned token decodes with its version (and `currentEvent`
        // above encodes one); reading it back here would be a wire leak.
        assert!(EventTypeRef::from_value(&serde_json::json!([3, 2])).is_ok());
    }
}
"##;

const CONSUMER_TEST: &str = r#"
import { test } from "node:test";
import assert from "node:assert/strict";
import { DbWebClient, Grams, Name, Row, Rows, Violation } from "../apis/db-web/gen/db_web";

test("the client decodes a reply and the consumer folds the rows", async () => {
  const client = new DbWebClient(async (json) => {
    const req = JSON.parse(json) as { fn: string; args: unknown[] };
    assert.equal(req.fn, "rowsForDay");
    return '{"a":[[{"f":[1.5,"egg"],"v":"Meal"},{"f":["x"],"v":"Note"}]],"c":"Rows"}';
  });
  const q = await client.rowsForDay();
  switch (q.kind) {
    case "Rows": {
      const kcal = q.a0.foldr((r: Row, acc: number) => {
        switch (r.v) {
          case "Meal": return acc + r.f0;
          case "Note": return acc;
        }
      }, 0);
      assert.equal(kcal, 1.5);
    }
  }
  let sent = "";
  const w = await new DbWebClient(async (json) => { sent = json; return '{"a":[2],"c":"Written"}'; })
    .log(Rows.new([{ v: "Note", f0: "hi" }, { v: "Meal", f0: 2, f1: "x" }]));
  assert.equal(sent, '{"fn":"log","args":[[{"v":"Note","f":["hi"]},{"v":"Meal","f":[2,"x"]}]]}');
  assert.equal(w.kind, "Written");
  const bad = new DbWebClient(async () => '{"a":[[{"f":[1.5,"a-name-longer-than-sixteen"],"v":"Meal"}]],"c":"Rows"}');
  await assert.rejects(bad.rowsForDay(), /longer than 16/);
  await assert.rejects(new DbWebClient(async () => '{"error":"unknown fn"}').rowsForDay(), /unknown fn/);
});

test("refinements fail at the constructor and evidence comes from the generator", async () => {
  assert.throws(() => new Grams(-1), Violation);
  assert.throws(() => new Grams(30000), /Grams: 0 < v ∧ v ≤ 20000/);
  let sent = "";
  const c = new DbWebClient(async (json) => {
    sent = json;
    const req = JSON.parse(json) as { fn: string };
    if (req.fn === "resolveProduct") return '{"a":[7],"c":"Resolved"}';
    if (req.fn === "currentEvent") return '{"a":[[3,2]],"c":"Current"}';
    return '{"a":[1],"c":"Written"}';
  });
  const r = await c.resolveProduct(new Name("egg"));
  const e = await c.currentEvent();
  if (r.kind === "Resolved" && e.kind === "Current") {
    const w = await c.weigh(r.a0, new Grams(30), e.a0);
    assert.equal(sent, '{"fn":"weigh","args":[7,30,[3,2]]}');
    assert.equal(w.kind, "Written");
  } else {
    assert.fail("expected Resolved and Current");
  }
  await assert.rejects(new DbWebClient(async () => '{"a":[[3]],"c":"Current"}').currentEvent(), /expected \[id, version\]/);
});
"#;

#[test]
fn a_rust_generator_and_a_typescript_consumer_meet_on_the_wire() {
    assert!(dashboard::lean::available(), "needs `lake`");
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write_lean_arch(r, MANIFEST, &[("foldable-db", FOLDABLE_DB), ("db-web", DB_WEB)], &[]);
    write_file(r, "engine/Cargo.toml", "[package]\nname = \"engine\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde_json = \"1\"\n");
    write_file(r, "engine/src/lib.rs", ENGINE);
    write_file(r, "engine/instances/foldable.rs", SEQ);
    write_file(r, "web/consumer.test.ts", CONSUMER_TEST);
    write_file(r, "apis/foldable-db/.keep", "");
    write_file(r, "apis/db-web/.keep", "");
    let loaded = arch::load(r).expect("load");
    let (wrote, errs) = sync_bindings(r, &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
    assert!(wrote.contains(&"apis/db-web/gen/db_web.ts".to_string()) && wrote.contains(&"apis/db-web/gen/db_web.laws.test.ts".to_string()), "{wrote:?}");
    let ts = std::fs::read_to_string(r.join("apis/db-web/gen/db_web.ts")).unwrap();
    assert!(ts.contains("export class Seq<A>") && ts.contains("append(other: Seq<A>)") && !ts.contains("fmap<B>"), "{ts}");
    let scratch = tempfile::tempdir().unwrap();
    let rep = check(r, &loaded, &Sandbox::off(), scratch.path());
    assert!(rep.problems.is_empty(), "{}", rep.problems.join("\n"));
    assert_eq!(rep.order, ["db", "web"]);
    assert!(rep.headroom.iter().any(|h| h.contains("api db-web: constructors 7/7, T slots 5, fns 5/5, inputs 5, element variants 2, fields 3, string max_len 200/256, rules 2/2, tokens 2")), "{:?}", rep.headroom);
    // A consumer that reads rows out of a tunnel is caught by the lint.
    write_file(r, "web/tunnel.ts", "import { Reason } from \"../apis/db-web/gen/db_web\";\nexport const r = new Reason(JSON.stringify({ a: 1 }));\n");
    let rep = check(r, &loaded, &Sandbox::off(), scratch.path());
    assert!(rep.problems.iter().any(|p| p.starts_with("web/tunnel.ts:2: serialized structure crosses the API")), "{:?}", rep.problems);
}
