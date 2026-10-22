// Integration tests for the step 6 worker side of src/player.rs (spec.md
// §7, §8.3, §9's launch row): launching a worker clone + tmux session
// right after a post, importing and validating its finish/block claims, and
// relaying worker rejections and send-backs into its session. Every test
// builds a real director repo and a real hub via `dashboard::init::init`,
// drives `Player::tick` with `NullSessions`, and simulates the worker by
// committing directly in the clone the player itself created
// (`paths::worker_clone`) — exactly as spec.md's "provenance by fetch
// source" model expects the player to pull from that clone, no push needed.
//
// The one exception is `tmux_smoke_test`, which drives the real
// `TmuxSessions` backend against an actual `tmux` binary (skipped if `tmux`
// isn't on `PATH`).

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{self, NullSessions, Player, Sessions};
use dashboard::records::{self, Transition};
use dashboard::state::{self, Status};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

// --------------------------------------------------------------------------
// Fixture helpers
// --------------------------------------------------------------------------

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
/// `test -f src/feature.txt` (fails until the "worker" creates that file) and
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
            "title = \"{slug}\"\nphase = \"p1\"\nmodules = [\"app\"]\n"
        ),
    );
    write_file(
        repo.path(),
        &*tests_file(&slug),
        &*task_suite_lean(&format!("[[check]]\nname = \"{slug}-check\"\ncmd = \"test -f src/feature.txt\"\n")),
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

fn commit_in_clone(repo: &DirectorRepo, slug: &str, rel: &str, content: &str, msg: &str) -> String {
    commit_multi_in_clone(repo, slug, &[(rel, content)], msg)
}

fn block_in_clone(repo: &DirectorRepo, slug: &str, msg: &str) -> String {
    let dir = paths::worker_clone(&repo.director_path(), slug);
    ensure_identity(&dir);
    commit_empty(&dir, msg)
}

// --------------------------------------------------------------------------
// Launch
// --------------------------------------------------------------------------

#[test]
fn post_launches_a_worker_clone_with_pins_and_a_prompt_file() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "foo",
        "title = \"Foo\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    plan_and_post(&repo, &mut player, "foo");

    assert!(
        records::has_pins(&hub, "foo"),
        "pins should be recorded at launch"
    );

    let clone_dir = paths::worker_clone(&repo.director_path(), "foo");
    let clone_git = Git::new(&clone_dir);
    let branch = clone_git
        .run(&["rev-parse", "--abbrev-ref", "HEAD"])
        .expect("clone should be on a branch");
    assert_eq!(branch, "task/foo");
    let clone_tip = clone_git
        .rev_parse("HEAD")
        .expect("clone should have a tip");
    let hub_tip = hub
        .rev_parse("refs/heads/task/foo")
        .expect("hub should have task/foo");
    assert_eq!(
        clone_tip, hub_tip,
        "the clone should be checked out exactly at the hub's post tip"
    );

    // The clone is a worktree, so its `.git` is a *file* pointing at its
    // real, private admin dir under the shared workspace's `.git/worktrees/`
    // — resolve that the same way the player does, rather than assuming the
    // plain-clone shape.
    let git_dir_raw = clone_git
        .run(&["rev-parse", "--git-dir"])
        .expect("git-dir should resolve");
    let git_dir = PathBuf::from(&git_dir_raw);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        clone_dir.join(git_dir)
    };
    let prompt_file = git_dir.join("dashboard-prompt.md");
    let prompt_content = std::fs::read_to_string(&prompt_file).expect("prompt file should exist");
    assert!(
        prompt_content.contains("worker for task `foo`"),
        "{prompt_content}"
    );
    assert!(
        prompt_content.contains("do the thing"),
        "prompt.md's body should be appended: {prompt_content}"
    );
    assert!(
        prompt_content.contains("tasks/foo/"),
        "the preamble should list the pinned tasks/foo/ paths: {prompt_content}"
    );
    assert!(
        prompt_content.contains("dashboard build") && prompt_content.contains("git show origin/task/foo:"),
        "an ordinary task's prompt describes its sparse tree: {prompt_content}"
    );

    assert_eq!(player.sessions().launched.len(), 1);
    let call = &player.sessions().launched[0];
    assert_eq!(call.slug, "foo");
    assert_eq!(call.clone_dir, clone_dir);
    assert_eq!(call.prompt_file, prompt_file);
    assert_eq!(
        call.agent_cmd, "claude --dangerously-skip-permissions",
        "should come from roadmap.toml's default at hub main"
    );
}

#[test]
fn no_relaunch_on_the_next_tick() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "foo");

    let report = player.tick().expect("tick should succeed");
    assert!(report.launched.is_empty(), "{report}");
    assert_eq!(
        player.sessions().launched.len(),
        1,
        "still just the one launch from post"
    );
}

#[test]
fn launch_skipped_with_an_error_when_the_clone_dir_already_exists() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "leftover",
        "title = \"Leftover\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );

    // A leftover directory from some previous, unrelated run — the player
    // must never touch it (spec §5).
    std::fs::create_dir_all(paths::worker_clone(&repo.director_path(), "leftover"))
        .expect("create leftover dir");
    std::fs::write(
        paths::worker_clone(&repo.director_path(), "leftover").join("marker.txt"),
        "leftover\n",
    )
    .expect("write marker");

    checkout_branch(repo.path(), "task/leftover");
    write_file(repo.path(), "tasks/leftover/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/leftover/task.toml",
        "title = \"Leftover\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(
        repo.path(),
        &*tests_file("leftover"),
        &*task_suite_lean("[[check]]\nname = \"leftover-check\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    commit(repo.path(), "post: leftover");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(
        report.imported.len(),
        1,
        "the post itself should still land: {report}"
    );
    assert!(report.launched.is_empty(), "{report}");
    assert_eq!(report.errors.len(), 1, "{report}");
    assert!(
        report.errors[0].contains("already exists"),
        "{}",
        report.errors[0]
    );
    assert!(
        player.sessions().launched.is_empty(),
        "no session should have been started"
    );
    assert!(
        paths::worker_clone(&repo.director_path(), "leftover")
            .join("marker.txt")
            .exists(),
        "the leftover clone must be untouched"
    );
    assert_eq!(task_status(&hub, "leftover"), Status::Posted);
}

// --------------------------------------------------------------------------
// Finish
// --------------------------------------------------------------------------

#[test]
fn finish_happy_path_is_tested_with_a_note_sent() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "x");
    commit_in_clone(&repo, "x", "src/feature.txt", "done\n", "finish work");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Finish);
    assert_eq!(task_status(&hub, "x"), Status::Tested);

    let sent = &player.sessions().sent;
    assert!(
        sent.iter()
            .any(|(slug, msg)| slug == "x" && msg.contains("awaiting director review")),
        "{sent:?}"
    );
}

#[test]
fn messages_come_from_prompts_kdl_on_main() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    // The director rewords a template on main; an invalid edit is rejected
    // and leaves the old file in force.
    let starter = dashboard::prompts::STARTER;
    write_file(
        repo.path(),
        "prompts.kdl",
        &starter.replace("{answer}", "{answr}"),
    );
    commit(repo.path(), "prompts: typo");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(
        report.rejected[0]
            .message
            .contains("unknown placeholder {answr}"),
        "{report}"
    );

    repo.git()
        .run(&["reset", "-q", "--hard", "HEAD~1"])
        .unwrap();
    let old = "finish accepted — tests passed, awaiting director review; don't commit further";
    write_file(
        repo.path(),
        "prompts.kdl",
        &starter.replace(old, "all green, sit tight"),
    );
    commit(repo.path(), "prompts: shorter finish note");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");

    plan_and_post(&repo, &mut player, "x");
    commit_in_clone(&repo, "x", "src/feature.txt", "done\n", "finish work");
    player.tick().expect("tick should succeed");
    let sent = &player.sessions().sent;
    assert!(
        sent.iter()
            .any(|(slug, msg)| slug == "x" && msg == "all green, sit tight"),
        "{sent:?}"
    );
}

