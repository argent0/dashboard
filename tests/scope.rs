// `dashboard scope` and `dashboard test` from a worker's clone (spec §8.3,
// §6.6): the task's modules, what it may write, its run set, and that run
// set run the way the player runs it, here or through the player's queue.

mod common;

use common::*;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{NullSessions, Player};
use dashboard::queue;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

/// Main's suite: `unit` (all), `story` covering both modules with its own
/// file, and `b_only` covering `b`, which always fails.
const SUITE: &str = "import Architecture\nopen Dashboard\n\n\
check unit where\n  cmd = \"true\"\n  covers = all\n\n\
check story where\n  cmd = \"sh stories/s.sh\"\n  covers = [a, b]\n  files = [\"stories/s.sh\"]\n\n\
check b_only where\n  cmd = \"false\"\n  covers = [b]\n";

/// A hub on the `a`/`b` architecture (runs `runs`) with task `s` planned on
/// `modules = ["a"]`, posted (`s-check`: `test -f a/feature.txt`) and
/// launched; returns the worker's clone.
fn launched(runs: &str) -> (DirectorRepo, TempDir, Home, Player<NullSessions>, PathBuf) {
    launched_with(runs, &format!("{LEAN_AB_MANIFEST}{APP_ROOT}"), "\"a\"")
}

