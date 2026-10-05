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
        "title = \"Foo\"\nphase = \"p1\"\n",
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
        "title = \"Leftover\"\nphase = \"p1\"\n",
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
        "title = \"Leftover\"\nphase = \"p1\"\n\n[check]\nname = \"leftover-check\"\ncmd = \"test -f feature.txt\"\n",
    );
    write_file(
        repo.path(),
        "tests.d/leftover.toml",
        "[[check]]\nname = \"leftover-check\"\ncmd = \"test -f feature.txt\"\n",
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
    commit_in_clone(&repo, "x", "feature.txt", "done\n", "finish work");

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
    commit_in_clone(&repo, "x", "feature.txt", "done\n", "finish work");
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

    commit_in_clone(&repo, "x", "feature.txt", "done\n", "finish work");
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
        "title = \"Y (tampered)\"\nphase = \"p1\"\n\n[check]\nname = \"y-check\"\ncmd = \"true\"\n",
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
fn finish_adding_a_suite_fragment_is_rejected_with_pins() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "frag");
    commit_in_clone(
        &repo,
        "frag",
        "tests.d/extra.toml",
        "[[check]]\nname = \"extra\"\ncmd = \"true\"\n",
        "add a suite check",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Pins);
    assert!(
        report.rejected[0].message.contains("tests.d/extra.toml"),
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
        "unrelated.txt",
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
    // feature.txt now makes it pass even though the first commit alone did
    // not (spec's "each is a finish claim, validated cumulatively").
    commit_in_clone(&repo, "z", "feature.txt", "done\n", "actually finish it");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Finish);
    assert_eq!(task_status(&hub, "z"), Status::Tested);
}

#[test]
fn worker_adding_a_new_test_file_is_allowed() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "w");
    commit_multi_in_clone(
        &repo,
        "w",
        &[
            ("feature.txt", "done\n"),
            ("tests/extra_test.txt", "some new test scaffolding\n"),
        ],
        "finish, plus a new test file",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Finish);
    assert_eq!(task_status(&hub, "w"), Status::Tested);
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

#[test]
fn worker_commit_built_on_a_stale_pre_send_back_tip_is_diverged_history() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "d");
    commit_in_clone(&repo, "d", "feature.txt", "done\n", "finish work");
    player.tick().expect("finish tick should succeed");
    assert_eq!(task_status(&hub, "d"), Status::Tested);

    director_pull_task_branch(repo.path(), "d");
    write_file(
        repo.path(),
        "tests.d/d.toml",
        "[[check]]\nname = \"d-check\"\ncmd = \"test -f feature.txt && test -f feature2.txt\"\n",
    );
    write_file(
        repo.path(),
        "tasks/d/reviews/r1.md",
        "add feature2.txt too\n",
    );
    commit(repo.path(), "send-back: d r1");
    let report = player.tick().expect("send-back tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::SendBack);
    assert_eq!(task_status(&hub, "d"), Status::InRevision);

    // The worker keeps committing on its stale pre-send-back tip instead
    // of pulling first — its history no longer contains the hub's new tip.
    commit_in_clone(
        &repo,
        "d",
        "feature2.txt",
        "done too\n",
        "add feature2 without pulling first",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::History);
    let sent = &player.sessions().sent;
    assert!(
        sent.iter().any(|(slug, msg)| slug == "d"
            && msg.contains("git fetch origin && git reset --hard origin/task/d")),
        "{sent:?}"
    );
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
        "attempt1.txt",
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
        "attempt2.txt",
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
    commit_in_clone(&repo, "full", "feature.txt", "done\n", "finish work");
    let report = player.tick().expect("finish tick should succeed");
    assert_eq!(report.imported[0].2, Transition::Finish);
    assert_eq!(task_status(&hub, "full"), Status::Tested);

    director_pull_task_branch(repo.path(), "full");
    write_file(
        repo.path(),
        "tests.d/full.toml",
        "[[check]]\nname = \"full-check\"\ncmd = \"test -f feature.txt && test -f feature2.txt\"\n",
    );
    write_file(
        repo.path(),
        "tasks/full/reviews/r1.md",
        "please also add feature2.txt\n",
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
        "feature2.txt",
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