/// Once a task is accepted, its worktree — and the local branch that
/// checked it out in the shared worker workspace — should be gone: the
/// hub's own `task/<slug>` ref is the permanent record (spec §8.1), so
/// leaving every finished task's worktree registered forever in the one
/// shared workspace repo would grow its bookkeeping without bound.
#[test]
fn accept_removes_the_worker_worktree() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "x");
    let clone_dir = paths::worker_clone(&repo.director_path(), "x");
    assert!(
        clone_dir.exists(),
        "worktree should exist right after launch"
    );

    commit_in_clone(&repo, "x", "src/feature.txt", "done\n", "finish work");
    player.tick().expect("finish tick should succeed");
    assert_eq!(task_status(&hub, "x"), Status::Tested);

    director_pull_task_branch(repo.path(), "x");
    commit_empty(repo.path(), "accept: looks good");
    let report = player.tick().expect("accept tick should succeed");
    assert_eq!(report.imported[0].2, Transition::Accept, "{report}");

    assert!(
        !clone_dir.exists(),
        "worktree should be removed once accepted"
    );
    let workspace = Git::new(paths::worker_workspace(&repo.director_path()));
    assert!(
        !workspace.ref_exists("refs/heads/task/x"),
        "the workspace's local branch should be dropped too"
    );
}

/// Same cleanup, via retire instead of accept — an escalated task's
/// worktree shouldn't linger either.
#[test]
fn retire_of_an_escalated_task_removes_the_worker_worktree() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "blk");
    let clone_dir = paths::worker_clone(&repo.director_path(), "blk");
    assert!(
        clone_dir.exists(),
        "worktree should exist right after launch"
    );

    block_in_clone(
        &repo,
        "blk",
        "block: cannot be done as specified\n\nThe API this needs doesn't exist.",
    );
    player.tick().expect("block tick should succeed");
    assert_eq!(task_status(&hub, "blk"), Status::Escalated);

    director_pull_task_branch(repo.path(), "blk");
    commit_empty(repo.path(), "retire: dead end, replanning");
    let report = player.tick().expect("retire tick should succeed");
    assert_eq!(report.imported[0].2, Transition::Retire, "{report}");

    assert!(
        !clone_dir.exists(),
        "worktree should be removed once retired"
    );
    let workspace = Git::new(paths::worker_workspace(&repo.director_path()));
    assert!(
        !workspace.ref_exists("refs/heads/task/blk"),
        "the workspace's local branch should be dropped too"
    );
}

#[test]
fn finish_modifying_a_pinned_file_is_rejected_with_pins_and_relayed() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "y");
    commit_in_clone(
        &repo,
        "y",
        "tasks/y/task.toml",
        "title = \"Y (tampered)\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "tamper with the pinned task.toml",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Pins);
    assert_eq!(report.rejected[0].scope, "y");

    let sent = &player.sessions().sent;
    assert!(
        sent.iter()
            .any(|(slug, msg)| slug == "y" && msg.contains("fix it with a new commit on top")),
        "{sent:?}"
    );
    assert_ne!(task_status(&hub, "y"), Status::Tested);
}

#[test]
fn finish_adding_another_suite_file_is_a_scope_strike() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "frag");
    commit_in_clone(
        &repo,
        "frag",
        &*tests_file("extra"),
        &*task_suite_lean("[[check]]\nname = \"extra\"\ncmd = \"true\"\n"),
        "add a suite check",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Scope);
    assert!(
        report.rejected[0].message.contains("architecture/Tests/extra.lean"),
        "{}",
        report.rejected[0].message
    );
    assert_ne!(task_status(&hub, "frag"), Status::Tested);
}

#[test]
fn finish_with_a_failing_suite_is_rejected_then_a_fix_on_top_passes() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "z");
    commit_in_clone(
        &repo,
        "z",
        "src/unrelated.txt",
        "still working\n",
        "not done yet",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Suite);
    let sent = &player.sessions().sent;
    assert!(
        sent.iter()
            .any(|(slug, msg)| slug == "z" && msg.contains("fix it with a new commit on top")),
        "{sent:?}"
    );
    assert_ne!(task_status(&hub, "z"), Status::Tested);

    // The cumulative diff from the hub tip includes both commits: adding
    // src/feature.txt now makes it pass even though the first commit alone did
    // not (spec's "each is a finish claim, validated cumulatively").
    commit_in_clone(&repo, "z", "src/feature.txt", "done\n", "actually finish it");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Finish);
    assert_eq!(task_status(&hub, "z"), Status::Tested);
}

#[test]
fn worker_merge_commit_is_rejected_with_history() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "m");

    let clone_dir = paths::worker_clone(&repo.director_path(), "m");
    let clone_git = Git::new(&clone_dir);
    let head = clone_git
        .rev_parse("HEAD")
        .expect("clone should have a tip");
    let origin_main = clone_git
        .rev_parse("origin/main")
        .expect("clone should track origin/main");
    let tree = clone_git
        .run(&["rev-parse", &format!("{head}^{{tree}}")])
        .expect("rev-parse tree");
    let merge_sha = clone_git
        .run(&[
            "commit-tree",
            &tree,
            "-p",
            &head,
            "-p",
            &origin_main,
            "-m",
            "merge main (not allowed)",
        ])
        .expect("commit-tree");
    clone_git
        .run(&["update-ref", "refs/heads/task/m", &merge_sha])
        .expect("move local branch to the merge commit");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::History);
    assert_eq!(report.rejected[0].scope, "m");
    assert_ne!(task_status(&hub, "m"), Status::Tested);
}

/// `d` finished, then sent back (r1): the player syncs the worker's tree
/// to the send-back just before pasting the review, unless `scratch` (an
/// untracked file) leaves the tree dirty.
fn finish_and_send_back(repo: &DirectorRepo, player: &mut Player<NullSessions>, hub: &Git, scratch: bool) -> String {
    plan_and_post(repo, player, "d");
    commit_in_clone(repo, "d", "src/feature.txt", "done\n", "finish work");
    player.tick().expect("finish tick should succeed");
    assert_eq!(task_status(hub, "d"), Status::Tested);
    let finish = hub.rev_parse("refs/heads/task/d").unwrap();
    let dir = paths::worker_clone(&repo.director_path(), "d");
    if scratch {
        write_file(&dir, "src/scratch.txt", "mine\n");
    }

    director_pull_task_branch(repo.path(), "d");
    write_file(
        repo.path(),
        &*tests_file("d"),
        &*task_suite_lean("[[check]]\nname = \"d-check\"\ncmd = \"test -f src/feature.txt && test -f src/feature2.txt\"\n"),
    );
    write_file(
        repo.path(),
        "tasks/d/reviews/r1.md",
        "add src/feature2.txt too\n",
    );
    commit(repo.path(), "send-back: d r1");
    let report = player.tick().expect("send-back tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::SendBack);
    assert_eq!(task_status(hub, "d"), Status::InRevision);
    finish
}

fn last_sent(player: &Player<NullSessions>, slug: &str) -> String {
    let sent = &player.sessions().sent;
    sent.iter()
        .rev()
        .find(|(s, _)| s == slug)
        .map(|(_, m)| m.clone())
        .unwrap_or_else(|| panic!("nothing sent to {slug}: {sent:?}"))
}

#[test]
fn the_player_syncs_a_clean_worker_tree_before_pasting_a_review() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    finish_and_send_back(&repo, &mut player, &hub, false);

    let tip = hub.rev_parse("refs/heads/task/d").unwrap();
    let worker = Git::new(paths::worker_clone(&repo.director_path(), "d"));
    assert_eq!(worker.rev_parse("HEAD"), Some(tip.clone()));
    assert!(worker.dir.join("tasks/d/reviews/r1.md").exists());
    let review = last_sent(&player, "d");
    assert!(review.starts_with("Review round 1"), "{review}");
    assert!(
        review.contains(&format!("Your tree is already at {}, the tip of task/d", &tip[..12])),
        "{review}"
    );
    assert!(!review.contains("git pull"), "{review}");
}

