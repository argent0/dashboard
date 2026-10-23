// Frames end to end: a module's frame declared in Lean, `arch gen` writing
// the skeleton and hole starters, the holes filled and compiled with
// `rustc` (hole unit tests through the generated stand-in, and a call
// through the provided API), then `arch check` catching a hole that
// reaches around its context and a hand-edited skeleton. Needs `lake` and
// `rustc`.

mod common;

use common::*;
use dashboard::arch;
use dashboard::sandbox::Sandbox;
use std::path::Path;
use std::process::Command;

const MANIFEST: &str = "import Architect\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n\
module core where\n  lang = rust\n  paths = [\"core\"]\n\n\
module alpha where\n  lang = rust\n  paths = [\"alpha\"]\n\n\
module beta where\n  lang = rust\n  paths = [\"beta\"]\n\n\
root app where\n  lang = rust\n  paths = [\"app\"]\n\n\
api «core-alpha» : core → alpha where\n  paths = [\"apis/core-alpha\"]\n\n\
api «alpha-ui» : alpha → app where\n  paths = [\"apis/alpha-ui\"]\n";

const CORE_ALPHA: &str = "import Architecture\nopen Dashboard\n\nidl «core-alpha» where\n  type HeroId : i64\n  \
sum Loaded | Found : HeroId | Missing\n  fn load : HeroId → Loaded\n";

const ALPHA_UI: &str = "import Architecture\nopen Dashboard\n\nidl «alpha-ui» where\n  type Id : i64\n  type Score : i64\n  \
type Note : i64\n  sum Plan | Attack : Score | Flee\n  sum Decision | Go : Score | Stay | Say : Note\n  fn decide : Id → Plan → Decision\n";

const FRAME: &str = "import Architecture\nopen Dashboard\n\nframe alpha where\n  src \"alpha/src\"\n  \
fn decide : Id → Plan → Decision calls rate, «core-alpha».load branch Plan\n  \
fn rate : Score → Tally\n  record Tally : i64 → bool\n";

/// A frame consuming nothing: a `bytes` input, a type named like a
/// keyword, a sum over a record.
const BETA: &str = "import Architecture\nopen Dashboard\n\nframe beta where\n  src \"beta/src\"\n  \
fn pack : bytes → Outcome calls count\n  fn count : Match → i64\n  record Match : i64\n  sum Outcome | Won : Match | Lost\n";

const PACK: &str = "pub fn pack(ctx: &mut dyn PackCtx, bytes: Vec<u8>) -> Outcome {\n    \
let n = ctx.count(Match(bytes.len() as i64));\n    if n > 0 { Outcome::Won(Match(n)) } else { Outcome::Lost }\n}\n";

const COUNT: &str = "pub fn count(_ctx: &mut dyn CountCtx, match_: Match) -> i64 {\n    match_.0 * one()\n}\n\n\
fn one() -> i64 {\n    enum E { A }\n    let _ = <i64 as Clone>::clone(&1);\n    match E::A { E::A => 1 }\n}\n";

const BETA_LIB: &str = "pub mod frame;\n\n#[cfg(test)]\nmod wired {\n    use crate::frame::prelude::*;\n\n    \
#[test]\n    fn packs() {\n        let mut f = crate::frame::Frame::new();\n        \
assert_eq!(crate::frame::pack(&mut f, vec![1, 2]), Outcome::Won(Match(2)));\n    }\n}\n";

const DECIDE: &str = "// Attack rates the score when the hero loads; flee stays.\n\
pub fn on_attack(ctx: &mut dyn DecideCtx, _id: Id, score: Score) -> Decision {\n    \
match ctx.load(HeroId::new(1)) {\n        Loaded::Found(_) => if ctx.rate(score).1 { Decision::Stay } else { Decision::Stay },\n        Loaded::Missing => Decision::Stay,\n    }\n}\n\n\
pub fn on_flee(_ctx: &mut dyn DecideCtx, _id: Id) -> Decision {\n    Decision::Say(Note::new(1))\n}\n\n\
#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn attack_loads_then_rates() {\n        \
let mut s = DecideCtxStandIn::default();\n        s.load.push_back(Box::new(|h| Loaded::Found(h)));\n        \
s.rate.push_back(Box::new(|_| Tally(3, true)));\n        assert!(matches!(on_attack(&mut s, Id::new(1), Score::new(2)), Decision::Stay));\n        \
assert_eq!(s.calls, [\"load\", \"rate\"]);\n    }\n}\n";

