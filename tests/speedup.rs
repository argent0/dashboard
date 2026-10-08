// Integration tests for the `test-speedup` task kind and for holding work
// while `main` is itself over its `[budget]` (spec §15.1, §20): a killed or
// over-total `main` must not strike unrelated work nor block the director,
// and only a test-speedup, judged by a ratchet against main's recorded
// timings, may fix it.
//
// The fixture suite has a `slow` and an `other` check whose `cmd`s a test
// chooses; `tests/inventory.txt` is the test inventory `list_cmd` prints.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::events::{self, EventKind};
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{self, NullSessions, Player};
use dashboard::records::{self, Transition};
use dashboard::state::{self, Status};
use std::path::Path;
use tempfile::TempDir;

fn edit_roadmap(repo: &DirectorRepo, from: &str, to: &str) {
    checkout_branch(repo.path(), "main");
    let git = repo.git();
    git.run(&["fetch", "-q", "hub"]).unwrap();
    git.run(&["merge", "-q", "--ff-only", "hub/main"]).unwrap();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    assert!(roadmap.contains(from), "{roadmap}");
    write_file(repo.path(), "roadmap.toml", &roadmap.replace(from, to));
    commit(repo.path(), "docs: change the budget");
}

fn suite_repo(budget: &str, slow: &str, other: &str) -> DirectorRepo {
    let repo = director_repo();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!("{roadmap}\n[budget]\n{budget}\n"),
    );
    write_file(
        repo.path(),
        "tests.toml",
        &format!(
            "test_paths = [\"tests/\"]\nlist_cmd = \"cat tests/inventory.txt\"\n\n[[check]]\nname = \"slow\"\ncmd = {slow:?}\n\n[[check]]\nname = \"other\"\ncmd = {other:?}\n"
        ),
    );
    write_file(repo.path(), "tests/inventory.txt", "one\ntwo\nthree\n");
    commit(repo.path(), "budget suite");
    repo
}

