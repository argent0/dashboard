// Integration tests for the `flaky-test-fix` task kind (src/flaky.rs and the
// post/launch/finish paths of src/player.rs). The "test project" is a text
// inventory (`tests/inventory.txt`, one test name per line, printed by
// `list_cmd`), so the flake is a counter file the evidence command bumps:
// it passes on every other run, deterministically.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{NullSessions, Player};
use dashboard::records;
use dashboard::state::{self, Status};
use std::path::Path;
use tempfile::TempDir;

fn init_hub(repo: &DirectorRepo) -> (TempDir, Home) {
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init should succeed");
    (home_dir, Home { root: report.home })
}

fn status_of(hub: &Git, slug: &str) -> Status {
    let hub_state = state::derive(hub, &|_| false).expect("derive should succeed");
    hub_state.tasks.iter().find(|t| t.slug == slug).unwrap_or_else(|| panic!("no task state for {slug:?}")).status
}

/// An evidence command that passes on every other run (counter in `dir`).
fn alternating(dir: &Path) -> String {
    let f = dir.join("count");
    format!("n=$(cat {f} 2>/dev/null || echo 0); echo $((n+1)) > {f}; test $((n % 2)) -eq 0", f = f.display())
}

fn task_toml(evidence_cmd: &str) -> String {
    format!(
        "title = \"fix\"\nphase = \"p1\"\nkind = \"flaky-test-fix\"\nfix_paths = [\"tests/inventory.txt\"]\n\n\
[[replace]]\nold = \"flaky\"\nnew = \"flaky_stable\"\n\n\
[[evidence]]\nname = \"flake\"\ncmd = {evidence_cmd:?}\nruns = 4\nmin_pass = 1\nmax_pass = 3\n\n\
[check]\nname = \"stable\"\ncmd = \"grep -qx flaky_stable tests/inventory.txt\"\n"
    )
}

const TESTS_TOML: &str = "test_paths = [\"tests/\"]\nlist_cmd = \"cat tests/inventory.txt\"\n\n[[check]]\nname = \"unit\"\ncmd = \"true\"\n";

/// Plans and posts `fix` (a flaky-test-fix), returning the player and hub.
fn plan_and_post(repo: &DirectorRepo, home: &Home, evidence_cmd: &str, tests_toml: &str) -> (Player<NullSessions>, dashboard::player::TickReport) {
    let mut player = Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("player open");
    director_commit_on_branch(repo.path(), "task/fix", "tasks/fix/task.toml", &task_toml(evidence_cmd), "plan: fix");
    let report = player.tick().expect("plan tick");
    assert!(report.rejected.is_empty(), "{report}");

    write_file(repo.path(), "tasks/fix/prompt.md", "make it stable\n");
    write_file(repo.path(), "tests/inventory.txt", "keep\nflaky\n");
    write_file(repo.path(), "tests.toml", tests_toml);
    commit(repo.path(), "post: fix");
    let report = player.tick().expect("post tick");
    (player, report)
}

#[test]
fn a_reproduced_flake_posts_launches_and_unpins_only_fix_paths() {
    let repo = director_repo();
    let (home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let (_player, report) = plan_and_post(&repo, &home, &alternating(home_dir.path()), TESTS_TOML);

    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.launched, vec!["fix".to_string()], "{report}");
    assert_eq!(status_of(&hub, "fix"), Status::Posted);

    let pinned = records::pinned_files(&hub, "fix").unwrap();
    assert!(pinned.contains_key("tests.toml"));
    assert!(pinned.contains_key("tasks/fix/task.toml"));
    assert!(!pinned.contains_key("tests/inventory.txt"), "fix_paths are unpinned: {pinned:?}");

    // The subagent is told which files are its to edit.
    let clone = paths::subagent_clone(&repo.director_path(), "fix");
    let git_dir = Git::new(&clone).run(&["rev-parse", "--absolute-git-dir"]).unwrap();
    let prompt = std::fs::read_to_string(Path::new(&git_dir).join("dashboard-prompt.md")).unwrap();
    assert!(prompt.contains("flaky-test-fix") && prompt.contains("flaky_stable"), "{prompt}");
}