#[test]
fn a_dirty_worker_tree_is_left_as_it_is_and_told_to_pull() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    let finish = finish_and_send_back(&repo, &mut player, &hub, true);

    let worker = Git::new(paths::worker_clone(&repo.director_path(), "d"));
    assert_eq!(worker.rev_parse("HEAD"), Some(finish), "untouched");
    assert_eq!(
        std::fs::read_to_string(worker.dir.join("src/scratch.txt")).unwrap(),
        "mine\n"
    );
    let review = last_sent(&player, "d");
    assert!(
        review.contains("Fetch it: git pull --ff-only origin task/d."),
        "{review}"
    );
}

#[test]
fn worker_commit_built_on_a_stale_pre_send_back_tip_is_diverged_history() {
    stale_commit_after_send_back(false);
}

#[test]
fn a_dirty_tree_with_a_diverged_history_is_left_to_the_worker_to_reset() {
    stale_commit_after_send_back(true);
}

/// The worker commits on its pre-send-back tip (it moved its tree back
/// there): diverged history. The player resets a clean tree to the hub's
/// tip itself; a `dirty` one gets the reset command.
fn stale_commit_after_send_back(dirty: bool) {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    let finish = finish_and_send_back(&repo, &mut player, &hub, false);

    let dir = paths::worker_clone(&repo.director_path(), "d");
    let worker = Git::new(&dir);
    worker.run(&["reset", "-q", "--hard", &finish]).unwrap();
    let stale = commit_in_clone(
        &repo,
        "d",
        "src/feature2.txt",
        "done too\n",
        "add feature2 without pulling first",
    );
    if dirty {
        write_file(&dir, "src/scratch.txt", "mine\n");
    }
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::History);
    let msg = last_sent(&player, "d");
    let tip = hub.rev_parse("refs/heads/task/d").unwrap();
    if dirty {
        assert!(
            msg.contains("git fetch origin && git reset --hard origin/task/d"),
            "{msg}"
        );
        assert_eq!(worker.rev_parse("HEAD"), Some(stale));
    } else {
        assert!(
            msg.contains(&format!("your tree is reset to task/d at {}", &tip[..12])),
            "{msg}"
        );
        assert_eq!(worker.rev_parse("HEAD"), Some(tip));
    }
    assert_eq!(task_status(&hub, "d"), Status::InRevision);
}

// --------------------------------------------------------------------------
// Block
// --------------------------------------------------------------------------

#[test]
fn block_marker_escalates_with_the_cause() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "blk");
    block_in_clone(
        &repo,
        "blk",
        "block: cannot be done as specified\n\nThe API this needs doesn't exist.",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Block);
    assert_eq!(task_status(&hub, "blk"), Status::Escalated);

    let hub_state = state::derive(&hub, &|_| false).expect("derive should succeed");
    let task = hub_state.tasks.iter().find(|t| t.slug == "blk").unwrap();
    assert!(
        task.escalation
            .as_deref()
            .unwrap_or("")
            .contains("doesn't exist"),
        "{:?}",
        task.escalation
    );
}

// --------------------------------------------------------------------------
// Strikes -> escalation
// --------------------------------------------------------------------------

#[test]
fn two_suite_strikes_escalate_and_the_second_rejection_is_not_relayed() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "s");

    commit_in_clone(
        &repo,
        "s",
        "src/attempt1.txt",
        "no feature yet\n",
        "attempt 1 (suite still fails)",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Suite);
    assert_ne!(task_status(&hub, "s"), Status::Escalated);
    let relayed_before = player
        .sessions()
        .sent
        .iter()
        .filter(|(slug, _)| slug == "s")
        .count();
    assert_eq!(relayed_before, 1, "the first strike should be relayed");

    commit_in_clone(
        &repo,
        "s",
        "src/attempt2.txt",
        "still no feature\n",
        "attempt 2 (suite still fails)",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Suite);
    assert_eq!(task_status(&hub, "s"), Status::Escalated);
    let relayed_after = player
        .sessions()
        .sent
        .iter()
        .filter(|(slug, _)| slug == "s")
        .count();
    assert_eq!(
        relayed_after, 1,
        "the escalating (second) rejection must not be relayed"
    );
}

// --------------------------------------------------------------------------
// Full loop: post -> finish -> send-back -> fix -> finish -> accept
// --------------------------------------------------------------------------

#[test]
fn full_loop_post_finish_send_back_fix_finish_accept() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "full");
    commit_in_clone(&repo, "full", "src/feature.txt", "done\n", "finish work");
    let report = player.tick().expect("finish tick should succeed");
    assert_eq!(report.imported[0].2, Transition::Finish);
    assert_eq!(task_status(&hub, "full"), Status::Tested);

    director_pull_task_branch(repo.path(), "full");
    write_file(
        repo.path(),
        &*tests_file("full"),
        &*task_suite_lean("[[check]]\nname = \"full-check\"\ncmd = \"test -f src/feature.txt && test -f src/feature2.txt\"\n"),
    );
    write_file(
        repo.path(),
        "tasks/full/reviews/r1.md",
        "please also add src/feature2.txt\n",
    );
    commit(repo.path(), "send-back: full r1");
    let report = player.tick().expect("send-back tick should succeed");
    assert_eq!(report.imported[0].2, Transition::SendBack);
    assert_eq!(task_status(&hub, "full"), Status::InRevision);
    let candidate = hub.rev_parse("refs/heads/task/full").unwrap();
    assert_eq!(
        hub.rev_parse("refs/dashboard/pins/full").as_deref(),
        Some(candidate.as_str()),
        "send-back should re-pin"
    );

    // The worker pulls the send-back commit from origin (the hub) before
    // continuing to work.
    let clone_dir = paths::worker_clone(&repo.director_path(), "full");
    let clone_git = Git::new(&clone_dir);
    clone_git
        .run(&["fetch", "-q", "origin", "task/full"])
        .expect("fetch origin");
    clone_git
        .run(&["merge", "-q", "--ff-only", "FETCH_HEAD"])
        .expect("fast-forward onto the send-back commit");

    commit_in_clone(
        &repo,
        "full",
        "src/feature2.txt",
        "done too\n",
        "address the review",
    );
    let report = player.tick().expect("second finish tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Finish);
    assert_eq!(task_status(&hub, "full"), Status::Tested);

    director_pull_task_branch(repo.path(), "full");
    commit_empty(repo.path(), "accept: looks good now");
    let report = player.tick().expect("accept tick should succeed");
    assert_eq!(report.imported.len(), 2, "{report}"); // accept marker, then merge_accept
    assert_eq!(report.imported[0].2, Transition::Accept);
    assert_eq!(report.imported[1].2, Transition::MergeAccept);
    assert_eq!(task_status(&hub, "full"), Status::Accepted);
    assert_eq!(player.sessions().killed, vec!["full".to_string()]);
}