fn event_count(home: &Home, kind: EventKind) -> usize {
    events::read_all(home)
        .into_iter()
        .filter(|e| e.kind == kind)
        .count()
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
/// the matching `tests.toml` entry. Ticks (which also launches the worker,
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
            "title = \"{slug}\"\nphase = \"p1\"\n\n[check]\nname = \"{slug}-check\"\ncmd = \"test -f feature.txt\"\n"
        ),
    );
    write_file(
        repo.path(),
        &format!("tests.d/{slug}.toml"),
        &format!("[[check]]\nname = \"{slug}-check\"\ncmd = \"test -f feature.txt\"\n"),
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
        &format!("title = \"{slug}\"\nphase = \"p1\"\n"),
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


const SPEED_TOML: &str =
    "title = \"speed\"\nphase = \"p1\"\nkind = \"test-speedup\"\n\n[[slow]]\ncheck = \"slow\"\n";

/// Plans and posts a test-speedup `slug` naming `slow_check`; returns the
/// post tick's report.
fn post_speedup(
    repo: &DirectorRepo,
    player: &mut Player<NullSessions>,
    slug: &str,
    slow_check: &str,
) -> player::TickReport {
    let toml = SPEED_TOML
        .replace("title = \"speed\"", &format!("title = \"{slug}\""))
        .replace("check = \"slow\"", &format!("check = \"{slow_check}\""));
    plan(repo, player, slug, &toml);
    write_file(repo.path(), &format!("tasks/{slug}/prompt.md"), "speed up\n");
    commit(repo.path(), &format!("post: {slug}"));
    player.tick().expect("post tick")
}

struct Ctx {
    repo: DirectorRepo,
    _home_dir: TempDir,
    home: Home,
    hub: Git,
    player: Player<NullSessions>,
}

/// A hub over `suite_repo(..)` with test-speedup `speed` posted and launched.
fn speedup_started(budget: &str, slow: &str, other: &str) -> Ctx {
    let repo = suite_repo(budget, slow, other);
    let (home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    let report = post_speedup(&repo, &mut player, "speed", "slow");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.launched, vec!["speed".to_string()], "{report}");
    Ctx {
        repo,
        _home_dir: home_dir,
        home,
        hub,
        player,
    }
}

fn worker_edit(ctx: &Ctx, files: &[(&str, &str)]) {
    commit_multi_in_clone(&ctx.repo, "speed", files, "finish");
}

fn tests_toml(slow: &str, other: &str, extra: &str) -> String {
    format!(
        "test_paths = [\"tests/\"]\nlist_cmd = \"cat tests/inventory.txt\"\n\n[[check]]\nname = \"slow\"\ncmd = {slow:?}\n\n[[check]]\nname = \"other\"\ncmd = {other:?}\n{extra}"
    )
}

#[test]
fn main_over_budget_is_reported_once_and_does_not_block_the_director() {
    let repo = suite_repo("check_timeout = 1", "sleep 3", "true");
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    let report = player.tick().unwrap();
    assert_eq!(report.main_over_budget.len(), 1, "{report}");
    assert!(report.main_over_budget[0].contains("slow"), "{report}");
    assert!(player.tick().unwrap().main_over_budget.is_empty());
    assert_eq!(event_count(&home, EventKind::MainOverBudget), 1);
    let hub_state = state::derive(&hub, &|_| false).unwrap();
    assert!(
        hub_state.main_over_budget[0].starts_with("slow:"),
        "{:?}",
        hub_state.main_over_budget
    );

    // A director commit still imports, though the suite fails on it (it is
    // judged under the old policy, so even raising the limit goes through).
    edit_roadmap(&repo, "check_timeout = 1", "check_timeout = 1\nsuite_warn = 0");
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert!(!report.imported.is_empty(), "{report}");
}

#[test]
fn an_unrelated_finish_is_held_and_released_once_main_is_back_under_budget() {
    let repo = suite_repo("check_timeout = 1", "sleep 3", "true");
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "u");
    commit_multi_in_clone(&repo, "u", &[("feature.txt", "x\n")], "finish");

    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.held.len(), 1, "{report}");
    assert!(report.imported.is_empty(), "{report}");
    assert!(records::held_commit(&hub, "u").is_some());
    assert_ne!(task_status(&hub, "u"), Status::Tested);
    assert_eq!(event_count(&home, EventKind::Held), 1);

    // The director raises the limit: the policy applies from the next tick.
    edit_roadmap(&repo, "check_timeout = 1", "check_timeout = 30");
    let mut released = false;
    for _ in 0..3 {
        let report = player.tick().unwrap();
        assert!(report.rejected.is_empty(), "{report}");
        released |= report.held.iter().any(|(_, m)| m.starts_with("released"));
    }
    assert!(released, "the held finish should be released");
    assert!(records::held_commit(&hub, "u").is_none());
}

#[test]
fn a_total_over_suite_fail_holds_a_finish_unless_a_check_got_slower() {
    // `slow` takes 3s on main, 5s once `slow.flag` exists.
    let repo = suite_repo(
        "suite_fail = 2",
        "if test -f slow.flag; then sleep 5; else sleep 3; fi",
        "true",
    );
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "t");
    plan_and_post(&repo, &mut player, "u");
    commit_multi_in_clone(&repo, "t", &[("feature.txt", "x\n")], "finish");
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.held.len(), 1, "{report}");
    assert!(records::held_commit(&hub, "t").is_some());

    // Main's excess is not a licence: `slow` got slower than on main.
    commit_multi_in_clone(
        &repo,
        "u",
        &[("feature.txt", "x\n"), ("slow.flag", "1\n")],
        "finish",
    );
    let report = player.tick().unwrap();
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Budget);
    assert!(records::held_commit(&hub, "u").is_none());
}