#[test]
fn evidence_that_does_not_reproduce_retires_the_task_on_the_first_miss() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let (_player, report) = plan_and_post(&repo, &home, "true", TESTS_TOML);

    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Evidence);
    assert!(report.launched.is_empty(), "{report}");

    assert_eq!(status_of(&hub, "fix"), Status::Retired);
    assert!(!hub.ref_exists("refs/heads/task/fix"));
    let mortem = records::post_mortem(&hub, "fix").unwrap();
    assert!(mortem.contains("evidence did not reproduce (4/4)"), "{mortem}");
}

#[test]
fn a_higher_evidence_limit_only_strikes() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    write_file(repo.path(), "roadmap.toml", "title = \"t\"\n\n[[phases]]\nid = \"p1\"\n\n[limits.strikes]\nevidence = 2\n");
    commit(repo.path(), "docs: raise the evidence limit");
    let mut player = Player::open(Home { root: home.root.clone() }, NullSessions::default()).unwrap();
    player.tick().unwrap();

    let (_player, report) = plan_and_post(&repo, &home, "true", TESTS_TOML);
    assert_eq!(report.rejected[0].reason, Reason::Evidence, "{report}");
    assert_eq!(status_of(&hub, "fix"), Status::Ready, "one miss of two is only a strike");
}

#[test]
fn the_stress_check_may_not_join_tests_toml() {
    let repo = director_repo();
    let (home_dir, home) = init_hub(&repo);
    let tests = format!("{TESTS_TOML}\n[[check]]\nname = \"stable\"\ncmd = \"grep -qx flaky_stable tests/inventory.txt\"\n");
    let (_player, report) = plan_and_post(&repo, &home, &alternating(home_dir.path()), &tests);
    assert_eq!(report.rejected[0].reason, Reason::Post, "{report}");
    assert!(report.rejected[0].message.contains("tests.toml"), "{report}");
}

#[test]
fn a_flaky_test_fix_needs_list_cmd() {
    let repo = director_repo();
    let (home_dir, home) = init_hub(&repo);
    let tests = "test_paths = [\"tests/\"]\n\n[[check]]\nname = \"unit\"\ncmd = \"true\"\n";
    let (_player, report) = plan_and_post(&repo, &home, &alternating(home_dir.path()), tests);
    assert_eq!(report.rejected[0].reason, Reason::Post, "{report}");
    assert!(report.rejected[0].message.contains("list_cmd"), "{report}");
}

#[test]
fn finish_replaces_the_flaky_test_and_nothing_else() {
    let repo = director_repo();
    let (home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let (mut player, _) = plan_and_post(&repo, &home, &alternating(home_dir.path()), TESTS_TOML);
    let clone = paths::subagent_clone(&repo.director_path(), "fix");
    let git = Git::new(&clone);
    git.run(&["config", "user.name", "Test Subagent"]).unwrap();
    git.run(&["config", "user.email", "subagent@example.com"]).unwrap();
    git.run(&["config", "commit.gpgsign", "false"]).unwrap();

    // Dropping another test is rejected as a suite strike, and relayed.
    write_file(&clone, "tests/inventory.txt", "flaky_stable\n");
    commit(&clone, "finish: lost a test");
    let report = player.tick().unwrap();
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Suite);
    assert!(report.rejected[0].message.contains("keep"), "{report}");
    assert_eq!(status_of(&hub, "fix"), Status::Posted);

    // A replacement that isn't there is rejected too (the check fails).
    write_file(&clone, "tests/inventory.txt", "keep\nflaky\n");
    commit(&clone, "finish: nothing replaced");
    let report = player.tick().unwrap();
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert!(report.rejected[0].message.contains("stable"), "{report}");
}

#[test]
fn finish_with_the_replacement_is_tested() {
    let repo = director_repo();
    let (home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let (mut player, _) = plan_and_post(&repo, &home, &alternating(home_dir.path()), TESTS_TOML);
    let clone = paths::subagent_clone(&repo.director_path(), "fix");
    let git = Git::new(&clone);
    git.run(&["config", "user.name", "Test Subagent"]).unwrap();
    git.run(&["config", "user.email", "subagent@example.com"]).unwrap();
    git.run(&["config", "commit.gpgsign", "false"]).unwrap();

    write_file(&clone, "tests/inventory.txt", "keep\nflaky_stable\n");
    commit(&clone, "finish: replace the flaky test");
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status_of(&hub, "fix"), Status::Tested);
}