/// `launched` on another manifest, with `s` on `modules = [<modules>]`.
fn launched_with(runs: &str, manifest: &str, modules: &str) -> (DirectorRepo, TempDir, Home, Player<NullSessions>, PathBuf) {
    let repo = director_repo();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(repo.path(), "roadmap.toml", &roadmap.replace("runs = \"parallel\"", &format!("runs = \"{runs}\"")));
    write_lean_arch(repo.path(), manifest, &[("ab", LEAN_AB_IDL)], &[("Tests", SUITE)]);
    for (f, t) in [("a/lib.rs", ""), ("b/lib.rs", ""), ("api/.keep", ""), ("stories/s.sh", "true\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "modules a and b");
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init");
    let home = Home { root: report.home };
    let mut player = Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("open");
    let plan = format!("title = \"s\"\nphase = \"p1\"\nmodules = [{modules}]\n");
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", &plan, "plan: s");
    player.tick().expect("plan tick");
    write_file(repo.path(), "tasks/s/prompt.md", "make a/feature.txt\n");
    write_file(repo.path(), "tasks/s/task.toml", &format!("{plan}\n[check]\nname = \"s-check\"\ncmd = \"test -f a/feature.txt\"\n"));
    write_file(repo.path(), &tests_file("s"), &lean_task_tests("s-check", "test -f a/feature.txt", "[a]"));
    commit(repo.path(), "post: s");
    let report = player.tick().expect("post tick");
    assert_eq!(report.launched, ["s"], "{report}");
    let clone = paths::worker_clone(&repo.director_path(), "s");
    (repo, home_dir, home, player, clone)
}

fn dash(clone: &PathBuf, home: &Home, cmd: &str) -> (i32, String) {
    dash_args(clone, home, &[cmd])
}

/// `dashboard <args…> --home <home>` in `clone`: exit code and output.
fn dash_args(clone: &PathBuf, home: &Home, args: &[&str]) -> (i32, String) {
    let mut all = args.to_vec();
    all.extend(["--home", home.root.to_str().unwrap()]);
    let out = run_dashboard(&all, clone);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn scope_lists_modules_writable_paths_and_the_run_set() {
    let (_repo, _home_dir, home, _player, clone) = launched("parallel");
    let (code, text) = dash(&clone, &home, "scope");
    assert_eq!(code, 0, "{text}");
    assert!(text.starts_with("task/s  modules: a\n"), "{text}");
    assert!(text.contains("  a ") && text.contains("architecture/Tests/s.worker.lean"), "{text}");
    assert!(!text.contains("  b "), "{text}");
    assert!(text.contains("run set (3 checks):"), "{text}");
    for name in ["unit", "story", "s-check"] {
        assert!(text.contains(&format!("  {name}")), "{name}: {text}");
    }
    assert!(!text.contains("b_only"), "{text}");
}

#[test]
fn workspace_lists_the_tree_the_worker_sees() {
    let (_repo, _home_dir, home, _player, clone) = launched("parallel");
    let hub = Git::new(home.hub());
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();
    let runner = dashboard::checks::Runner::for_cli(&home);
    let w = dashboard::records::workspace_at(&hub, "s", &tip, Some(runner.exporter())).expect("workspace");
    let ro: Vec<&str> = w.readonly.iter().map(|(p, _)| p.as_str()).collect();
    for p in ["architecture/", "tasks/s/", "api", "stories/s.sh", ".gitignore"] {
        assert!(ro.contains(&p), "{p}: {ro:?}");
    }
    let all: Vec<&str> = ro.iter().copied().chain(w.scope.paths.iter().map(|p| p.as_str())).collect();
    assert!(!all.iter().any(|p| p.starts_with('b')), "{all:?}");
    let (code, text) = dash(&clone, &home, "scope");
    assert_eq!(code, 0, "{text}");
    let tree = text.split("working tree:\n").nth(1).expect("a working tree section");
    let tree = tree.split("run set").next().unwrap();
    assert!(tree.contains("  api ") && tree.contains("  stories/s.sh "), "{text}");
    assert!(!tree.contains("b/"), "{text}");
}

#[test]
fn test_runs_the_run_set_in_its_own_worktree() {
    let (_repo, _home_dir, home, _player, clone) = launched("parallel");
    let (code, text) = dash(&clone, &home, "test");
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("s-check"), "{text}");
    // `b_only` fails too, but it covers another module: not this task's.
    write_file(&clone, "a/feature.txt", "x\n");
    let (code, text) = dash(&clone, &home, "test");
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("b_only"), "{text}");
    let hub = Git::new(home.hub());
    assert!(hub.for_each_ref("refs/dashboard/test/*").is_empty());
    assert!(Git::new(&clone).for_each_ref("refs/dashboard/test/*").is_empty());
}

#[test]
fn a_queued_run_runs_the_run_set() {
    let (_repo, _home_dir, home, mut player, clone) = launched("queued");
    write_file(&clone, "a/feature.txt", "x\n");
    let wt = Git::new(&clone);
    let snap = queue::snapshot(&wt).unwrap();
    let req = queue::enqueue(&home, &wt, "s", &snap, &[], 1).unwrap();
    player.tick().expect("serve tick");
    let (pass, text) = queue::wait(&home, &req.id, Some(Duration::from_secs(1))).expect("result");
    assert!(pass, "{text}");
    assert!(!text.contains("b_only"), "{text}");
}

#[test]
fn test_runs_only_the_named_checks() {
    let (_repo, _home_dir, home, _player, clone) = launched("parallel");
    let (code, text) = dash_args(&clone, &home, &["test", "unit"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("running unit of the run set of a"), "{text}");
    assert!(text.contains("ok   unit") && !text.contains("story") && !text.contains("s-check"), "{text}");
    let (code, text) = dash_args(&clone, &home, &["test", "s-check", "unit"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("ok   unit") && text.contains("FAIL s-check") && !text.contains("story"), "{text}");
}

#[test]
fn a_check_outside_the_run_set_is_refused() {
    let (_repo, _home_dir, home, _player, clone) = launched("parallel");
    for name in ["b_only", "nope"] {
        let (code, text) = dash_args(&clone, &home, &["test", name]);
        assert_eq!(code, 1, "{text}");
        assert!(text.contains(&format!("check \"{name}\" is outside the run set")), "{text}");
        for n in ["unit", "story", "s-check"] {
            assert!(text.contains(n), "{n}: {text}");
        }
    }
    assert!(Git::new(home.hub()).for_each_ref("refs/dashboard/test/*").is_empty());
}

#[test]
fn a_queued_run_takes_check_names() {
    let (_repo, _home_dir, home, mut player, clone) = launched("queued");
    let wt = Git::new(&clone);
    let snap = queue::snapshot(&wt).unwrap();
    let req = queue::enqueue(&home, &wt, "s", &snap, &["unit".into()], 1).unwrap();
    player.tick().expect("serve tick");
    let (pass, text) = queue::wait(&home, &req.id, Some(Duration::from_secs(1))).expect("result");
    assert!(pass, "{text}");
    assert!(text.contains("ok   unit") && !text.contains("s-check") && !text.contains("story"), "{text}");
    // A name outside the run set fails the queued run with the allowed names.
    let req = queue::enqueue(&home, &wt, "s", &snap, &["b_only".into()], 2).unwrap();
    player.tick().expect("serve tick");
    let (pass, text) = queue::wait(&home, &req.id, Some(Duration::from_secs(1))).expect("result");
    assert!(!pass && text.contains("outside the run set"), "{text}");
}

/// `s`'s worker file declaring `s_own` (covering `a`), whose file is
/// `stories/own.sh`, outside the worker's tree.
const S_OWN: &str = "import Architecture\nopen Dashboard\n\n\
check s_own where\n  cmd = \"sh stories/own.sh\"\n  covers = [a]\n  files = [\"stories/own.sh\"]\n";

fn staged(clone: &PathBuf) -> String {
    let git = Git::new(clone);
    let _ = git.run(&["add", "-A"]);
    let names = git.run(&["diff", "--cached", "--name-only"]).unwrap();
    git.run(&["reset", "-q"]).unwrap();
    names
}

#[test]
fn declaring_a_check_opens_its_files_for_the_next_commit() {
    let (_repo, _home_dir, home, mut player, clone) = launched("parallel");
    let hub = Git::new(home.hub());
    write_file(&clone, "architecture/Tests/s.worker.lean", S_OWN);
    write_file(&clone, "stories/own.sh", "true\n");
    write_file(&clone, "a/feature.txt", "x\n");
    let names = staged(&clone);
    assert!(names.contains("architecture/Tests/s.worker.lean") && names.contains("a/feature.txt"), "{names}");
    assert!(!names.contains("stories/own.sh"), "outside the tree until declared: {names}");

    let (code, text) = dash(&clone, &home, "test");
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("opened: stories/own.sh"), "{text}");
    assert!(text.contains("ok   s_own"), "the second snapshot carries the file: {text}");
    assert!(staged(&clone).contains("stories/own.sh"));

    let git = Git::new(&clone);
    for (k, v) in [("user.name", "w"), ("user.email", "w@example.com"), ("commit.gpgsign", "false")] {
        git.run(&["config", k, v]).unwrap();
    }
    git.run(&["add", "-A"]).unwrap();
    git.run(&["commit", "-q", "-m", "finish with s_own"]).unwrap();
    let report = player.tick().expect("finish tick");
    assert!(report.rejected.is_empty(), "{report}");
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();
    assert!(hub.show_file(&tip, "stories/own.sh").is_some(), "{report}");
    let runner = dashboard::checks::Runner::for_cli(&home);
    let scope = dashboard::records::scope_at(&hub, "s", &tip, Some(runner.exporter())).unwrap();
    assert!(scope.run_set.contains(&"s_own".to_string()), "{:?}", scope.run_set);
}

#[test]
fn a_full_tree_needs_no_widening() {
    let (_repo, _home_dir, home, _player, clone) = launched("parallel");
    Git::new(&clone).run(&["sparse-checkout", "disable"]).unwrap();
    write_file(&clone, "architecture/Tests/s.worker.lean", S_OWN);
    write_file(&clone, "stories/own.sh", "true\n");
    write_file(&clone, "a/feature.txt", "x\n");
    let (code, text) = dash(&clone, &home, "test");
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("opened:"), "{text}");
    assert!(text.contains("ok   s_own"), "{text}");
}

/// The `a`/`b` manifest with `build_cmd`s: `a`'s proves its view lacks
/// `b`, root `app`'s proves the full worktree has both; `b` has none.
fn build_manifest(a_cmd: &str) -> String {
    let app = "test -e b/lib.rs && test -e a/lib.rs && echo built-app";
    LEAN_AB_MANIFEST.replace("max_ast = 100\n", &format!("max_ast = 100\n  build_cmd = {a_cmd:?}\n"))
        + &format!("{APP_ROOT}  build_cmd = {app:?}\n")
}

const BUILD_A: &str = "test ! -e b/lib.rs && echo built-a";

#[test]
fn build_compiles_modules_in_their_views_and_roots_in_the_full_tree() {
    let (_repo, _home_dir, home, _player, clone) = launched_with("parallel", &build_manifest(BUILD_A), "\"a\", \"app\"");
    assert!(!clone.join("b").exists());
    let (code, text) = dash(&clone, &home, "build");
    assert_eq!(code, 0, "{text}");
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s}: {text}"));
    assert!(at("building a (view)") < at("built-a") && at("built-a") < at("building app (full worktree)") && at("building app (full worktree)") < at("built-app"), "{text}");
    let (code, text) = dash_args(&clone, &home, &["build", "a"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("built-a") && !text.contains("built-app"), "{text}");
    assert!(Git::new(home.hub()).for_each_ref("refs/dashboard/test/*").is_empty());
}

#[test]
fn build_refuses_a_node_outside_the_task() {
    let (_repo, _home_dir, home, _player, clone) = launched_with("parallel", &build_manifest(BUILD_A), "\"a\", \"app\"");
    let (code, text) = dash_args(&clone, &home, &["build", "b"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("\"b\" is outside the task's modules: a, app"), "{text}");
}

#[test]
fn build_reports_a_node_without_build_cmd() {
    let (_repo, _home_dir, home, _player, clone) = launched_with("parallel", &build_manifest(BUILD_A), "\"a\", \"b\"");
    let (code, text) = dash(&clone, &home, "build");
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("built-a") && text.contains("b: no build_cmd"), "{text}");
}

#[test]
fn build_runs_locally_when_runs_is_queued() {
    let (_repo, _home_dir, home, _player, clone) = launched_with("queued", &build_manifest(BUILD_A), "\"a\"");
    let (code, text) = dash(&clone, &home, "build");
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("built-a"), "{text}");
    let queued = std::fs::read_dir(home.queue()).map(|d| d.count()).unwrap_or(0);
    assert_eq!(queued, 0, "no request filed");
    assert!(Git::new(home.hub()).for_each_ref("refs/dashboard/test/*").is_empty());
}

#[test]
fn build_stops_at_the_first_failure() {
    let (_repo, _home_dir, home, _player, clone) = launched_with("parallel", &build_manifest("false"), "\"a\", \"app\"");
    let (code, text) = dash(&clone, &home, "build");
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("a: build_cmd exited 1") && !text.contains("building app"), "{text}");
}