// --------------------------------------------------------------------------
// Relaunch
// --------------------------------------------------------------------------

#[test]
fn relaunch_refuses_when_the_session_is_already_alive() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "r");
    assert!(player.sessions().alive("r"));

    let err = player
        .relaunch("r")
        .expect_err("relaunch should refuse a live session");
    assert!(err.contains("already alive"), "{err}");
}

/// `slug`'s worker relaunches since its last progress, as `status` sees them.
fn relaunches(hub: &Git, slug: &str) -> u32 {
    state::derive(hub, &|_| false)
        .expect("derive")
        .tasks
        .into_iter()
        .find(|t| t.slug == slug)
        .expect("task")
        .relaunches
}

#[test]
fn relaunches_past_max_relaunches_are_refused_and_escalate() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "r");
    assert_eq!(
        relaunches(&hub, "r"),
        0,
        "the launch itself isn't a relaunch"
    );

    // `dashboard relaunch` runs in its own process; each one is counted.
    for n in 1..=2 {
        player.sessions_mut().alive.remove("r");
        open_player(&home)
            .relaunch("r")
            .expect("relaunch within the limit");
        assert_eq!(relaunches(&hub, "r"), n);
    }

    // The count is in the hub, not in the player's local state.
    drop(player);
    std::fs::remove_dir_all(home.launches()).expect("drop local launch ids");
    let mut player = open_player(&home);
    player.tick().expect("tick after a restart");
    assert_eq!(relaunches(&hub, "r"), 2);

    let launched = player.sessions().launched.len();
    let err = player
        .relaunch("r")
        .expect_err("a third relaunch is past max_relaunches = 2");
    assert!(err.contains("max_relaunches = 2"), "{err}");
    assert!(err.contains("escalated"), "{err}");
    assert_eq!(player.sessions().launched.len(), launched, "not launched");
    let t = state::derive(&hub, &|_| false)
        .unwrap()
        .tasks
        .into_iter()
        .find(|t| t.slug == "r")
        .unwrap();
    assert_eq!(t.status, Status::Escalated);
    assert!(
        t.escalation
            .as_deref()
            .is_some_and(|c| c.starts_with("relaunch limit (2) reached")),
        "{:?}",
        t.escalation
    );

    // The director hears it as any escalation.
    player.tick().expect("tick");
    let escalated: Vec<_> = dashboard::events::read_all(&home)
        .into_iter()
        .filter(|e| e.kind == dashboard::events::EventKind::Escalated)
        .collect();
    assert_eq!(escalated.len(), 1, "{escalated:?}");
    assert!(
        escalated[0].message.contains("relaunch limit"),
        "{escalated:?}"
    );
    assert!(
        player.relaunch("r").is_err(),
        "escalated: no more relaunches"
    );

    // And it resolves as any escalation: a retire.
    director_pull_task_branch(repo.path(), "r");
    commit_empty(repo.path(), "retire: keeps crashing");
    let report = player.tick().expect("retire tick");
    assert_eq!(report.imported[0].2, Transition::Retire, "{report}");
    assert_eq!(task_status(&hub, "r"), Status::Retired);
}

/// Relaunches `slug`'s worker until `max_relaunches = 2` escalates it.
fn relaunch_until_escalated(home: &Home, player: &mut Player<NullSessions>, slug: &str) {
    player.sessions_mut().alive.remove(slug);
    while open_player(home).relaunch(slug).is_ok() {}
    assert_eq!(task_status(&Git::new(home.hub()), slug), Status::Escalated);
}

fn events_of(home: &Home, kind: dashboard::events::EventKind) -> Vec<dashboard::events::Event> {
    dashboard::events::read_all(home)
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

#[test]
fn reset_clears_a_relaunch_limit_escalation_and_the_cap_applies_again() {
    use dashboard::events::EventKind;
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "r");
    relaunch_until_escalated(&home, &mut player, "r");
    player.tick().expect("tick");
    assert_eq!(events_of(&home, EventKind::Escalated).len(), 1);

    // Refused while the session is alive, and for the task agent's limit.
    player.sessions_mut().alive.insert("r".to_string());
    let err = player.reset_relaunch("r", false).expect_err("alive");
    assert!(err.contains("already alive"), "{err}");
    player.sessions_mut().alive.remove("r");
    let err = player.reset_relaunch("r", true).expect_err("wrong session");
    assert!(err.contains("worker's relaunch limit"), "{err}");
    assert!(err.contains("dashboard relaunch r --reset"), "{err}");
    assert_eq!(task_status(&hub, "r"), Status::Escalated);

    // `dashboard relaunch --reset` runs in its own process.
    let mut cli = open_player(&home);
    cli.reset_relaunch("r", false).expect("reset");
    assert_eq!(cli.sessions().launched.len(), 1, "relaunched");
    assert_eq!(task_status(&hub, "r"), Status::Posted);
    assert_eq!(relaunches(&hub, "r"), 1, "the count starts over");

    // The escalation is kept as evidence, out of the way of `status`.
    assert!(!hub.ref_exists("refs/dashboard/escalated/r"));
    let cleared = hub.rev_parse("refs/dashboard/escalation-cleared/r/1");
    assert!(cleared.is_some());
    let records = records::cleared_escalations(&hub, "r");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].session, "worker");
    assert!(
        records[0]
            .cause
            .starts_with("relaunch limit (2) reached: the worker"),
        "{records:?}"
    );

    // A restarted player tells the director once, and nothing else.
    drop(player);
    let mut player = open_player(&home);
    player.tick().expect("tick after a restart");
    player.tick().expect("another tick");
    let resumed = events_of(&home, EventKind::Resumed);
    assert_eq!(resumed.len(), 1, "{resumed:?}");
    assert!(
        resumed[0]
            .message
            .contains("(was: relaunch limit (2) reached"),
        "{resumed:?}"
    );
    assert!(resumed[0].message.contains("posted again"), "{resumed:?}");
    assert_eq!(events_of(&home, EventKind::Escalated).len(), 1);
    assert_eq!(relaunches(&hub, "r"), 1);

    // The cap applies again: one more relaunch, then the next escalates.
    open_player(&home).relaunch("r").expect("within the limit");
    assert_eq!(relaunches(&hub, "r"), 2);
    let err = open_player(&home).relaunch("r").expect_err("capped again");
    assert!(err.contains("dashboard relaunch r --reset"), "{err}");
    assert_eq!(task_status(&hub, "r"), Status::Escalated);
    player.tick().expect("tick");
    assert_eq!(events_of(&home, EventKind::Escalated).len(), 2);

    // And may be reset again; both clears stay on record.
    open_player(&home)
        .reset_relaunch("r", false)
        .expect("second reset");
    assert_eq!(task_status(&hub, "r"), Status::Posted);
    assert!(hub.ref_exists("refs/dashboard/escalation-cleared/r/2"));
    assert_eq!(records::cleared_escalations(&hub, "r").len(), 2);
}

