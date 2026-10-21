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
    let repo = director_repo();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(repo.path(), "roadmap.toml", &roadmap.replace("runs = \"parallel\"", &format!("runs = \"{runs}\"")));
    write_lean_arch(repo.path(), &format!("{LEAN_AB_MANIFEST}{APP_ROOT}"), &[("ab", LEAN_AB_IDL)], &[("Tests", SUITE)]);
    for (f, t) in [("a/lib.rs", ""), ("b/lib.rs", ""), ("api/.keep", ""), ("stories/s.sh", "true\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "modules a and b");
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init");
    let home = Home { root: report.home };
    let mut player = Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("open");
    let plan = "title = \"s\"\nphase = \"p1\"\nmodules = [\"a\"]\n";
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", plan, "plan: s");
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
    let out = run_dashboard(&[cmd, "--home", home.root.to_str().unwrap()], clone);
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
    let req = queue::enqueue(&home, &wt, "s", &snap, 1).unwrap();
    player.tick().expect("serve tick");
    let (pass, text) = queue::wait(&home, &req.id, Some(Duration::from_secs(1))).expect("result");
    assert!(pass, "{text}");
    assert!(!text.contains("b_only"), "{text}");
}
