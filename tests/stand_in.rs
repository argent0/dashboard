// The generated stand-in (`<api>_stand_in!`), compiled for real: a fixture
// calling it through `super::` builds in the consumer's test crate and in
// the generator's, scripted replies answer calls in order, and an empty
// queue panics naming the fn. Needs `rustc`.

use dashboard::idl::Idl;
use std::path::Path;
use std::process::Command;

const IDL: &str = r#"{"api":"core-alpha","types":[{"name":"HeroId","kind":{"repr":"i64"}}],
    "sums":[{"name":"Loaded","ctors":[{"name":"Hero","args":["HeroId"]},{"name":"Missing"}]}],
    "fns":[{"name":"load","inputs":["HeroId"],"returns":"Loaded"},
    {"name":"swap","inputs":["HeroId","HeroId"],"returns":"Loaded"}]}"#;

/// The fixture both test crates mount: crate-neutral, through `super::`.
const FIXTURE: &str = "#![allow(unused)]\nsuper::stand_in!(Stand, super::api);\n\
pub fn hero(n: i64) -> super::api::HeroId { super::api::HeroId::new(n) }\n";

/// A test crate root: maps `api` and `stand_in` to its binding, then tests.
fn test_root(krate: &str) -> String {
    format!(
        "#![deny(warnings)]\nuse {krate}::core_alpha as api;\nuse {krate}::core_alpha_stand_in as stand_in;\n\
         #[path = \"fixture.rs\"] mod fixture;\nuse api::CoreAlpha;\n\
         #[test]\nfn replies_in_order() {{\n    let mut s = fixture::Stand::default();\n    \
         s.load.push_back(Box::new(|h| api::Loaded::Hero(h)));\n    \
         s.load.push_back(Box::new(|_| api::Loaded::Missing));\n    \
         assert!(matches!(s.load(fixture::hero(1)), api::Loaded::Hero(_)));\n    \
         assert!(matches!(s.load(fixture::hero(2)), api::Loaded::Missing));\n    \
         assert_eq!(s.calls, [\"load\", \"load\"]);\n}}\n\
         #[test]\n#[should_panic(expected = \"core-alpha stand-in: no scripted reply left for swap\")]\n\
         fn an_empty_queue_names_the_fn() {{\n    fixture::Stand::default().swap(fixture::hero(1), fixture::hero(2));\n}}\n"
    )
}

fn rustc(dir: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new("rustc").current_dir(dir).args(["--edition", "2024"]).args(args).output().expect("rustc");
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn run(dir: &Path, bin: &str) -> (bool, String) {
    let o = Command::new(dir.join(bin)).arg("-q").output().unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

#[test]
fn a_fixture_scripts_the_stand_in_in_both_crates() {
    let idl = Idl::from_export(serde_json::from_str(IDL).unwrap()).unwrap();
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    std::fs::write(p.join("binding.rs"), idl.rust(false, &[])).unwrap();
    std::fs::write(p.join("fixture.rs"), FIXTURE).unwrap();
    // The consumer: a lib crate mounting the binding, and its test crate.
    std::fs::write(p.join("alpha.rs"), "#[path = \"binding.rs\"]\npub mod core_alpha;\n").unwrap();
    let (ok, out) = rustc(p, &["--crate-type=lib", "--crate-name=alpha", "alpha.rs", "-o", "libalpha.rlib"]);
    assert!(ok, "{out}");
    std::fs::write(p.join("alpha_test.rs"), test_root("alpha")).unwrap();
    let (ok, out) = rustc(p, &["--test", "alpha_test.rs", "--extern", "alpha=libalpha.rlib", "-L", ".", "-o", "alpha_test"]);
    assert!(ok, "{out}");
    let (ok, out) = run(p, "alpha_test");
    assert!(ok, "{out}");
    // The generator's own tests mount the same binding and the same fixture.
    std::fs::write(p.join("core_test.rs"), test_root("crate").replace("use crate::core_alpha as api;", "#[path = \"binding.rs\"] pub mod core_alpha;\nuse crate::core_alpha as api;")).unwrap();
    let (ok, out) = rustc(p, &["--test", "core_test.rs", "-o", "core_test"]);
    assert!(ok, "{out}");
    let (ok, out) = run(p, "core_test");
    assert!(ok, "{out}");
}