const RATE: &str = "pub fn rate(_ctx: &mut dyn RateCtx, _score: Score) -> Tally {\n    Tally(1, true)\n}\n";

const LIB: &str = "#[path = \"../../apis/core-alpha/gen/core_alpha.rs\"]\npub mod core_alpha;\n\
#[path = \"../../apis/alpha-ui/gen/alpha_ui.rs\"]\npub mod alpha_ui;\npub mod frame;\n\n\
#[cfg(test)]\nmod wired {\n    use crate::alpha_ui::{AlphaUi, Decision, Id, Plan};\n    use crate::core_alpha::{CoreAlpha, HeroId, Loaded};\n    \
struct Core(u32);\n    impl CoreAlpha for Core {\n        fn load(&mut self, h: HeroId) -> Loaded { self.0 += 1; Loaded::Found(h) }\n    }\n\n    \
#[test]\n    fn the_provided_api_routes_through_the_holes() {\n        let mut core = Core(0);\n        \
let mut f = crate::frame::Frame::new(&mut core);\n        \
assert!(matches!(f.decide(Id::new(1), Plan::Flee), Decision::Say(_)));\n        \
assert!(matches!(f.decide(Id::new(1), Plan::Attack(crate::alpha_ui::Score::new(5))), Decision::Stay));\n        \
drop(f);\n        assert_eq!(core.0, 1);\n    }\n}\n";

fn rustc_test(root: &Path) -> (bool, String) {
    rustc_crate(root, "alpha")
}

fn rustc_crate(root: &Path, krate: &str) -> (bool, String) {
    let out = root.join(format!("{krate}-test"));
    let o = Command::new("rustc")
        .current_dir(root)
        .args(["--edition", "2024", "--test", &format!("{krate}/src/lib.rs"), "-o"])
        .arg(&out)
        .output()
        .expect("rustc");
    let mut text = String::from_utf8_lossy(&o.stderr).into_owned();
    if !o.status.success() {
        return (false, text);
    }
    let r = Command::new(&out).arg("-q").output().unwrap();
    text.push_str(&String::from_utf8_lossy(&r.stdout));
    text.push_str(&String::from_utf8_lossy(&r.stderr));
    (r.status.success(), text)
}

fn check(root: &Path) -> arch::Report {
    let loaded = arch::load(root).expect("load");
    let scratch = tempfile::tempdir().unwrap();
    arch::check(root, &loaded, &Sandbox::off(), scratch.path())
}