#[test]
fn reset_refuses_every_other_escalation_and_a_task_not_escalated() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "n");
    player.sessions_mut().alive.remove("n");
    let err = player
        .reset_relaunch("n", false)
        .expect_err("not escalated");
    assert!(err.contains("not escalated"), "{err}");

    // A record-based escalation of another cause (e.g. review rounds).
    let tip = hub.rev_parse("refs/heads/task/n").unwrap();
    records::record_escalation(&hub, "n", &tip, "review rounds exceeded (2)").unwrap();
    let err = player
        .reset_relaunch("n", false)
        .expect_err("review rounds");
    assert!(
        err.contains("escalated for review rounds exceeded (2)")
            && err.contains("only a relaunch-limit escalation can be reset"),
        "{err}"
    );
    assert!(hub.ref_exists("refs/dashboard/escalated/n"), "untouched");

    // Strikes at their limit outrank even a relaunch-limit record.
    plan_and_post(&repo, &mut player, "s");
    for n in 1..=2 {
        commit_in_clone(&repo, "s", &format!("src/a{n}.txt"), "no\n", "suite fails");
        player.tick().expect("tick");
    }
    assert_eq!(task_status(&hub, "s"), Status::Escalated);
    player.sessions_mut().alive.remove("s");
    let err = player.reset_relaunch("s", false).expect_err("strikes");
    assert!(err.contains("strike limit: suite 2/2"), "{err}");
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();
    records::record_relaunch_escalation(&hub, "s", &tip, "relaunch limit (2) reached: the worker was relaunched 2 times since its last progress and died again", false).unwrap();
    let err = player
        .reset_relaunch("s", false)
        .expect_err("strikes first");
    assert!(err.contains("strike limit"), "{err}");
    assert!(records::cleared_escalations(&hub, "s").is_empty());
}

#[test]
fn progress_on_the_task_branch_starts_the_relaunch_count_over() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "p");
    for _ in 0..2 {
        player.sessions_mut().alive.remove("p");
        player.relaunch("p").expect("relaunch");
    }
    assert_eq!(relaunches(&hub, "p"), 2);

    // The worker's finish is progress.
    commit_in_clone(&repo, "p", "src/feature.txt", "done\n", "finish work");
    player.tick().expect("finish tick");
    assert_eq!(task_status(&hub, "p"), Status::Tested);
    assert_eq!(relaunches(&hub, "p"), 0);

    // Sent back, the worker may be relaunched afresh.
    director_pull_task_branch(repo.path(), "p");
    write_file(
        repo.path(),
        &*tests_file("p"),
        &*task_suite_lean("[[check]]\nname = \"p-check\"\ncmd = \"test -f src/feature.txt && test -f src/feature2.txt\"\n"),
    );
    write_file(repo.path(), "tasks/p/reviews/r1.md", "add src/feature2.txt\n");
    commit(repo.path(), "send-back: p r1");
    player.tick().expect("send-back tick");
    assert_eq!(task_status(&hub, "p"), Status::InRevision);
    player.sessions_mut().alive.remove("p");
    player.relaunch("p").expect("relaunch after progress");
    assert_eq!(relaunches(&hub, "p"), 1);
}

// --------------------------------------------------------------------------
// TmuxSessions smoke test (real tmux, no other test touches it)
// --------------------------------------------------------------------------

