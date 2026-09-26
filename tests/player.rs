// Integration tests for src/player.rs (spec.md §5, §9, §11): fetch, the
// history rules, director `main` (docs-only) commits, director `task/*`
// plan/replan, and unblock merges. Every test builds a real director repo, a
// real hub via `dashboard::init::init`, and drives `Player::tick` against
// them with `NullSessions` (no tmux). Step 5b (post/send-back/accept/retire)
// and step 6 (subagent import/launch) aren't implemented yet: anywhere this
// file needs an accepted or retired task, it hand-builds that state with
// direct git plumbing on the hub, exactly as spec.md's step 5a instructions
// call for.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::player::{NullSessions, Player};
use dashboard::records::{self, Actor, Transition};
use dashboard::state::{self, Status};
use tempfile::TempDir;

// --------------------------------------------------------------------------
// Fixture helpers
// --------------------------------------------------------------------------

fn init_hub(repo: &DirectorRepo) -> (TempDir, Home) {
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init should succeed");
    (home_dir, Home { root: report.home })
}

fn open_player(home: &Home) -> Player<NullSessions> {
    Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("player open")
}

/// A director repo whose `roadmap.toml` declares `docs_paths = ["docs"]`, so
/// tests that need a real doc-path commit (not just a roadmap.toml edit)
/// have somewhere to write.
fn director_repo_with_docs_paths() -> DirectorRepo {
    let repo = director_repo();
    write_file(repo.path(), "roadmap.toml", "title = \"Test Project\"\ndocs_paths = [\"docs\"]\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n");
    commit(repo.path(), "declare docs_paths");
    repo
}

fn task_status(hub: &Git, slug: &str) -> Status {
    let hub_state = state::derive(hub, &|_| false).expect("derive should succeed");
    hub_state.tasks.iter().find(|t| t.slug == slug).unwrap_or_else(|| panic!("no task state for {slug:?}")).status
}

// --------------------------------------------------------------------------
// Director main: docs commits vs ownership/suite/history rejections
// --------------------------------------------------------------------------

#[test]
fn main_doc_commit_is_imported_and_non_doc_path_is_rejected_as_ownership() {
    let repo = director_repo_with_docs_paths();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    write_file(repo.path(), "docs/adr-0001.md", "# ADR 1\n");
    commit(repo.path(), "docs: add adr 1");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Docs);
    let director_main = repo.git().rev_parse("HEAD").unwrap();
    assert_eq!(hub.rev_parse("refs/heads/main").as_deref(), Some(director_main.as_str()));

    // A commit outside roadmap.toml/docs_paths is rejected — recorded under
    // `_main`, which state::derive never counts as a strike on any task.
    write_file(repo.path(), "src/code.rs", "fn main() {}\n");
    commit(repo.path(), "add code file");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].scope, "_main");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    // main didn't move.
    assert_eq!(hub.rev_parse("refs/heads/main").as_deref(), Some(director_main.as_str()));
}

#[test]
fn main_commit_whose_suite_fails_is_rejected_with_suite() {
    let repo = director_repo();
    write_file(repo.path(), "tests.toml", "[[check]]\nname = \"unit\"\ncmd = \"false\"\n");
    commit(repo.path(), "make the suite fail");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let hub_main_before = hub.rev_parse("refs/heads/main").unwrap();
    let mut player = open_player(&home);

    write_file(repo.path(), "roadmap.toml", "title = \"Updated Title\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n");
    commit(repo.path(), "update title");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Suite);
    assert_eq!(hub.rev_parse("refs/heads/main").as_deref(), Some(hub_main_before.as_str()));
}

#[test]
fn diverged_main_is_rejected_with_history() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let hub_main_before = hub.rev_parse("refs/heads/main").unwrap();
    let mut player = open_player(&home);

    // Rewrite the director's only commit: the new tip shares no history
    // with the hub's main, so it's neither ahead nor behind — diverged.
    repo.git().run(&["commit", "--amend", "-q", "-m", "amended initial commit"]).unwrap();

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::History);
    assert_eq!(report.rejected[0].scope, "_main");
    assert_eq!(hub.rev_parse("refs/heads/main").as_deref(), Some(hub_main_before.as_str()));
}

#[test]
fn director_behind_hub_is_a_no_op() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    // Nothing changed since `dashboard init` fetched main: hub == director.
    let report = player.tick().expect("tick should succeed");
    assert!(report.is_empty(), "{report}");
}

// --------------------------------------------------------------------------
// Plan
// --------------------------------------------------------------------------

#[test]
fn plan_happy_path() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/foo", "tasks/foo/task.toml", "title = \"Foo\"\nphase = \"p1\"\n", "plan: foo");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Plan);
    assert!(hub.ref_exists("refs/heads/task/foo"));
    assert_eq!(task_status(&hub, "foo"), Status::Ready);
}

#[test]
fn plan_rejects_bad_slug() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/Bad_Slug", "tasks/Bad_Slug/task.toml", "title = \"Bad\"\n", "plan: bad slug");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(!hub.ref_exists("refs/heads/task/Bad_Slug"));
}

