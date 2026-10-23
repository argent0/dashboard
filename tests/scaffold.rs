// A module enters a project as one director `main` commit
// (architect-v3.md, "Adding a module"): `scaffold::add` writes the
// manifest entries, stub IDLs, the module's scaffold and its bindings, and
// the player admits exactly that skeleton on `main`. Needs `lake`.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::player::{NullSessions, Player};
use dashboard::sandbox::Sandbox;
use dashboard::scaffold::{self, ApiSpec, Build, Request};
use dashboard::{arch, lean};
use std::path::Path;
use tempfile::TempDir;

/// Two modules over `mods/`, the root `app`, one API `a → b`, and a
/// `lock.txt` every build appends to (a lockfile in `shared`).
const MANIFEST: &str = "import Dashboard\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n  shared = [\"lock.txt\"]\n  source_paths = [\"mods\"]\n\n\
-- The first.\nmodule «a» where\n  lang = rust\n  paths = [\"mods/a\"]\n\n\
-- The second.\nmodule «b» where\n  lang = rust\n  paths = [\"mods/b\"]\n\n\
root app where\n  lang = rust\n  paths = [\"src\"]\n\n\
api «a-b» : «a» → «b» where\n  paths = [\"apis/a-b\"]\n";

const AB_IDL: &str = "import Architecture\nopen Dashboard\n\nidl «a-b» where\n  constructor X\n";

const MODULE_TEMPLATE: &str = "-- {name}: what it owns.\nmodule «{name}» where\n  lang = rust\n  paths = [\"mods/{name}\"]\n  \
test_cmd = \"test -f mods/{name}/lib.rs\"\n  build_cmd = \"echo {name} >> lock.txt\"\n";

const LIB_TEMPLATE: &str = "//! {name}\npub fn {crate}() {}\n";

fn write_project(root: &Path) {
    write_lean_arch(root, MANIFEST, &[("a-b", AB_IDL)], &[]);
    write_file(root, "architecture/scaffold/module.lean", MODULE_TEMPLATE);
    write_file(root, "architecture/scaffold/files/mods/{name}/lib.rs", LIB_TEMPLATE);
    write_file(root, "mods/a/lib.rs", "pub fn a() {}\n");
    write_file(root, "mods/b/lib.rs", "pub fn b() {}\n");
    write_file(root, "lock.txt", "a\nb\n");
    write_file(root, "src/.keep", "");
    let loaded = arch::load(root).expect("load");
    let (_, errs) = arch::sync_bindings(root, &loaded, true);
    assert!(errs.is_empty(), "{errs:?}");
}

fn api(s: &str) -> ApiSpec {
    s.parse().unwrap()
}

fn arch_problems(root: &Path) -> Vec<String> {
    let loaded = arch::load(root).expect("load");
    let scratch = TempDir::new().unwrap();
    arch::check(root, &loaded, &Sandbox::off(), scratch.path()).problems
}

#[test]
fn arch_gen_writes_a_missing_api_dir_s_bindings_under_it() {
    let d = TempDir::new().unwrap();
    assert!(lean::available(), "these tests need `lake`");
    write_project(d.path());
    assert!(d.path().join("apis/a-b/gen/a_b.rs").is_file(), "the gen dir sits under the api's own path");
    assert!(!d.path().join("apis/gen").exists());
    assert_eq!(arch_problems(d.path()), Vec::<String>::new());
}

#[test]
fn add_module_writes_a_skeleton_that_passes_arch_check() {
    let d = TempDir::new().unwrap();
    write_project(d.path());
    let root = d.path();

    let out = scaffold::add(root, &Request { module: Some("c-d".into()), apis: vec![api("a:c-d"), api("c-d:b")] }).expect("add");
    assert_eq!(out.entries, ["module c-d", "api a-c-d", "api c-d-b"]);
    assert_eq!(out.wrote, ["architecture/Apis/a-c-d.lean", "architecture/Apis/c-d-b.lean", "mods/c-d/lib.rs"]);
    assert!(out.generated.contains(&"apis/a-c-d/gen/a_c_d.rs".to_string()), "{:?}", out.generated);
    assert!(matches!(&out.build, Build::Ran { ok: true, .. }), "{:?}", out.build);
    assert!(out.notes.iter().any(|n| n.contains("at its cap (3/3)")), "{:?}", out.notes);
    assert_eq!(std::fs::read_to_string(root.join("mods/c-d/lib.rs")).unwrap(), "//! c-d\npub fn c_d() {}\n");
    assert_eq!(std::fs::read_to_string(root.join("lock.txt")).unwrap(), "a\nb\nc-d\n");
    let manifest = std::fs::read_to_string(root.join("architecture/Architecture.lean")).unwrap();
    assert!(
        manifest.contains("  paths = [\"mods/b\"]\n\n-- c-d: what it owns.\nmodule «c-d» where\n  lang = rust\n  paths = [\"mods/c-d\"]\n"),
        "{manifest}"
    );
    assert!(manifest.ends_with("api «c-d-b» : «c-d» → «b» where\n  paths = [\"apis/c-d-b\"]\n"), "{manifest}");
    assert_eq!(arch_problems(root), Vec::<String>::new());

    // At the cap, a module waits for its raise; known names are refused.
    let e = scaffold::add(root, &Request { module: Some("e".into()), apis: vec![] }).unwrap_err();
    assert!(e.contains("max_modules is 3") && e.contains("dashboard raise"), "{e}");
    let e = scaffold::add(root, &Request { module: None, apis: vec![api("a:nope")] }).unwrap_err();
    assert!(e.contains("nope is neither a module nor a root"), "{e}");
    let e = scaffold::add(root, &Request { module: None, apis: vec![api("ab2=a:b")] }).unwrap_err();
    assert!(e.contains("already has api a-b"), "{e}");

    // APIs alone between existing nodes.
    let out = scaffold::add(root, &Request { module: None, apis: vec![api("b:app")] }).expect("add-api");
    assert_eq!(out.entries, ["api b-app"]);
    assert_eq!(out.build, Build::NoModule);
    assert_eq!(arch_problems(root), Vec::<String>::new());
}