#[test]
fn a_speedup_post_imports_while_main_is_over_and_unproven_evidence_retires_it() {
    let repo = suite_repo("check_timeout = 1", "sleep 3", "true");
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    let report = post_speedup(&repo, &mut player, "speed", "slow");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.launched, vec!["speed".to_string()], "{report}");
    assert_eq!(task_status(&hub, "speed"), Status::Posted);
    let pinned = records::pinned_files(&hub, "speed").unwrap();
    assert!(
        !pinned.contains_key("tests.toml") && !pinned.contains_key("tests/inventory.txt"),
        "the test side is unpinned: {pinned:?}"
    );
    let clone = paths::worker_clone(&repo.director_path(), "speed");
    let git_dir = Git::new(&clone).run(&["rev-parse", "--absolute-git-dir"]).unwrap();
    let prompt = std::fs::read_to_string(Path::new(&git_dir).join("dashboard-prompt.md")).unwrap();
    assert!(prompt.contains("test-speedup") && prompt.contains("- slow"), "{prompt}");

    // `other` is not slow on main.
    let report = post_speedup(&repo, &mut player, "nope", "other");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Evidence);
    assert_eq!(task_status(&hub, "nope"), Status::Retired);
    assert!(
        records::post_mortem(&hub, "nope").unwrap().contains("not slow on main"),
        "{:?}",
        records::post_mortem(&hub, "nope")
    );
}

#[test]
fn a_speedup_posted_while_main_is_within_budget_is_retired() {
    let repo = suite_repo("check_timeout = 60", "sleep 1", "true");
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    let report = post_speedup(&repo, &mut player, "speed", "slow");
    assert_eq!(report.rejected[0].reason, Reason::Evidence, "{report}");
    assert_eq!(task_status(&hub, "speed"), Status::Retired);
}

fn assert_struck_for_scope(ctx: &mut Ctx, what: &str) {
    let report = ctx.player.tick().unwrap();
    assert_eq!(report.rejected.len(), 1, "{what}: {report}");
    assert_eq!(report.rejected[0].reason, Reason::Pins, "{what}: {report}");
    assert!(
        report.rejected[0].message.contains("test-speedup"),
        "{what}: {}",
        report.rejected[0].message
    );
    assert_ne!(task_status(&ctx.hub, "speed"), Status::Tested);
}

#[test]
fn a_speedup_touching_non_test_code_is_struck() {
    let mut ctx = speedup_started("check_timeout = 1", "sleep 3", "true");
    worker_edit(
        &ctx,
        &[("src/lib.rs", "// faster\n"), ("tests.toml", &tests_toml("true", "true", ""))],
    );
    assert_struck_for_scope(&mut ctx, "code");
}

#[test]
fn a_speedup_changing_runs_or_adding_a_check_is_struck() {
    let mut ctx = speedup_started("check_timeout = 1", "sleep 3", "true");
    worker_edit(&ctx, &[("tests.toml", &tests_toml("true", "true", "runs = 3\nmin_pass = 1\n"))]);
    assert_struck_for_scope(&mut ctx, "runs");

    let mut ctx = speedup_started("check_timeout = 1", "sleep 3", "true");
    worker_edit(
        &ctx,
        &[(
            "tests.toml",
            &tests_toml("true", "true", "\n[[check]]\nname = \"extra\"\ncmd = \"true\"\n"),
        )],
    );
    assert_struck_for_scope(&mut ctx, "added check");
}

#[test]
fn a_speedup_touching_the_roadmap_budget_is_struck() {
    let mut ctx = speedup_started("check_timeout = 1", "sleep 3", "true");
    let roadmap = std::fs::read_to_string(
        paths::worker_clone(&ctx.repo.director_path(), "speed").join("roadmap.toml"),
    )
    .unwrap();
    worker_edit(
        &ctx,
        &[
            ("roadmap.toml", &roadmap.replace("check_timeout = 1", "check_timeout = 600")),
            ("tests.toml", &tests_toml("true", "true", "")),
        ],
    );
    assert_struck_for_scope(&mut ctx, "roadmap");
}