#[test]
fn plan_rejects_unknown_dependency() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/needs-ghost", "tasks/needs-ghost/task.toml", "title = \"Needs ghost\"\nphase = \"p1\"\ndepends_on = [\"ghost\"]\n", "plan: needs-ghost");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(!hub.ref_exists("refs/heads/task/needs-ghost"));
}

#[test]
fn plan_rejects_retired_dependency() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/deadend", "tasks/deadend/task.toml", "title = \"Dead end\"\nphase = \"p1\"\n", "plan: deadend");
    player.tick().expect("tick should succeed");
    assert!(hub.ref_exists("refs/heads/task/deadend"));

    // Hand-retire it (plumbing — retire isn't implemented until step 5b).
    let deadend_tip = hub.rev_parse("refs/heads/task/deadend").unwrap();
    hub.update_ref("refs/retired/deadend", &deadend_tip).unwrap();
    hub.delete_ref("refs/heads/task/deadend").unwrap();

    director_commit_on_branch(repo.path(), "task/needs-deadend", "tasks/needs-deadend/task.toml", "title = \"Needs deadend\"\nphase = \"p1\"\ndepends_on = [\"deadend\"]\n", "plan: needs-deadend");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(!hub.ref_exists("refs/heads/task/needs-deadend"));
}

#[test]
fn plan_rejects_bad_phase() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/bad-phase", "tasks/bad-phase/task.toml", "title = \"Bad phase\"\nphase = \"nonexistent\"\n", "plan: bad phase");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(!hub.ref_exists("refs/heads/task/bad-phase"));
}

#[test]
fn plan_rejects_extra_paths_in_the_plan_commit() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    checkout_branch(repo.path(), "task/extra-paths");
    write_file(repo.path(), "tasks/extra-paths/task.toml", "title = \"Extra\"\nphase = \"p1\"\n");
    write_file(repo.path(), "extra.txt", "this shouldn't be here\n");
    commit(repo.path(), "plan: extra-paths (with a stray file)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(!hub.ref_exists("refs/heads/task/extra-paths"));
}

#[test]
fn plan_rejects_a_cycle_via_replan() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    // Plan B first (no deps) in its own tick, so the dependency exists by
    // the time A is planned (import order within a single tick is
    // alphabetical by ref name, so doing both at once could process "a"
    // before "b" and reject A for an unmet — not yet cyclic — dependency).
    director_commit_on_branch(repo.path(), "task/b", "tasks/b/task.toml", "title = \"B\"\nphase = \"p1\"\n", "plan: b");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert!(hub.ref_exists("refs/heads/task/b"));

    // Then A depends_on=[b]: A -> B.
    director_commit_on_branch(repo.path(), "task/a", "tasks/a/task.toml", "title = \"A\"\nphase = \"p1\"\ndepends_on = [\"b\"]\n", "plan: a");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert!(hub.ref_exists("refs/heads/task/a"));
    let b_tip_before = hub.rev_parse("refs/heads/task/b").unwrap();

    // Replan B to depend on A: B -> A closes the cycle A -> B -> A.
    director_commit_on_branch(repo.path(), "task/b", "tasks/b/task.toml", "title = \"B\"\nphase = \"p1\"\ndepends_on = [\"a\"]\n", "replan: b depends on a");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(hub.rev_parse("refs/heads/task/b").as_deref(), Some(b_tip_before.as_str()), "the cyclic replan must not move task/b");
}

#[test]
fn fix_on_top_commit_after_a_rejected_plan_passes_cumulatively() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    checkout_branch(repo.path(), "task/fix");
    write_file(repo.path(), "tasks/fix/task.toml", "title = \"Fix\"\nphase = \"p1\"\n");
    write_file(repo.path(), "extra.txt", "oops\n");
    commit(repo.path(), "plan: fix (bad, has an extra file)");
    std::fs::remove_file(repo.path().join("extra.txt")).unwrap();
    commit(repo.path(), "fix: drop the extra file");

    // Both commits arrive in the same tick; the second's cumulative diff
    // from main is clean, so it plans the branch despite the first commit
    // being rejected.
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Plan);
    assert!(hub.ref_exists("refs/heads/task/fix"));
    let director_tip = repo.git().rev_parse("HEAD").unwrap();
    assert_eq!(hub.rev_parse("refs/heads/task/fix").as_deref(), Some(director_tip.as_str()));
}

