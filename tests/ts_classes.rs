// A class API whose consumer is TypeScript (handover gap b): the `foldable`
// module (Rust) owns `Seq : Foldable`; two TypeScript modules each get it
// through their own class API, and a TypeScript -> TypeScript API refs one
// of them. The class APIs get only a TypeScript binding (the instance, with
// its laws), no Rust side and no `impl` check; the consumer's jail view
// type-checks and runs every law with `deno test`. Needs `lake` and `deno`.

mod common;

use common::*;
use dashboard::arch::{self, check, sync_bindings};
use dashboard::sandbox::Sandbox;

const MANIFEST: &str = "import Architect\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n\
module foldable where\n  lang = rust\n  paths = [\"engine/instances\"]\n\n\
module client where\n  lang = typescript\n  paths = [\"web/client\"]\n\n\
module sandbox where\n  lang = typescript\n  paths = [\"web/sandbox\"]\n  \
test_cmd = \"deno test --quiet --unstable-sloppy-imports apis/ web/sandbox/\"\n\n\
api «foldable-client» : foldable → client where\n  paths = [\"apis/foldable-client\"]\n  impl = [\"engine/instances/foldable.rs\"]\n\n\
api «foldable-sandbox» : foldable → sandbox where\n  paths = [\"apis/foldable-sandbox\"]\n  impl = [\"engine/instances/foldable.rs\"]\n\n\
api «client-sandbox» : client → sandbox where\n  paths = [\"apis/client-sandbox\"]\n";

const FOLDABLE: &str = "import Architecture\nopen Dashboard\n\nidl «NAME» where\n  container Seq : Foldable, Monoid\n";

const CLIENT_SANDBOX: &str = "import Architecture\nimport Apis.«foldable-client»\nopen Dashboard\n\nidl «client-sandbox» where\n  \
element Row\n    | Json : string 64\n    | Count : i64\n  \
container Rows := «foldable-client».Seq Row\n  \
sum Called\n    | Value : Rows\n    | Empty\n  \
fn call : Rows → Called\n";

const CONSUMER_TEST: &str = r#"
import { test } from "node:test";
import assert from "node:assert/strict";
import { Rows, type Row } from "../../apis/client-sandbox/gen/client_sandbox";
import { Seq } from "../../apis/foldable-sandbox/gen/foldable_sandbox";

test("the sandbox folds the client's rows into its own Seq", () => {
  const rows = Rows.new([{ v: "Json", f0: "{}" }, { v: "Count", f0: 2n }]);
  const mine = Seq.new(rows.foldr((r: Row, acc: Row[]) => [r, ...acc], []));
  const n = mine.foldr((r: Row, acc: bigint) => {
    switch (r.v) {
      case "Json": return acc;
      case "Count": return acc + r.f0;
    }
  }, 0n);
  assert.equal(n, 2n);
});
"#;

#[test]
fn a_rust_class_api_with_a_typescript_consumer_generates_the_instance() {
    assert!(dashboard::lean::available(), "needs `lake`");
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write_lean_arch(
        r,
        MANIFEST,
        &[
            ("foldable-client", &FOLDABLE.replace("NAME", "foldable-client")),
            ("foldable-sandbox", &FOLDABLE.replace("NAME", "foldable-sandbox")),
            ("client-sandbox", CLIENT_SANDBOX),
        ],
        &[],
    );
    // No engine/instances/foldable.rs: nothing checks it for these APIs.
    write_file(r, "engine/instances/README", "the Rust instances\n");
    write_file(r, "web/client/client.ts", "export {};\n");
    write_file(r, "web/sandbox/sandbox.test.ts", CONSUMER_TEST);
    for a in ["foldable-client", "foldable-sandbox", "client-sandbox"] {
        write_file(r, &format!("apis/{a}/.keep"), "");
    }
    let loaded = arch::load(r).expect("load");
    let (wrote, errs) = sync_bindings(r, &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
    wrote.iter().for_each(|w| assert!(!w.ends_with(".rs"), "a Rust binding for a TypeScript consumer: {wrote:?}"));
    for f in ["foldable-sandbox/gen/foldable_sandbox.ts", "foldable-sandbox/gen/foldable_sandbox.laws.test.ts", "client-sandbox/gen/client_sandbox.laws.test.ts"] {
        assert!(wrote.contains(&format!("apis/{f}")), "{f}: {wrote:?}");
    }
    let ts = std::fs::read_to_string(r.join("apis/foldable-sandbox/gen/foldable_sandbox.ts")).unwrap();
    assert!(ts.contains("export class Seq<A>") && ts.contains("append(other: Seq<A>)") && !ts.contains("fmap<B>"), "{ts}");
    let scratch = tempfile::tempdir().unwrap();
    let rep = check(r, &loaded, &Sandbox::off(), scratch.path());
    assert!(rep.problems.is_empty(), "{}", rep.problems.join("\n"));
    assert!(rep.order.contains(&"sandbox".to_string()), "{:?}", rep.order);
}

#[test]
fn a_typescript_api_owning_container_classes_is_still_refused() {
    assert!(dashboard::lean::available(), "needs `lake`");
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    let manifest = "import Architect\nopen Dashboard\n\narchitecture where\n  max_modules = 2\n\n\
module client where\n  lang = typescript\n  paths = [\"web/client\"]\n\n\
module sandbox where\n  lang = typescript\n  paths = [\"web/sandbox\"]\n\n\
api «client-sandbox» : client → sandbox where\n  paths = [\"apis/client-sandbox\"]\n";
    write_lean_arch(r, manifest, &[("client-sandbox", &FOLDABLE.replace("NAME", "client-sandbox"))], &[]);
    let loaded = arch::load(r).expect("load");
    let (_, errs) = sync_bindings(r, &loaded, false);
    assert!(errs.iter().any(|e| e.contains("a TypeScript API declares no container classes")), "{errs:?}");
}