#[test]
fn a_project_without_a_scaffold_is_told_how_to_add_one() {
    let d = TempDir::new().unwrap();
    write_project(d.path());
    std::fs::remove_dir_all(d.path().join("architecture/scaffold")).unwrap();
    let e = scaffold::add(d.path(), &Request { module: Some("c".into()), apis: vec![] }).unwrap_err();
    assert!(e.contains("no architecture/scaffold/module.lean") && e.contains("dashboard arch gen"), "{e}");
}

fn init_hub(repo: &DirectorRepo) -> (TempDir, Home) {
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init");
    (home_dir, Home { root: report.home })
}

#[test]
fn the_player_admits_a_new_module_s_skeleton_on_main_and_nothing_more() {
    let repo = director_repo();
    write_project(repo.path());
    commit(repo.path(), "architecture with a scaffold");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("open");
    player.tick().expect("tick");
    let main = |hub: &Git| hub.rev_parse("refs/heads/main").unwrap();

    // The skeleton: manifest, IDLs, bindings, the module's files, lock.txt.
    scaffold::add(repo.path(), &Request { module: Some("c".into()), apis: vec![api("a:c")] }).expect("add");
    let skeleton = commit(repo.path(), "add module c");
    let report = player.tick().expect("tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(main(&hub), skeleton);

    // An IDL edit on main brings its regenerated binding along.
    write_file(repo.path(), "architecture/Apis/a-c.lean", "import Architecture\nopen Dashboard\n\nidl «a-c» where\n  constructor Go\n");
    let loaded = arch::load(repo.path()).unwrap();
    let (wrote, _) = arch::sync_bindings(repo.path(), &loaded, true);
    assert_eq!(wrote, ["apis/a-c/gen/a_c.rs"]);
    let idl = commit(repo.path(), "a-c: Go");
    let report = player.tick().expect("tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(main(&hub), idl);

    // A new module with code of its own: refused.
    let good = idl;
    let manifest = std::fs::read_to_string(repo.path().join("architecture/Architecture.lean")).unwrap();
    write_file(repo.path(), "architecture/Architecture.lean", &manifest.replace("max_modules = 3", "max_modules = 4"));
    commit(repo.path(), "raise: room for d");
    player.tick().expect("tick");
    let raised = main(&hub);
    assert_ne!(raised, good);
    scaffold::add(repo.path(), &Request { module: Some("d".into()), apis: vec![] }).expect("add d");
    write_file(repo.path(), "mods/d/lib.rs", "pub fn d() -> u32 { 42 }\n");
    commit(repo.path(), "add module d with code");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(report.rejected[0].message.contains("mods/d/lib.rs differs from the scaffold"), "{}", report.rejected[0].message);
    assert_eq!(main(&hub), raised);

    // An existing module's code: refused, even beside a new module.
    repo.git().run(&["reset", "-q", "--hard", &raised]).unwrap();
    repo.git().run(&["clean", "-qfd"]).unwrap();
    scaffold::add(repo.path(), &Request { module: Some("d".into()), apis: vec![] }).expect("add d");
    write_file(repo.path(), "mods/a/lib.rs", "pub fn a() -> u8 { 1 }\n");
    commit(repo.path(), "add module d, touch a");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert!(report.rejected[0].message.contains("main commit touches \"mods/a/lib.rs\""), "{}", report.rejected[0].message);
    assert_eq!(main(&hub), raised);
}