#[test]
fn merge_commit_on_a_task_branch_is_rejected_with_history() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/x", "tasks/x/task.toml", "title = \"X\"\nphase = \"p1\"\n", "plan: x");
    player.tick().expect("tick should succeed");
    let tip_before = hub.rev_parse("refs/heads/task/x").unwrap();

    // Build a 2-parent commit directly (rather than `git merge`, which would
    // also have to introduce genuinely new content on the other side — and
    // that content would itself show up as an extra, separately-rejected
    // commit in the cumulative walk). Both parents here (the branch tip and
    // main) are already known to the hub, so this is a clean single new
    // commit whose only defect is having two parents.
    let git = repo.git();
    let tree = git.run(&["rev-parse", &format!("{tip_before}^{{tree}}")]).unwrap();
    let main_tip = git.rev_parse("main").unwrap();
    let merge_sha = git.run(&["commit-tree", &tree, "-p", &tip_before, "-p", &main_tip, "-m", "merge main into task/x (not allowed)"]).unwrap();
    checkout_branch(repo.path(), "task/x");
    git.run(&["update-ref", "refs/heads/task/x", &merge_sha]).unwrap();

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::History);
    assert_eq!(report.rejected[0].scope, "x");
    assert_eq!(hub.rev_parse("refs/heads/task/x").as_deref(), Some(tip_before.as_str()));
}

// --------------------------------------------------------------------------
// Replan and the step 5b dispatch point
// --------------------------------------------------------------------------

#[test]
fn replan_happy_path() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/replan-me", "tasks/replan-me/task.toml", "title = \"Original\"\nphase = \"p1\"\n", "plan: replan-me");
    player.tick().expect("tick should succeed");

    director_commit_on_branch(repo.path(), "task/replan-me", "tasks/replan-me/task.toml", "title = \"Revised\"\nphase = \"p1\"\n", "replan: replan-me");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Replan);
    let tip = hub.rev_parse("refs/heads/task/replan-me").unwrap();
    let note = records::import_note(&hub, &tip).unwrap();
    assert_eq!(note.transition, Transition::Replan);
    assert_eq!(note.actor, Actor::Director);
}

#[test]
fn unsupported_transition_is_rejected_with_ownership() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/unsupported", "tasks/unsupported/task.toml", "title = \"Unsupported\"\nphase = \"p1\"\n", "plan: unsupported");
    player.tick().expect("tick should succeed");
    let tip_before = hub.rev_parse("refs/heads/task/unsupported").unwrap();

    // Adding prompt.md is a `post` (spec §9) — not implemented until 5b.
    director_commit_on_branch(repo.path(), "task/unsupported", "tasks/unsupported/prompt.md", "do the thing\n", "post: unsupported");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(report.rejected[0].message.contains("unsupported transition"), "{}", report.rejected[0].message);
    assert_eq!(hub.rev_parse("refs/heads/task/unsupported").as_deref(), Some(tip_before.as_str()));
}

// --------------------------------------------------------------------------
// Unblock merges
// --------------------------------------------------------------------------

#[test]
fn unblock_merge_brings_an_accepted_dependency_into_the_branch() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    // Plan A (no deps) and B (depends on A, not yet accepted) in one tick.
    director_commit_on_branch(repo.path(), "task/a", "tasks/a/task.toml", "title = \"A\"\nphase = \"p1\"\n", "plan: a");
    director_commit_on_branch(repo.path(), "task/b", "tasks/b/task.toml", "title = \"B\"\nphase = \"p1\"\ndepends_on = [\"a\"]\n", "plan: b");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 2, "{report}");
    let b_tip_before = hub.rev_parse("refs/heads/task/b").unwrap();

    // Hand-accept A: merge it into hub main with a `Task: a` trailer, purely
    // via plumbing (accept isn't implemented until step 5b).
    let a_tip = hub.rev_parse("refs/heads/task/a").unwrap();
    let main_tip = hub.rev_parse("refs/heads/main").unwrap();
    let tree = hub.merge_tree_write(&a_tip, &main_tip).expect("merge-tree should run").expect("no conflicts expected");
    let merge_sha = hub.commit_tree(&tree, &[&main_tip, &a_tip], "Merge task/a\n\nTask: a").expect("commit-tree should succeed");
    hub.update_ref_cas("refs/heads/main", &merge_sha, &main_tip).expect("main should fast-forward");

    // B's tip predates the acceptance, so its branch doesn't contain A's
    // merge commit yet: this tick should unblock it.
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.merges.len(), 1, "{report}");
    assert!(report.merges[0].contains("task/b"), "{report}");

    let b_tip_after = hub.rev_parse("refs/heads/task/b").unwrap();
    assert_ne!(b_tip_after, b_tip_before);
    assert!(hub.is_ancestor(&merge_sha, &b_tip_after), "task/b should now contain A's acceptance merge");

    let note = records::import_note(&hub, &b_tip_after).unwrap();
    assert_eq!(note.transition, Transition::MergeUnblock);
    assert_eq!(note.actor, Actor::Player);

    assert_eq!(task_status(&hub, "b"), Status::Ready);
}

// --------------------------------------------------------------------------
// CLI
// --------------------------------------------------------------------------

#[test]
fn player_once_via_binary_prints_a_report() {
    let repo = director_repo();
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init should succeed");

    let output = run_dashboard(&["player", "--once", "--home", report.home.to_str().unwrap()], repo.path());
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no changes"), "unexpected stdout: {stdout}");
}