#[test]
fn tmux_smoke_test() {
    let has_tmux = Command::new("which")
        .arg("tmux")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_tmux {
        eprintln!("skipping tmux_smoke_test: tmux not found on PATH");
        return;
    }

    let project = format!(
        "dashtest{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut sessions = player::TmuxSessions::new(project);

    let dir = TempDir::new().expect("tempdir");
    let prompt_file = dir.path().join("prompt.md");
    // `sleep` treats its trailing arg (the prompt content, via `$(cat ...)`)
    // as another duration to add, so it must parse as a number — anything
    // else makes `sleep` exit immediately instead of staying alive.
    std::fs::write(&prompt_file, "0\n").expect("write prompt file");

    sessions
        .launch("smoke", dir.path(), "sleep 30", &prompt_file)
        .expect("tmux launch should succeed");
    assert!(
        sessions.alive("smoke"),
        "session should be alive right after launch"
    );

    sessions.send("smoke", "a message\nwith a second line\n"); // must not panic or error

    sessions.kill("smoke");
    assert!(
        !sessions.alive("smoke"),
        "session should be gone after kill"
    );
}

#[test]
fn finish_touching_an_api_file_or_the_manifest_is_rejected_with_pins() {
    for target in ["api/ab.rs", "architecture/Apis/ab.lean", "architecture/Architecture.lean"] {
        let repo = director_repo();
        write_lean_arch(repo.path(), &format!("{LEAN_AB_MANIFEST}{APP_ROOT}"), &[("ab", LEAN_AB_IDL)], &[]);
        write_file(repo.path(), "api/ab.rs", "pub enum Ab { X }\n");
        commit(repo.path(), "add architecture");
        let (_home_dir, home) = init_hub(&repo);
        let mut player = open_player(&home);

        plan_and_post(&repo, &mut player, "y");
        commit_in_clone(&repo, "y", target, "changed\n", "touch the architecture");

        let report = player.tick().expect("tick should succeed");
        assert_eq!(report.rejected.len(), 1, "{target}: {report}");
        assert_eq!(report.rejected[0].reason, Reason::Pins, "{target}");
    }
}

// --------------------------------------------------------------------------
// Worker scope (spec §8.3)
// --------------------------------------------------------------------------

/// Main's suite on the `a`/`b` architecture: `unit` (all) and `story`
/// covering both, with its own file `stories/s.sh`.
const AB_SUITE: &str = "import Architecture\nopen Dashboard\n\n\
check unit where\n  cmd = \"true\"\n  covers = all\n\n\
check story where\n  cmd = \"sh stories/s.sh\"\n  covers = [a, b]\n  files = [\"stories/s.sh\"]\n";

/// A hub on the `a`/`b` architecture with `slug` planned on `modules =
/// ["a"]` and posted (task check `test -f a/feature.txt`, covering `a`).
fn scoped(slug: &str) -> (DirectorRepo, TempDir, Home, Player<NullSessions>) {
    scoped_with(slug, &format!("{LEAN_AB_MANIFEST}{APP_ROOT}"), AB_SUITE)
}

/// `scoped` with another manifest and main suite.
fn scoped_with(slug: &str, manifest: &str, suite: &str) -> (DirectorRepo, TempDir, Home, Player<NullSessions>) {
    let repo = director_repo();
    write_lean_arch(repo.path(), manifest, &[("ab", LEAN_AB_IDL)], &[("Tests", suite)]);
    for (f, t) in [("a/lib.rs", "pub fn f() {}\n"), ("b/lib.rs", "pub fn g() {}\n"), ("api/.keep", ""), ("stories/s.sh", "true\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "modules a and b");
    let (home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post_on(&repo, &mut player, slug, "a");
    (repo, home_dir, home, player)
}

/// Plans `slug` on `modules = [module]` and posts it (task check `test -f
/// <module>/feature.txt`, covering `module`), asserting the launch.
fn plan_and_post_on(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str, module: &str) {
    let plan_toml = format!("title = \"{slug}\"\nphase = \"p1\"\nmodules = [\"{module}\"]\n");
    checkout_branch(repo.path(), "main");
    plan(repo, player, slug, &plan_toml);
    checkout_branch(repo.path(), &format!("task/{slug}"));
    write_file(repo.path(), &format!("tasks/{slug}/prompt.md"), "do the thing\n");
    let cmd = format!("test -f {module}/feature.txt");
    write_file(repo.path(), &tests_file(slug), &lean_task_tests(&format!("{slug}-check"), &cmd, &format!("[{module}]")));
    commit(repo.path(), &format!("post: {slug}"));
    let report = player.tick().expect("post tick");
    assert_eq!(report.launched, vec![slug.to_string()], "{report}");
}

fn worker_file(slug: &str) -> String {
    format!("architecture/Tests/{slug}.worker.lean")
}

#[test]
fn a_finish_outside_its_modules_is_a_scope_strike() {
    let (repo, _home_dir, home, mut player) = scoped("sc");
    let hub = Git::new(home.hub());
    commit_multi_in_clone(&repo, "sc", &[("a/feature.txt", "x\n"), ("b/lib.rs", "pub fn g2() {}\n")], "finish, and b too");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Scope);
    assert!(report.rejected[0].message.contains("b/lib.rs") && report.rejected[0].message.contains("dashboard scope"), "{report}");
    let st = state::derive(&hub, &|_| false).unwrap();
    let text = dashboard::render::render(&st, &[], false, 100);
    assert!(text.contains("scope 1/2"), "{text}");
    // A second strike escalates.
    commit_in_clone(&repo, "sc", "b/more.rs", "\n", "b again");
    player.tick().expect("tick");
    assert_eq!(task_status(&hub, "sc"), Status::Escalated);
}

/// `slug`'s finish with its own check `wk_own` (covering `covers`, with
/// its file `a/tests/t.sh`) in its worker file.
fn finish_with_own_check(repo: &DirectorRepo, slug: &str, covers: &str) {
    let own = format!("{}  files = [\"a/tests/t.sh\"]\n", lean_task_tests("wk_own", "sh a/tests/t.sh", covers));
    commit_multi_in_clone(
        repo,
        slug,
        &[("a/feature.txt", "x\n"), ("a/tests/t.sh", "test -f a/feature.txt\n"), (&worker_file(slug), &own)],
        "finish with my own check",
    );
}

#[test]
fn the_worker_adds_its_own_checks_within_its_modules() {
    // Covering another module, or all: scope.
    for covers in ["[b]", "all"] {
        let (repo, _home_dir, _home, mut player) = scoped("wk");
        finish_with_own_check(&repo, "wk", covers);
        let report = player.tick().expect("tick");
        assert_eq!(report.rejected.len(), 1, "{covers}: {report}");
        assert_eq!(report.rejected[0].reason, Reason::Scope, "{covers}");
        assert!(report.rejected[0].message.contains("outside modules"), "{report}");
    }
    // Within `a`, with its own file: the check joins the suite and passes.
    let (repo, _home_dir, home, mut player) = scoped("wk");
    let hub = Git::new(home.hub());
    finish_with_own_check(&repo, "wk", "[a]");
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(task_status(&hub, "wk"), Status::Tested);
    let tip = hub.rev_parse("refs/heads/task/wk").unwrap();
    assert!(dashboard::checks::cached(&hub, &tip).iter().any(|r| r.name == "wk_own" && r.pass));
}

#[test]
fn the_post_and_pinned_check_files_stay_pinned() {
    for (path, what) in [("architecture/Tests/pn.lean", "the post"), ("stories/s.sh", "a pinned check's files")] {
        let (repo, _home_dir, _home, mut player) = scoped("pn");
        commit_multi_in_clone(&repo, "pn", &[("a/feature.txt", "x\n"), (path, "-- changed\n")], "finish");
        let report = player.tick().expect("tick");
        assert_eq!(report.rejected.len(), 1, "{what}: {report}");
        assert_eq!(report.rejected[0].reason, Reason::Pins, "{what}: {report}");
    }
}

// --------------------------------------------------------------------------
// Run sets (spec §6.6, §11)
// --------------------------------------------------------------------------

/// `LEAN_AB_MANIFEST` with `shared = ["Cargo.toml"]`.
fn ab_shared() -> String {
    format!("{}{APP_ROOT}", LEAN_AB_MANIFEST.replace("max_modules = 2\n", "max_modules = 2\n  shared = [\"Cargo.toml\"]\n"))
}

/// Main's suite: `unit` (all) and `b_only` covering `b`, which fails
/// once `a/feature.txt` and `b/moved.txt` both exist in the tree.
const RUN_SET_SUITE: &str = "import Architecture\nopen Dashboard\n\n\
check unit where\n  cmd = \"true\"\n  covers = all\n\n\
check b_only where\n  cmd = \"test ! -e \\\"$DASHBOARD_ROOT/a/feature.txt\\\" || test ! -e b/moved.txt\"\n  covers = [b]\n";

#[test]
fn a_post_check_covers_only_the_task_modules() {
    let repo = director_repo();
    write_lean_arch(repo.path(), &format!("{LEAN_AB_MANIFEST}{APP_ROOT}"), &[("ab", LEAN_AB_IDL)], &[("Tests", AB_SUITE)]);
    for (f, t) in [("a/lib.rs", ""), ("b/lib.rs", ""), ("api/.keep", ""), ("stories/s.sh", "true\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "modules a and b");
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    let plan_toml = "title = \"o\"\nphase = \"p1\"\nmodules = [\"a\"]\n";
    plan(&repo, &mut player, "o", plan_toml);
    write_file(repo.path(), "tasks/o/prompt.md", "x\n");
    write_file(repo.path(), &tests_file("o"), &lean_task_tests("o-check", "test -f b/x", "[b]"));
    commit(repo.path(), "post: o");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(report.rejected[0].message.contains("covers b, outside the task's modules"), "{report}");
}

#[test]
fn a_finish_runs_its_run_set_unless_it_touches_shared() {
    // b_only fails on this branch (it has a/feature.txt), outside the run
    // set of an `a` task: the finish is tested.
    let fails_with_a = RUN_SET_SUITE.replace(" || test ! -e b/moved.txt", "");
    let (repo, _home_dir, home, mut player) = scoped_with("rs", &ab_shared(), &fails_with_a);
    let hub = Git::new(home.hub());
    commit_multi_in_clone(&repo, "rs", &[("a/feature.txt", "x\n")], "finish");
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(task_status(&hub, "rs"), Status::Tested);
    let st = state::derive(&hub, &|_| false).unwrap();
    let text = dashboard::render::render(&st, &[], false, 200);
    assert!(text.contains("run set (2): "), "{text}");
    let tip = hub.rev_parse("refs/heads/task/rs").unwrap();
    let ran: Vec<String> = dashboard::checks::cached(&hub, &tip).into_iter().map(|r| r.name).collect();
    assert!(!ran.contains(&"b_only".to_string()), "{ran:?}");

    // Touching `shared` runs the whole suite: b_only fails it.
    let (repo, _home_dir, _home, mut player) = scoped_with("sh", &ab_shared(), RUN_SET_SUITE);
    commit_multi_in_clone(&repo, "sh", &[("a/feature.txt", "x\n"), ("b/moved.txt", "")], "finish, b too");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected[0].reason, Reason::Scope, "{report}");
    let (repo, _home_dir, _home, mut player) = scoped_with("sh", &ab_shared(), &RUN_SET_SUITE.replace("test ! -e b/moved.txt", "test ! -e Cargo.toml"));
    commit_multi_in_clone(&repo, "sh", &[("a/feature.txt", "x\n"), ("Cargo.toml", "[package]\n")], "finish with a shared file");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Suite, "{report}");
    assert!(report.rejected[0].message.contains("b_only"), "{report}");
}

#[test]
fn landing_adds_the_modules_main_changed() {
    // `bt` (on b) lands b/moved.txt first; `at` (on a) then lands on a main
    // that moved in b, so its landing runs b_only, which fails on the merge.
    let (repo, _home_dir, home, mut player) = scoped_with("at", &ab_shared(), RUN_SET_SUITE);
    let hub = Git::new(home.hub());
    plan_and_post_on(&repo, &mut player, "bt", "b");
    commit_multi_in_clone(&repo, "bt", &[("b/feature.txt", "x\n"), ("b/moved.txt", "")], "finish b");
    commit_multi_in_clone(&repo, "at", &[("a/feature.txt", "x\n")], "finish a");
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    for slug in ["bt", "at"] {
        assert_eq!(task_status(&hub, slug), Status::Tested, "{slug}");
        director_pull_task_branch(repo.path(), slug);
        commit_empty(repo.path(), &format!("accept: {slug}"));
        player.tick().expect("accept tick");
    }
    assert_eq!(task_status(&hub, "bt"), Status::Accepted);
    assert_eq!(task_status(&hub, "at"), Status::InRevision);
    let relayed = player.sessions().sent.iter().filter(|(k, _)| k == "at").map(|(_, m)| m.clone()).collect::<Vec<_>>().join("\n");
    assert!(relayed.contains("b_only"), "{relayed}");
}

#[test]
fn a_finish_failing_only_where_main_fails_is_held() {
    let suite = RUN_SET_SUITE.replace("check unit where\n  cmd = \"true\"", "check unit where\n  cmd = \"false\"");
    let (repo, _home_dir, home, mut player) = scoped_with("mf", &ab_shared(), &suite);
    let hub = Git::new(home.hub());
    let report = player.tick().expect("tick");
    assert!(report.main_failing.is_empty(), "reported on the first tick already");
    let failing = dashboard::events::read_all(&home).into_iter().filter(|e| e.kind == dashboard::events::EventKind::MainFailing).count();
    assert_eq!(failing, 1);
    commit_multi_in_clone(&repo, "mf", &[("a/feature.txt", "x\n")], "finish");
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.held.len(), 1, "{report}");
    assert!(records::held_commit(&hub, "mf").is_some());
}

// --------------------------------------------------------------------------
// The task workspace (spec §21)
// --------------------------------------------------------------------------

#[test]
fn the_worker_tree_holds_only_the_tasks_files() {
    let (repo, _home_dir, home, _player) = scoped("sp");
    let clone = paths::worker_clone(&repo.director_path(), "sp");
    for f in ["a/lib.rs", "architecture/Architecture.lean", "architecture/Tests.lean", "tasks/sp/task.toml", "api/.keep", "stories/s.sh"] {
        assert!(clone.join(f).exists(), "{f} should be in the worker's tree");
    }
    for f in ["b/lib.rs", "roadmap.toml", "src/.keep"] {
        assert!(!clone.join(f).exists(), "{f} should be absent from the worker's tree");
    }
    let git = Git::new(&clone);
    assert_eq!(git.run(&["show", "HEAD:b/lib.rs"]).unwrap(), "pub fn g() {}");
    assert!(git.is_sparse());
    let hub = Git::new(home.hub());
    let main = hub.rev_parse("refs/heads/main").unwrap();
    if hub.show_file(&main, ".gitignore").is_some() {
        assert!(clone.join(".gitignore").exists());
    }
    let workspace = paths::worker_workspace(&repo.director_path());
    assert_eq!(Git::new(&workspace).config_get("extensions.worktreeConfig").as_deref(), Some("true"));
    let entries: Vec<String> = std::fs::read_dir(&workspace)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, [".git"], "the workspace holds its repository only");
}

/// Main's suite with `b_story` too: a check outside `a`'s run set whose
/// file, `stories/b.sh`, is absent from `a`'s worker tree.
const AB_B_SUITE: &str = "import Architecture\nopen Dashboard\n\n\
check unit where\n  cmd = \"true\"\n  covers = all\n\n\
check story where\n  cmd = \"sh stories/s.sh\"\n  covers = [a, b]\n  files = [\"stories/s.sh\"]\n\n\
check b_story where\n  cmd = \"sh stories/b.sh\"\n  covers = [b]\n  files = [\"stories/b.sh\"]\n";

/// `scoped` on `AB_B_SUITE`, finished (`tested`), then, after `before` runs
/// on the worker's clone, sent back with a new check `r1` reading
/// `stories/b.sh`. Returns the clone.
fn sent_back_reading_b_story(before: impl FnOnce(&Path)) -> (DirectorRepo, TempDir, Home, Player<NullSessions>, PathBuf) {
    let repo = director_repo();
    write_lean_arch(repo.path(), &format!("{LEAN_AB_MANIFEST}{APP_ROOT}"), &[("ab", LEAN_AB_IDL)], &[("Tests", AB_B_SUITE)]);
    for (f, t) in [("a/lib.rs", "pub fn f() {}\n"), ("b/lib.rs", "pub fn g() {}\n"), ("api/.keep", ""), ("stories/s.sh", "true\n"), ("stories/b.sh", "true\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "modules a and b");
    let (home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post_on(&repo, &mut player, "sp", "a");
    let hub = Git::new(home.hub());
    let clone = paths::worker_clone(&repo.director_path(), "sp");
    assert!(!clone.join("stories/b.sh").exists());
    commit_in_clone(&repo, "sp", "a/feature.txt", "x\n", "finish");
    player.tick().expect("finish tick");
    assert_eq!(task_status(&hub, "sp"), Status::Tested);
    before(&clone);

    director_pull_task_branch(repo.path(), "sp");
    let r1 = "\ncheck r1 where\n  cmd = \"sh stories/b.sh && test -f a/feature2.txt\"\n  covers = [a]\n  files = [\"stories/b.sh\"]\n";
    let tests = std::fs::read_to_string(repo.path().join(tests_file("sp"))).unwrap();
    write_file(repo.path(), &tests_file("sp"), &(tests + r1));
    write_file(repo.path(), "tasks/sp/reviews/r1.md", "add a/feature2.txt\n");
    commit(repo.path(), "send-back: sp r1");
    let report = player.tick().expect("send-back tick");
    assert_eq!(task_status(&hub, "sp"), Status::InRevision, "{report}");
    assert!(report.errors.is_empty(), "{report}");
    (repo, home_dir, home, player, clone)
}

#[test]
fn a_send_back_widens_the_worker_tree() {
    let (_repo, _home_dir, _home, _player, clone) = sent_back_reading_b_story(|_| {});
    let git = Git::new(&clone);
    assert!(git.sparse_list().contains(&"/stories/b.sh".to_string()), "{:?}", git.sparse_list());
    git.run(&["pull", "--quiet", "--ff-only", "origin", "task/sp"]).expect("pull");
    assert!(clone.join("stories/b.sh").exists());
    assert!(clone.join("tasks/sp/reviews/r1.md").exists());
    assert!(!clone.join("b/lib.rs").exists());
}

#[test]
fn a_re_pin_keeps_the_workers_uncommitted_work() {
    let (_repo, _home_dir, _home, _player, clone) = sent_back_reading_b_story(|clone| {
        write_file(clone, "a/wip.rs", "wip\n");
        write_file(clone, "a/lib.rs", "pub fn f2() {}\n");
    });
    assert_eq!(std::fs::read_to_string(clone.join("a/wip.rs")).unwrap(), "wip\n");
    assert_eq!(std::fs::read_to_string(clone.join("a/lib.rs")).unwrap(), "pub fn f2() {}\n");
}

// --------------------------------------------------------------------------
// `dashboard trim` (spec §6.5)
// --------------------------------------------------------------------------

/// A tested task `slug` on module `a`: its post declares `<slug>-check` and
/// `tr_one` / `tr_two`, which share `a/tests/shared.sh`; its worker adds
/// `wk_own` (file `a/tests/t.sh`) in its worker file.
fn tested_for_trim(slug: &str) -> (DirectorRepo, TempDir, Home, Player<NullSessions>) {
    let repo = director_repo();
    write_lean_arch(repo.path(), &format!("{LEAN_AB_MANIFEST}{APP_ROOT}"), &[("ab", LEAN_AB_IDL)], &[("Tests", AB_SUITE)]);
    for (f, t) in [("a/lib.rs", "pub fn f() {}\n"), ("b/lib.rs", "pub fn g() {}\n"), ("api/.keep", ""), ("stories/s.sh", "true\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "modules a and b");
    let (home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player, slug, &format!("title = \"{slug}\"\nphase = \"p1\"\nmodules = [\"a\"]\n"));
    checkout_branch(repo.path(), &format!("task/{slug}"));
    write_file(repo.path(), &format!("tasks/{slug}/prompt.md"), "do the thing\n");
    let tests = format!(
        "{}\n-- Scaffolding: the first step.\n{}  files = [\"a/tests/one.sh\", \"a/tests/shared.sh\"]\n\n{}  files = [\"a/tests/two.sh\", \"a/tests/shared.sh\"]\n",
        lean_task_tests(&format!("{slug}-check"), "test -f a/feature.txt", "[a]"),
        lean_check("tr_one", "sh a/tests/one.sh", "[a]"),
        lean_check("tr_two", "sh a/tests/two.sh", "[a]"),
    );
    write_file(repo.path(), &tests_file(slug), &tests);
    write_file(repo.path(), "a/tests/one.sh", "sh a/tests/shared.sh\n");
    write_file(repo.path(), "a/tests/two.sh", "sh a/tests/shared.sh\n");
    write_file(repo.path(), "a/tests/shared.sh", "test -f a/feature.txt\n");
    commit(repo.path(), &format!("post: {slug}"));
    let report = player.tick().expect("post tick");
    assert_eq!(report.launched, vec![slug.to_string()], "{report}");
    finish_with_own_check(&repo, slug, "[a]");
    let report = player.tick().expect("finish tick");
    assert!(report.rejected.is_empty(), "{report}");
    (repo, home_dir, home, player)
}

fn trim(repo: &DirectorRepo, home: &Home, slug: &str, names: &[&str]) -> Result<dashboard::trim::Outcome, String> {
    let scratch = TempDir::new().unwrap();
    let exporter = dashboard::lean::Exporter::new(scratch.path().to_path_buf(), dashboard::sandbox::Sandbox::off());
    let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    dashboard::trim::run(&Git::new(home.hub()), &repo.git(), slug, &names, &exporter)
}

/// The player imports `out`'s commit as a trim and the task stays tested.
fn assert_trim_imported(player: &mut Player<NullSessions>, hub: &Git, slug: &str, out: &dashboard::trim::Outcome) {
    let report = player.tick().expect("trim tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Trim, "{report}");
    assert_eq!(hub.rev_parse(&format!("refs/heads/task/{slug}")).as_deref(), Some(out.commit.as_str()));
    assert_eq!(task_status(hub, slug), Status::Tested);
}

#[test]
fn trim_keeps_a_file_another_check_still_lists() {
    let (repo, _home_dir, home, mut player) = tested_for_trim("ts");
    let hub = Git::new(home.hub());

    let out = trim(&repo, &home, "ts", &["tr_one"]).expect("trim tr_one");
    assert_eq!(out.plan.deleted, vec!["a/tests/one.sh".to_string()]);
    assert_eq!(
        out.plan.kept,
        vec![("a/tests/shared.sh".to_string(), "also in the files of check tr_two".to_string())]
    );
    // The checked-out branch moved with its tree.
    assert_eq!(repo.git().rev_parse("HEAD").as_deref(), Some(out.commit.as_str()));
    assert!(!repo.path().join("a/tests/one.sh").exists());
    let lean = std::fs::read_to_string(repo.path().join(tests_file("ts"))).unwrap();
    assert!(!lean.contains("tr_one") && !lean.contains("Scaffolding"), "{lean}");
    assert!(lean.contains("check tr_two where") && lean.contains("check «ts-check» where"), "{lean}");
    assert!(repo.git().commit_body(&out.commit).unwrap_or_default().contains("a/tests/shared.sh"));
    assert_trim_imported(&mut player, &hub, "ts", &out);

    // Its last lister gone, the shared file goes too.
    let out = trim(&repo, &home, "ts", &["tr_two"]).expect("trim tr_two");
    let mut deleted = out.plan.deleted.clone();
    deleted.sort();
    assert_eq!(deleted, vec!["a/tests/shared.sh".to_string(), "a/tests/two.sh".to_string()]);
    assert!(out.plan.kept.is_empty());
    assert_trim_imported(&mut player, &hub, "ts", &out);
}

#[test]
fn trim_refuses_unknown_names_other_files_checks_and_untested_tasks() {
    let (repo, _home_dir, home, mut player) = tested_for_trim("tu");
    let tip = repo.git().rev_parse("refs/heads/task/tu");

    let err = trim(&repo, &home, "tu", &["nope"]).unwrap_err();
    assert!(err.contains("no check \"nope\"") && err.contains("tr_one") && err.contains("wk_own"), "{err}");
    let err = trim(&repo, &home, "tu", &["tr_one", "story"]).unwrap_err();
    assert!(err.contains("architecture/Tests.lean"), "{err}");
    assert!(err.contains("a trim drops only checks task/tu added"), "{err}");
    let err = trim(&repo, &home, "nosuch", &["tr_one"]).unwrap_err();
    assert!(err.contains("no task/nosuch"), "{err}");
    checkout_branch(repo.path(), "main");
    plan(&repo, &mut player, "tv", "title = \"tv\"\nphase = \"p1\"\nmodules = [\"a\"]\n");
    let err = trim(&repo, &home, "tv", &["tr_one"]).unwrap_err();
    assert!(err.contains("task/tv is ready"), "{err}");
    assert_eq!(repo.git().rev_parse("refs/heads/task/tu"), tip, "a refused trim commits nothing");
}

#[test]
fn trim_drops_a_worker_check_and_its_whole_file() {
    let (repo, _home_dir, home, mut player) = tested_for_trim("tw");
    let hub = Git::new(home.hub());
    // Off the branch: only its ref moves.
    checkout_branch(repo.path(), "main");

    let out = trim(&repo, &home, "tw", &["wk_own", "tr_one", "tr_two"]).expect("trim");
    assert!(out.plan.lean.contains(&(worker_file("tw"), None)), "{:?}", out.plan.lean);
    let mut deleted = out.plan.deleted.clone();
    deleted.sort();
    assert_eq!(deleted, ["a/tests/one.sh", "a/tests/shared.sh", "a/tests/t.sh", "a/tests/two.sh"]);
    assert_eq!(repo.git().rev_parse("refs/heads/task/tw").as_deref(), Some(out.commit.as_str()));
    assert_eq!(repo.git().run(&["symbolic-ref", "--short", "HEAD"]).unwrap(), "main");
    assert_trim_imported(&mut player, &hub, "tw", &out);
    let tip = hub.rev_parse("refs/heads/task/tw").unwrap();
    assert!(hub.blob_hash(&tip, &worker_file("tw")).is_none());
    assert!(hub.blob_hash(&tip, &tests_file("tw")).is_some());

    // A dropped name is unknown from then on.
    let err = trim(&repo, &home, "tw", &["wk_own"]).unwrap_err();
    assert!(err.contains("no check \"wk_own\""), "{err}");
}