#[test]
fn a_frame_generates_a_skeleton_whose_holes_compile_and_stay_in_their_context() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    write_lean_arch(root, MANIFEST, &[("core-alpha", CORE_ALPHA), ("alpha-ui", ALPHA_UI)], &[("Frames/alpha", FRAME), ("Frames/beta", BETA)]);
    write_file(root, "core/lib.rs", "");
    write_file(root, "app/main.rs", "fn main() {}\n");
    write_file(root, "alpha/src/lib.rs", LIB);
    write_file(root, "beta/src/lib.rs", BETA_LIB);
    let loaded = arch::load(root).expect("load");
    assert_eq!(loaded.cfg.frames.len(), 2);
    let edges = dashboard::frame::edges(&loaded.cfg);
    assert_eq!(edges, [("alpha.decide".to_string(), "alpha.rate".to_string()), ("alpha.decide".into(), "core-alpha.load".into()), ("beta.pack".into(), "beta.count".into())]);
    let (wrote, errs) = arch::sync_bindings(root, &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
    for f in ["alpha/src/frame/mod.rs", "alpha/src/frame/skeleton.rs", "alpha/src/frame/holes.rs", "alpha/src/holes/decide.rs", "alpha/src/holes/rate.rs"] {
        assert!(wrote.iter().any(|w| w == f), "{f} in {wrote:?}");
    }
    // The starters compile as they are: each hole a `todo!()`.
    let starter = std::fs::read_to_string(root.join("alpha/src/holes/decide.rs")).unwrap();
    assert!(starter.contains("pub fn on_attack(_ctx: &mut dyn DecideCtx, _id: Id, _score: Score) -> Decision"), "{starter}");
    let (ok, out) = rustc_test(root);
    assert!(!ok && out.contains("not yet implemented"), "{out}");
    // A second gen keeps the worker's holes.
    write_file(root, "alpha/src/holes/decide.rs", DECIDE);
    write_file(root, "alpha/src/holes/rate.rs", RATE);
    write_file(root, "beta/src/holes/pack.rs", PACK);
    write_file(root, "beta/src/holes/count.rs", COUNT);
    let (wrote, _) = arch::sync_bindings(root, &loaded, true);
    assert!(wrote.is_empty(), "{wrote:?}");
    let (ok, out) = rustc_test(root);
    assert!(ok, "{out}");
    let (ok, out) = rustc_crate(root, "beta");
    assert!(ok, "{out}");
    let rep = check(root);
    assert!(rep.pass(), "{:?}", rep.problems);
    assert!(rep.headroom.iter().any(|h| h == "frame alpha: fns 2, holes 3, api calls 1, provides alpha-ui"), "{:?}", rep.headroom);

    // A hole reaching around its context fails the check, and the build
    // would have let it through.
    write_file(root, "alpha/src/holes/rate.rs", "pub fn rate(_ctx: &mut dyn RateCtx, _s: Score) -> Tally {\n    \
        let _ = crate::core_alpha::HeroId::new(1);\n    std::fs::read(\"/etc/hostname\").ok();\n    Tally(1, true)\n}\n");
    let (ok, out) = rustc_test(root);
    assert!(ok, "{out}");
    let all = check(root).problems.join("\n");
    assert!(all.contains("alpha/src/holes/rate.rs:2: `crate::core_alpha::HeroId::new`"), "{all}");
    assert!(all.contains("`std::fs` reaches the world"), "{all}");
    write_file(root, "alpha/src/holes/rate.rs", RATE);
    // A file the frame does not declare is reported, in the skeleton
    // directory and among the holes.
    write_file(root, "alpha/src/frame/extra.rs", "");
    write_file(root, "alpha/src/holes/gone.rs", "");
    let all = check(root).problems.join("\n");
    assert!(all.contains("alpha/src/frame/extra.rs: no file of frame alpha's skeleton"), "{all}");
    assert!(all.contains("alpha/src/holes/gone.rs: a hole file of no fn of frame alpha"), "{all}");
    std::fs::remove_file(root.join("alpha/src/frame/extra.rs")).unwrap();
    std::fs::remove_file(root.join("alpha/src/holes/gone.rs")).unwrap();
    // The skeleton is pinned to its Lean source.
    let skel = root.join("alpha/src/frame/skeleton.rs");
    std::fs::write(&skel, std::fs::read_to_string(&skel).unwrap() + "\npub fn extra() {}\n").unwrap();
    let all = check(root).problems.join("\n");
    assert!(all.contains("alpha/src/frame/skeleton.rs: generated binding or frame is missing or differs"), "{all}");
}

#[test]
fn a_frame_names_what_its_module_sees_and_lives_in_its_own_file() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    let bad = FRAME.replace("calls rate, «core-alpha».load", "calls rate, «core-alpha».store, gone");
    write_lean_arch(root, MANIFEST, &[("core-alpha", CORE_ALPHA), ("alpha-ui", ALPHA_UI)], &[("Frames/alpha", &bad)]);
    write_file(root, "core/lib.rs", "");
    write_file(root, "app/main.rs", "");
    let loaded = arch::load(root).expect("load");
    let (_, errs) = arch::sync_bindings(root, &loaded, false);
    let all = errs.join("\n");
    assert!(all.contains("fn decide calls core-alpha.store: core-alpha has no fn store"), "{all}");
    assert!(all.contains("fn decide calls gone: no frame fn gone"), "{all}");
    // A frame declared outside Frames/<module>.lean is refused at export.
    write_file(root, "architecture/Frames/alpha.lean", "import Architecture\nopen Dashboard\n");
    write_file(root, "architecture/Tests/x.lean", FRAME);
    let e = arch::load(root).unwrap_err();
    assert!(e.contains("architecture/Tests/x.lean: declares the frame of alpha"), "{e}");
}
