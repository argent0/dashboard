// Integration tests for the suite time budget (`[budget]` in roadmap.toml):
// a per-run timeout that kills a check, a total that rejects a slow suite
// with a `budget` strike, and a warn threshold that only reports.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{self, NullSessions, Player};
use dashboard::records::Transition;
use dashboard::state::{self, Status};
use std::path::Path;
use tempfile::TempDir;

/// A director repo whose suite has a `slow` check that sleeps 3s once a
/// worker adds `slow.flag`, under the given `[budget]`.
fn budget_repo(budget: &str) -> DirectorRepo {
    let repo = director_repo();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!("{roadmap}\n[budget]\n{budget}\n"),
    );
    write_file(
        repo.path(),
        TESTS_LEAN,
        &*suite_lean("[[check]]\nname = \"slow\"\ncmd = \"if test -f slow.flag; then sleep 3; fi\"\n"),
    );
    commit(repo.path(), "budget");
    repo
}

fn finish_slow(repo: &DirectorRepo, slug: &str) {
    commit_multi_in_clone(
        repo,
        slug,
        &[("feature.txt", "done\n"), ("slow.flag", "1\n")],
        "finish",
    );
}

fn init_hub(repo: &DirectorRepo) -> (TempDir, Home) {
    let home_dir = temp_home();
    let report = init::init(
        repo.path(),
        Some("proj"),
        Some(home_dir.path().join("home")),
    )
    .expect("init should succeed");
    (home_dir, Home { root: report.home })
}

fn open_player(home: &Home) -> Player<NullSessions> {
    Player::open(
        Home {
            root: home.root.clone(),
        },
        NullSessions::default(),
    )
    .expect("player open")
}

fn task_status(hub: &Git, slug: &str) -> Status {
    let hub_state = state::derive(hub, &|_| false).expect("derive should succeed");
    hub_state
        .tasks
        .iter()
        .find(|t| t.slug == slug)
        .unwrap_or_else(|| panic!("no task state for {slug:?}"))
        .status
}

/// Plans `slug` (no deps unless `task_toml` says otherwise) and ticks.
fn plan(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str, task_toml: &str) {
    director_commit_on_branch(
        repo.path(),
        &format!("task/{slug}"),
        &format!("tasks/{slug}/task.toml"),
        task_toml,
        &format!("plan: {slug}"),
    );
    let report = player.tick().expect("plan tick should succeed");
    assert!(
        report.rejected.is_empty(),
        "plan: {slug} unexpectedly rejected: {report}"
    );
}

/// Posts `slug` (already planned and `ready`) with a task check
/// `test -f feature.txt` (fails until the "worker" creates that file) and
/// the matching check in `architecture/Tests/<slug>.lean`. Ticks (which also launches the worker,
/// per step 6) and returns the report so callers can inspect `launched`/
/// `errors` too.
fn post(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) -> player::TickReport {
    checkout_branch(repo.path(), &format!("task/{slug}"));
    write_file(
        repo.path(),
        &format!("tasks/{slug}/prompt.md"),
        "do the thing\n",
    );
    write_file(
        repo.path(),
        &format!("tasks/{slug}/task.toml"),
        &format!(
            "title = \"{slug}\"\nphase = \"p1\"\nmodules = [\"app\"]\n\n[check]\nname = \"{slug}-check\"\ncmd = \"test -f feature.txt\"\n"
        ),
    );
    write_file(
        repo.path(),
        &*tests_file(&slug),
        &*task_suite_lean(&format!("[[check]]\nname = \"{slug}-check\"\ncmd = \"test -f feature.txt\"\n")),
    );
    commit(repo.path(), &format!("post: {slug}"));
    let report = player.tick().expect("post tick should succeed");
    assert_eq!(
        report.imported.len(),
        1,
        "post: {slug} not imported: {report}"
    );
    assert_eq!(report.imported[0].2, Transition::Post);
    report
}

/// plan -> post, asserting the launch succeeded (clone + pins + a launch
/// call, no errors). Returns the post report.
fn plan_and_post(
    repo: &DirectorRepo,
    player: &mut Player<NullSessions>,
    slug: &str,
) -> player::TickReport {
    plan(
        repo,
        player,
        slug,
        &format!("title = \"{slug}\"\nphase = \"p1\"\nmodules = [\"app\"]\n"),
    );
    let report = post(repo, player, slug);
    assert_eq!(report.launched, vec![slug.to_string()], "{report}");
    assert!(report.errors.is_empty(), "{report}");
    report
}

/// Sets a throwaway committer identity (and disables gpg signing) on a
/// worker clone — needed because `git clone` doesn't inherit the source
/// repo's local `user.name`/`user.email`.
fn ensure_identity(dir: &Path) {
    let git = Git::new(dir);
    git.run(&["config", "user.name", "Test Worker"])
        .expect("set user.name");
    git.run(&["config", "user.email", "worker@example.com"])
        .expect("set user.email");
    git.run(&["config", "commit.gpgsign", "false"])
        .expect("disable gpgsign");
}

/// Writes every `(rel, content)` pair into `slug`'s clone and commits them
/// together — simulating the worker's own commit, made directly in the
/// clone the player created (`paths::worker_clone`), never pushed
/// anywhere: the player fetches from this clone itself (spec's "provenance
/// by fetch source").
fn commit_multi_in_clone(
    repo: &DirectorRepo,
    slug: &str,
    files: &[(&str, &str)],
    msg: &str,
) -> String {
    let dir = paths::worker_clone(&repo.director_path(), slug);
    ensure_identity(&dir);
    for (rel, content) in files {
        write_file(&dir, rel, content);
    }
    commit(&dir, msg)
}

#[test]
fn a_check_past_check_timeout_is_killed_with_a_budget_strike() {
    let repo = budget_repo("check_timeout = 1");
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "t");
    finish_slow(&repo, "t");

    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Budget);
    assert!(
        report.rejected[0].message.contains("timed out"),
        "{}",
        report.rejected[0].message
    );
    assert_ne!(task_status(&hub, "t"), Status::Tested);
}

#[test]
fn a_passing_suite_over_suite_fail_is_rejected_with_budget() {
    let repo = budget_repo("suite_fail = 1");
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "f");
    finish_slow(&repo, "f");

    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Budget);
    assert!(
        report.rejected[0].message.contains("suite_fail"),
        "{}",
        report.rejected[0].message
    );
    assert!(
        report.rejected[0].message.contains("slow"),
        "{}",
        report.rejected[0].message
    );
    assert_ne!(task_status(&hub, "f"), Status::Tested);
}

#[test]
fn a_suite_over_suite_warn_is_imported_and_reported() {
    let repo = budget_repo("suite_warn = 1\nsuite_fail = 60\ncheck_timeout = 60");
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "w");
    finish_slow(&repo, "w");

    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Finish);
    assert_eq!(report.slow.len(), 1, "{report}");
    assert_eq!(report.slow[0].0, "w");
    assert_eq!(task_status(&hub, "w"), Status::Tested);
}

#[test]
fn no_budget_means_no_limits() {
    let repo = budget_repo("");
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "n");
    finish_slow(&repo, "n");

    let report = player.tick().expect("tick");
    assert!(
        report.rejected.is_empty() && report.slow.is_empty(),
        "{report}"
    );
}
