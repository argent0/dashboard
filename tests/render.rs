// Integration tests for the CLI's `status` output (src/render.rs + src/
// main.rs's `run_status`, step 8 of the migration plan). Builds a real hub
// through the player (plan two tasks, post and finish one via the subagent
// clone with `NullSessions`, exactly like tests/player_subagent.rs), then
// runs the actual `dashboard` binary against that home and checks the
// rendered text.

mod common;

use common::*;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{NullSessions, Player};
use tempfile::TempDir;

// --------------------------------------------------------------------------
// Fixture helpers (trimmed copies of tests/player_subagent.rs's, which are
// private to that test binary)
// --------------------------------------------------------------------------

fn init_hub(repo: &DirectorRepo) -> (TempDir, Home) {
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init should succeed");
    (home_dir, Home { root: report.home })
}

fn open_player(home: &Home) -> Player<NullSessions> {
    Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("player open")
}

/// Plans `slug` on a fresh `task/<slug>` branch (no deps) and ticks.
fn plan(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) {
    director_commit_on_branch(repo.path(), &format!("task/{slug}"), &format!("tasks/{slug}/task.toml"), &format!("title = \"{slug} title\"\nphase = \"p1\"\n"), &format!("plan: {slug}"));
    let report = player.tick().expect("plan tick should succeed");
    assert!(report.rejected.is_empty(), "plan: {slug} unexpectedly rejected: {report}");
}

/// Posts `slug` with a task check that fails until `feature.txt` exists, and
/// ticks (which also launches the subagent, via `NullSessions`).
fn post(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) {
    checkout_branch(repo.path(), &format!("task/{slug}"));
    write_file(repo.path(), &format!("tasks/{slug}/prompt.md"), "do the thing\n");
    write_file(
        repo.path(),
        &format!("tasks/{slug}/task.toml"),
        &format!("title = \"{slug} title\"\nphase = \"p1\"\n\n[check]\nname = \"{slug}-check\"\ncmd = \"test -f feature.txt\"\n"),
    );
    write_file(repo.path(), &format!("tests.d/{slug}.toml"), &format!("[[check]]\nname = \"{slug}-check\"\ncmd = \"test -f feature.txt\"\n"));
    commit(repo.path(), &format!("post: {slug}"));
    let report = player.tick().expect("post tick should succeed");
    assert_eq!(report.launched, vec![slug.to_string()], "{report}");
    assert!(report.errors.is_empty(), "{report}");
}

/// Simulates the subagent's finish commit directly in the clone the player
/// created (`paths::subagent_clone`) — the player fetches from that clone
/// itself (spec's "provenance by fetch source"), no push needed.
fn finish(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) {
    let dir = paths::subagent_clone(&repo.director_path(), slug);
    let git = Git::new(&dir);
    git.run(&["config", "user.name", "Test Subagent"]).expect("set user.name");
    git.run(&["config", "user.email", "subagent@example.com"]).expect("set user.email");
    git.run(&["config", "commit.gpgsign", "false"]).expect("disable gpgsign");
    write_file(&dir, "feature.txt", "done\n");
    commit(&dir, "finish work");
    let report = player.tick().expect("finish tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
}

// --------------------------------------------------------------------------
// Happy path: plan two, post + finish one, render the result
// --------------------------------------------------------------------------

#[test]
fn status_renders_ready_and_tested_tasks_from_a_real_hub() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan(&repo, &mut player, "alpha");
    plan(&repo, &mut player, "beta");
    post(&repo, &mut player, "alpha");
    finish(&repo, &mut player, "alpha");

    let output = run_dashboard(&["--home", home.root.to_str().unwrap(), "--no-color", "--width", "100"], repo.path());
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Header and overall progress: two live tasks, none accepted yet.
    assert!(stdout.contains("Test Project — Roadmap"), "{stdout}");
    assert!(stdout.contains("0/2 accepted (0%)"), "{stdout}");

    // `beta` is ready (planned, no unmet deps) and shows up in Ready.
    assert!(stdout.contains("▶ Ready (1)"), "{stdout}");
    assert!(stdout.contains("beta"), "{stdout}");

    // `alpha` finished successfully: tested, shown in the in-flight block
    // with its round, cached suite result and (dead, since NullSessions
    // never actually starts a session) tmux session.
    assert!(stdout.contains("In flight"), "{stdout}");
    assert!(stdout.contains("tested"), "{stdout}");
    assert!(stdout.contains("alpha"), "{stdout}");
    assert!(stdout.contains("r0/2"), "{stdout}");
    assert!(stdout.contains("alpha-check: pass"), "{stdout}");
    assert!(stdout.contains("dash-proj-alpha"), "{stdout}");

    // Empty sections still print, dimmed "(none)".
    assert!(stdout.contains("✗ Escalated (0)"), "{stdout}");
    assert!(stdout.contains("⊘ Orphaned (0)"), "{stdout}");
    assert!(stdout.contains("✝ Retired (0)"), "{stdout}");
}

// --------------------------------------------------------------------------
// No home configured: a clean error, not a panic
// --------------------------------------------------------------------------

#[test]
fn status_with_no_home_errors_cleanly() {
    let dir = TempDir::new().expect("tempdir");
    // A plain (non-git) directory: `--home` is omitted and there's no
    // `dashboard.home` to read from the (nonexistent) repo config.
    let output = run_dashboard(&["--no-color"], dir.path());
    assert!(!output.status.success(), "expected a non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dashboard init"), "{stderr}");
}