#[test]
fn a_faster_check_finishes_with_the_removals_reported_and_accept_lands_it() {
    let mut ctx = speedup_started("check_timeout = 1", "sleep 3", "true");
    worker_edit(
        &ctx,
        &[
            ("tests.toml", &tests_toml("true", "true", "")),
            ("tests/inventory.txt", "one\nthree\n"),
        ],
    );
    let report = ctx.player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.imported[0].2, Transition::Finish, "{report}");
    assert_eq!(report.speedups.len(), 1, "{report}");
    let note = &report.speedups[0].1;
    assert!(note.contains("removed 1 tests: two"), "{note}");
    assert!(note.contains("slow: `sleep 3` -> `true`"), "{note}");
    assert_eq!(task_status(&ctx.hub, "speed"), Status::Tested);
    assert_eq!(event_count(&ctx.home, EventKind::SpeedupFinish), 1);
    let tip = ctx.hub.rev_parse("refs/heads/task/speed").unwrap();
    assert_eq!(records::speedup_notes(&ctx.hub, &tip).as_deref(), Some(note.as_str()));

    director_pull_task_branch(ctx.repo.path(), "speed");
    commit_empty(ctx.repo.path(), "accept: the removed test is redundant");
    let report = ctx.player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(task_status(&ctx.hub, "speed"), Status::Accepted);
}

#[test]
fn the_whole_flow_main_over_budget_to_held_work_released() {
    let mut ctx = speedup_started("check_timeout = 1", "sleep 3", "true");
    assert_eq!(event_count(&ctx.home, EventKind::MainOverBudget), 1);
    // An unrelated task's finish is held meanwhile.
    plan_and_post(&ctx.repo, &mut ctx.player, "u");
    commit_multi_in_clone(&ctx.repo, "u", &[("feature.txt", "x\n")], "finish");
    let report = ctx.player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.held.len(), 1, "{report}");

    worker_edit(&ctx, &[("tests.toml", &tests_toml("true", "true", ""))]);
    let report = ctx.player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(task_status(&ctx.hub, "speed"), Status::Tested);
    assert!(
        report.speedups[0].1.contains("slow: `sleep 3` -> `true`") && !report.speedups[0].1.contains("removed"),
        "{report}"
    );

    director_pull_task_branch(ctx.repo.path(), "speed");
    commit_empty(ctx.repo.path(), "accept: faster");
    let report = ctx.player.tick().unwrap();
    assert_eq!(task_status(&ctx.hub, "speed"), Status::Accepted, "{report}");

    let mut released = false;
    for _ in 0..3 {
        let report = ctx.player.tick().unwrap();
        released |= report.held.iter().any(|(s, m)| s == "u" && m.starts_with("released"));
    }
    assert!(released, "u should be released once main is fast again");
    assert!(records::held_commit(&ctx.hub, "u").is_none());
    assert_eq!(event_count(&ctx.home, EventKind::MainOverBudget), 1);
}

// ---- the ratchet, while main's total is over suite_fail with nothing killed ----

fn over_total() -> Ctx {
    speedup_started("suite_fail = 2", "sleep 4", "sleep 2")
}

#[test]
fn a_faster_check_that_is_still_over_budget_passes_the_ratchet() {
    let mut ctx = over_total();
    worker_edit(&ctx, &[("tests.toml", &tests_toml("sleep 3", "sleep 2", ""))]);
    let report = ctx.player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(task_status(&ctx.hub, "speed"), Status::Tested);
}

#[test]
fn a_check_that_is_not_faster_fails_the_ratchet() {
    let mut ctx = over_total();
    worker_edit(&ctx, &[("tests.toml", &tests_toml("sleep 5", "sleep 2", ""))]);
    let report = ctx.player.tick().unwrap();
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Budget);
    assert!(report.rejected[0].message.contains("not faster"), "{report}");
}

#[test]
fn another_check_getting_slower_fails_the_ratchet() {
    let mut ctx = over_total();
    worker_edit(&ctx, &[("tests.toml", &tests_toml("sleep 3", "sleep 4", ""))]);
    let report = ctx.player.tick().unwrap();
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Budget);
    assert!(report.rejected[0].message.contains("got slower"), "{report}");
}
