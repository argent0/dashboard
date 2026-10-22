// Integration tests for src/player.rs (spec.md §5, §9, §11): fetch, the
// history rules, director `main` (docs-only) commits, director `task/*`
// plan/replan, and unblock merges. Every test builds a real director repo, a
// real hub via `dashboard::init::init`, and drives `Player::tick` against
// them with `NullSessions` (no tmux). Step 5b (post/send-back/accept/retire)
// and step 6 (worker import/launch) aren't implemented yet: anywhere this
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

/// A director repo whose `roadmap.toml` declares `docs_paths = ["docs"]`, so
/// tests that need a real doc-path commit (not just a roadmap.toml edit)
/// have somewhere to write.
fn director_repo_with_docs_paths() -> DirectorRepo {
    let repo = director_repo();
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!(
            "title = \"Test Project\"\ndocs_paths = [\"docs\"]\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n{NO_TASK_AGENTS}"
        ),
    );
    commit(repo.path(), "declare docs_paths");
    repo
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
    assert_eq!(
        hub.rev_parse("refs/heads/main").as_deref(),
        Some(director_main.as_str())
    );

    // A commit outside roadmap.toml/docs_paths is rejected — recorded under
    // `_main`, which state::derive never counts as a strike on any task.
    write_file(repo.path(), "src/code.rs", "fn main() {}\n");
    commit(repo.path(), "add code file");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].scope, "_main");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    // main didn't move.
    assert_eq!(
        hub.rev_parse("refs/heads/main").as_deref(),
        Some(director_main.as_str())
    );
}

#[test]
fn a_failing_main_is_reported_once_and_only_new_failures_reject() {
    let repo = director_repo();
    write_file(
        repo.path(),
        TESTS_LEAN,
        &*suite_lean("[[check]]\nname = \"unit\"\ncmd = \"false\"\n"),
    );
    commit(repo.path(), "make the suite fail");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.main_failing.len(), 1, "{report}");
    assert!(report.main_failing[0].contains("unit"), "{report}");
    assert!(player.tick().expect("tick").main_failing.is_empty(), "once per tip");
    let st = dashboard::state::derive(&hub, &|_| false).unwrap();
    assert_eq!(st.main_failing, ["unit"]);

    // Failing only where main already fails: the director's commit lands.
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!(
            "title = \"Updated Title\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n{NO_TASK_AGENTS}"
        ),
    );
    let titled = commit(repo.path(), "update title");
    let report = player.tick().expect("tick should succeed");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(hub.rev_parse("refs/heads/main").as_deref(), Some(titled.as_str()));

    // A new failure is the commit's own: rejected with suite.
    write_file(
        repo.path(),
        TESTS_LEAN,
        &*suite_lean("[[check]]\nname = \"unit\"\ncmd = \"false\"\n\n[[check]]\nname = \"more\"\ncmd = \"false\"\n"),
    );
    commit(repo.path(), "another failing check");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Suite);
    assert_eq!(hub.rev_parse("refs/heads/main").as_deref(), Some(titled.as_str()));
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
    repo.git()
        .run(&["commit", "--amend", "-q", "-m", "amended initial commit"])
        .unwrap();

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::History);
    assert_eq!(report.rejected[0].scope, "_main");
    assert_eq!(
        hub.rev_parse("refs/heads/main").as_deref(),
        Some(hub_main_before.as_str())
    );
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

    director_commit_on_branch(
        repo.path(),
        "task/foo",
        "tasks/foo/task.toml",
        "title = \"Foo\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: foo",
    );

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

    director_commit_on_branch(
        repo.path(),
        "task/Bad_Slug",
        "tasks/Bad_Slug/task.toml",
        "title = \"Bad\"\n",
        "plan: bad slug",
    );

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

    director_commit_on_branch(
        repo.path(),
        "task/needs-ghost",
        "tasks/needs-ghost/task.toml",
        "title = \"Needs ghost\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"ghost\"]\n",
        "plan: needs-ghost",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(!hub.ref_exists("refs/heads/task/needs-ghost"));
}

#[test]
fn plan_batch_resolves_same_tick_dependency_regardless_of_ref_order() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    // "alpha" depends on "zeta" but sorts before it alphabetically — the
    // order `for_each_ref` returns branches in. Both are planned before a
    // single tick, landing in the same fetch batch: alpha's dependency must
    // still resolve even though, in ref order, the player would reach
    // alpha before it has validated zeta at all.
    director_commit_on_branch(
        repo.path(),
        "task/zeta",
        "tasks/zeta/task.toml",
        "title = \"Zeta\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: zeta",
    );
    director_commit_on_branch(
        repo.path(),
        "task/alpha",
        "tasks/alpha/task.toml",
        "title = \"Alpha\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"zeta\"]\n",
        "plan: alpha",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 0, "{report}");
    assert_eq!(report.imported.len(), 2, "{report}");
    assert!(hub.ref_exists("refs/heads/task/alpha"));
    assert!(hub.ref_exists("refs/heads/task/zeta"));
}

#[test]
fn plan_rejects_retired_dependency() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(
        repo.path(),
        "task/deadend",
        "tasks/deadend/task.toml",
        "title = \"Dead end\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: deadend",
    );
    player.tick().expect("tick should succeed");
    assert!(hub.ref_exists("refs/heads/task/deadend"));

    // Hand-retire it (plumbing — retire isn't implemented until step 5b).
    let deadend_tip = hub.rev_parse("refs/heads/task/deadend").unwrap();
    hub.update_ref("refs/retired/deadend", &deadend_tip)
        .unwrap();
    hub.delete_ref("refs/heads/task/deadend").unwrap();

    director_commit_on_branch(
        repo.path(),
        "task/needs-deadend",
        "tasks/needs-deadend/task.toml",
        "title = \"Needs deadend\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"deadend\"]\n",
        "plan: needs-deadend",
    );
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

    director_commit_on_branch(
        repo.path(),
        "task/bad-phase",
        "tasks/bad-phase/task.toml",
        "title = \"Bad phase\"\nphase = \"nonexistent\"\nmodules = [\"app\"]\n",
        "plan: bad phase",
    );

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
    write_file(
        repo.path(),
        "tasks/extra-paths/task.toml",
        "title = \"Extra\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
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
    director_commit_on_branch(
        repo.path(),
        "task/b",
        "tasks/b/task.toml",
        "title = \"B\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: b",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert!(hub.ref_exists("refs/heads/task/b"));

    // Then A depends_on=[b]: A -> B.
    director_commit_on_branch(
        repo.path(),
        "task/a",
        "tasks/a/task.toml",
        "title = \"A\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"b\"]\n",
        "plan: a",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert!(hub.ref_exists("refs/heads/task/a"));
    let b_tip_before = hub.rev_parse("refs/heads/task/b").unwrap();

    // Replan B to depend on A: B -> A closes the cycle A -> B -> A.
    director_commit_on_branch(
        repo.path(),
        "task/b",
        "tasks/b/task.toml",
        "title = \"B\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"a\"]\n",
        "replan: b depends on a",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(
        hub.rev_parse("refs/heads/task/b").as_deref(),
        Some(b_tip_before.as_str()),
        "the cyclic replan must not move task/b"
    );
}

#[test]
fn fix_on_top_commit_after_a_rejected_plan_passes_cumulatively() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    checkout_branch(repo.path(), "task/fix");
    write_file(
        repo.path(),
        "tasks/fix/task.toml",
        "title = \"Fix\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
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
    assert_eq!(
        hub.rev_parse("refs/heads/task/fix").as_deref(),
        Some(director_tip.as_str())
    );
}

#[test]
fn merge_commit_on_a_task_branch_is_rejected_with_history() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(
        repo.path(),
        "task/x",
        "tasks/x/task.toml",
        "title = \"X\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: x",
    );
    player.tick().expect("tick should succeed");
    let tip_before = hub.rev_parse("refs/heads/task/x").unwrap();

    // Build a 2-parent commit directly (rather than `git merge`, which would
    // also have to introduce genuinely new content on the other side — and
    // that content would itself show up as an extra, separately-rejected
    // commit in the cumulative walk). Both parents here (the branch tip and
    // main) are already known to the hub, so this is a clean single new
    // commit whose only defect is having two parents.
    let git = repo.git();
    let tree = git
        .run(&["rev-parse", &format!("{tip_before}^{{tree}}")])
        .unwrap();
    let main_tip = git.rev_parse("main").unwrap();
    let merge_sha = git
        .run(&[
            "commit-tree",
            &tree,
            "-p",
            &tip_before,
            "-p",
            &main_tip,
            "-m",
            "merge main into task/x (not allowed)",
        ])
        .unwrap();
    checkout_branch(repo.path(), "task/x");
    git.run(&["update-ref", "refs/heads/task/x", &merge_sha])
        .unwrap();

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::History);
    assert_eq!(report.rejected[0].scope, "x");
    assert_eq!(
        hub.rev_parse("refs/heads/task/x").as_deref(),
        Some(tip_before.as_str())
    );
}

// --------------------------------------------------------------------------
// Replan
// --------------------------------------------------------------------------

#[test]
fn replan_happy_path() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(
        repo.path(),
        "task/replan-me",
        "tasks/replan-me/task.toml",
        "title = \"Original\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: replan-me",
    );
    player.tick().expect("tick should succeed");

    director_commit_on_branch(
        repo.path(),
        "task/replan-me",
        "tasks/replan-me/task.toml",
        "title = \"Revised\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "replan: replan-me",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Replan);
    let tip = hub.rev_parse("refs/heads/task/replan-me").unwrap();
    let note = records::import_note(&hub, &tip).unwrap();
    assert_eq!(note.transition, Transition::Replan);
    assert_eq!(note.actor, Actor::Director);
}

// Post/send-back/accept/retire happy-path and rejection coverage lives in
// tests/player_transitions.rs (step 5b); this file keeps its original
// pre-5b scope (fetch, history rules, main docs commits, plan/replan,
// unblock merges).
#[test]
fn post_missing_a_task_check_is_rejected_with_post() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(
        repo.path(),
        "task/nocheck",
        "tasks/nocheck/task.toml",
        "title = \"No check\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: nocheck",
    );
    player.tick().expect("tick should succeed");
    let tip_before = hub.rev_parse("refs/heads/task/nocheck").unwrap();

    // Adding prompt.md is a `post` (spec §9) attempt, but it adds no
    // architecture/Tests/nocheck.lean, the task check's one home, so
    // validate_post rejects it before ever running anything.
    director_commit_on_branch(
        repo.path(),
        "task/nocheck",
        "tasks/nocheck/prompt.md",
        "do the thing\n",
        "post: nocheck",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(
        report.rejected[0].message.contains("architecture/Tests/nocheck.lean has no check"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(
        hub.rev_parse("refs/heads/task/nocheck").as_deref(),
        Some(tip_before.as_str())
    );
}

#[test]
fn plan_with_a_check_in_task_toml_is_rejected_with_plan() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    // The task check lives only in architecture/Tests/<slug>.lean.
    director_commit_on_branch(
        repo.path(),
        "task/dup",
        "tasks/dup/task.toml",
        "title = \"Dup\"\nphase = \"p1\"\nmodules = [\"app\"]\n\n[check]\nname = \"dup-check\"\ncmd = \"true\"\n",
        "plan: dup",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(
        report.rejected[0].message.contains("architecture/Tests/<slug>.lean"),
        "{}",
        report.rejected[0].message
    );
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
    director_commit_on_branch(
        repo.path(),
        "task/a",
        "tasks/a/task.toml",
        "title = \"A\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: a",
    );
    director_commit_on_branch(
        repo.path(),
        "task/b",
        "tasks/b/task.toml",
        "title = \"B\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"a\"]\n",
        "plan: b",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 2, "{report}");
    let b_tip_before = hub.rev_parse("refs/heads/task/b").unwrap();

    // Hand-accept A: merge it into hub main with a `Task: a` trailer, purely
    // via plumbing (accept isn't implemented until step 5b).
    let a_tip = hub.rev_parse("refs/heads/task/a").unwrap();
    let main_tip = hub.rev_parse("refs/heads/main").unwrap();
    let tree = hub
        .merge_tree_write(&a_tip, &main_tip)
        .expect("merge-tree should run")
        .expect("no conflicts expected");
    let merge_sha = hub
        .commit_tree(&tree, &[&main_tip, &a_tip], "Merge task/a\n\nTask: a")
        .expect("commit-tree should succeed");
    hub.update_ref_cas("refs/heads/main", &merge_sha, &main_tip)
        .expect("main should fast-forward");

    // B's tip predates the acceptance, so its branch doesn't contain A's
    // merge commit yet: this tick should unblock it.
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.merges.len(), 1, "{report}");
    assert!(report.merges[0].contains("task/b"), "{report}");

    let b_tip_after = hub.rev_parse("refs/heads/task/b").unwrap();
    assert_ne!(b_tip_after, b_tip_before);
    assert!(
        hub.is_ancestor(&merge_sha, &b_tip_after),
        "task/b should now contain A's acceptance merge"
    );

    let note = records::import_note(&hub, &b_tip_after).unwrap();
    assert_eq!(note.transition, Transition::MergeUnblock);
    assert_eq!(note.actor, Actor::Player);

    assert_eq!(task_status(&hub, "b"), Status::Ready);
}

/// A branch unblocked earlier (or with no deps at all) must keep following
/// `main` until it's posted: otherwise a later acceptance can add a pinned
/// test the director's post commit has no way to update (the ai-director
/// safety-schema-field case, 2026-09-27).
#[test]
fn unposted_ready_branch_keeps_picking_up_main() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    // A and C both have no deps, so C is ready from the start and never
    // needed a dependency merge.
    director_commit_on_branch(
        repo.path(),
        "task/a",
        "tasks/a/task.toml",
        "title = \"A\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: a",
    );
    director_commit_on_branch(
        repo.path(),
        "task/c",
        "tasks/c/task.toml",
        "title = \"C\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: c",
    );
    player.tick().expect("tick should succeed");
    let c_tip_before = hub.rev_parse("refs/heads/task/c").unwrap();

    // Hand-accept A (same plumbing as the unblock test above); main moves.
    let a_tip = hub.rev_parse("refs/heads/task/a").unwrap();
    let main_tip = hub.rev_parse("refs/heads/main").unwrap();
    let tree = hub
        .merge_tree_write(&a_tip, &main_tip)
        .expect("merge-tree should run")
        .expect("no conflicts expected");
    let merge_sha = hub
        .commit_tree(&tree, &[&main_tip, &a_tip], "Merge task/a\n\nTask: a")
        .expect("commit-tree should succeed");
    hub.update_ref_cas("refs/heads/main", &merge_sha, &main_tip)
        .expect("main should fast-forward");

    let report = player.tick().expect("tick should succeed");
    assert!(
        report.merges.iter().any(|m| m.contains("task/c")),
        "{report}"
    );
    let c_tip_after = hub.rev_parse("refs/heads/task/c").unwrap();
    assert_ne!(c_tip_after, c_tip_before);
    assert!(
        hub.is_ancestor(&merge_sha, &c_tip_after),
        "task/c should now contain main"
    );
    assert_eq!(
        records::import_note(&hub, &c_tip_after).unwrap().transition,
        Transition::MergeUnblock
    );
    assert_eq!(task_status(&hub, "c"), Status::Ready);

    // Idempotent: nothing new on main, so no further merge.
    let report = player.tick().expect("tick should succeed");
    assert!(
        !report.merges.iter().any(|m| m.contains("task/c")),
        "{report}"
    );
}

// --------------------------------------------------------------------------
// CLI
// --------------------------------------------------------------------------

#[test]
fn player_once_via_binary_prints_a_report() {
    let repo = director_repo();
    let home_dir = temp_home();
    let report = init::init(
        repo.path(),
        Some("proj"),
        Some(home_dir.path().join("home")),
    )
    .expect("init should succeed");

    let output = run_dashboard(
        &["player", "--once", "--home", report.home.to_str().unwrap()],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no changes"), "unexpected stdout: {stdout}");
}

#[test]
fn a_budget_raise_needs_a_reason_and_a_small_step_and_is_noted_and_shown() {
    use dashboard::{render, state};
    let manifest = |ast: u32| {
        format!("import Dashboard\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\nmodule a where\n  lang = rust\n  paths = [\"a\"]\n  max_ast = {ast}\n")
    };
    let repo = director_repo();
    write_file(repo.path(), "architecture/Architecture.lean", &manifest(100));
    commit(repo.path(), "add architecture");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    let base = repo.git().rev_parse("HEAD").unwrap();

    // No `raise:` reason: ownership.
    write_file(repo.path(), "architecture/Architecture.lean", &manifest(110));
    commit(repo.path(), "make room");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(report.rejected[0].message.contains("raise: <reason>"), "{}", report.rejected[0].message);
    assert_eq!(hub.rev_parse("refs/heads/main").as_deref(), Some(base.as_str()));

    // Past the step cap (+25%): ownership.
    repo.git().run(&["reset", "--hard", &base]).unwrap();
    write_file(repo.path(), "architecture/Architecture.lean", &manifest(126));
    commit(repo.path(), "raise: too much");
    let report = player.tick().expect("tick");
    assert!(report.rejected[0].message.contains("step cap"), "{}", report.rejected[0].message);

    // Lowering needs nothing.
    repo.git().run(&["reset", "--hard", &base]).unwrap();
    write_file(repo.path(), "architecture/Architecture.lean", &manifest(90));
    commit(repo.path(), "tighten");
    let report = player.tick().expect("tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert!(records::budget_raises(&hub).is_empty());

    // A reasoned raise within the cap is imported and noted.
    write_file(repo.path(), "architecture/Architecture.lean", &manifest(110));
    commit(repo.path(), "raise(parser): the parser grew; operator agreed");
    let report = player.tick().expect("tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    let raises = records::budget_raises(&hub);
    assert_eq!(raises.len(), 1);
    assert_eq!((raises[0].budget.as_str(), raises[0].old, raises[0].new), ("module a max_ast", 90, 110));
    assert_eq!(raises[0].task.as_deref(), Some("parser"));

    let st = state::derive(&hub, &|_| false).expect("derive");
    let text = render::render(&st, &[], false, 100);
    assert!(text.contains("Budget raises (1)") && text.contains("module a max_ast raised 1x (90 -> 110)"), "{text}");
}

#[test]
fn main_commit_of_a_lean_architecture_is_exported_cached_and_its_raises_judged() {
    let repo = director_repo();
    write_lean_arch(repo.path(), LEAN_AB_MANIFEST, &[("ab", LEAN_AB_IDL)], &[]);
    commit(repo.path(), "add the lean architecture");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    let base = repo.git().rev_parse("HEAD").unwrap();

    // The hub was made from a main that already had it: the first tick
    // exports and caches it on the package's tree.
    let report = player.tick().expect("tick");
    assert!(report.errors.is_empty(), "{report}");
    let tree = hub.rev_parse(&format!("{base}:architecture")).unwrap();
    assert!(hub.notes_show(records::ARCH_NOTES, &tree).is_some(), "export not cached");
    let arch = records::arch_at(&hub, &base).expect("arch from the cache");
    assert_eq!(arch.modules.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(arch.api_paths(), ["api"]);

    // A raise needs its reason, like the TOML's.
    let raised = LEAN_AB_MANIFEST.replace("max_ast = 100", "max_ast = 110");
    write_file(repo.path(), "architecture/Architecture.lean", &raised);
    commit(repo.path(), "make room");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert!(report.rejected[0].message.contains("raise: <reason>"), "{}", report.rejected[0].message);
    repo.git().run(&["reset", "--hard", &base]).unwrap();
    write_file(repo.path(), "architecture/Architecture.lean", &raised);
    commit(repo.path(), "raise(ab): a grew");
    let report = player.tick().expect("tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    let raises = records::budget_raises(&hub);
    assert_eq!((raises[0].budget.as_str(), raises[0].old, raises[0].new), ("module a max_ast", 100, 110));

    // An API naming an undeclared module doesn't elaborate: ownership, with
    // Lean's message.
    let good = repo.git().rev_parse("HEAD").unwrap();
    write_file(repo.path(), "architecture/Architecture.lean", &raised.replace("api ab : a → b", "api ab : a → nope"));
    commit(repo.path(), "break the architecture");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(report.rejected[0].message.contains("nope"), "{}", report.rejected[0].message);
    assert_eq!(hub.rev_parse("refs/heads/main").as_deref(), Some(good.as_str()));
}

#[test]
fn dashboard_raise_commits_only_a_budget_raise_on_main() {
    let repo = director_repo();
    write_lean_arch(repo.path(), LEAN_AB_MANIFEST, &[("ab", LEAN_AB_IDL)], &[]);
    commit(repo.path(), "add the lean architecture");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    player.tick().expect("tick");
    let base = repo.git().rev_parse("HEAD").unwrap();
    let scratch = TempDir::new().unwrap();
    let exporter = dashboard::lean::Exporter::new(scratch.path().to_path_buf(), dashboard::sandbox::Sandbox::off());
    let raise = |slug: Option<&str>, reason: &str| dashboard::marker::raise(&hub, &repo.git(), slug, reason, &exporter);
    let arch = "architecture/Architecture.lean";

    let err = raise(None, "x").unwrap_err();
    assert!(err.contains("nothing to raise"), "{err}");
    // Past the step cap, with another file, or raising nothing: refused
    // with the player's words, and nothing moves.
    write_file(repo.path(), arch, &LEAN_AB_MANIFEST.replace("max_ast = 100", "max_ast = 200"));
    let err = raise(Some("ab"), "a grew").unwrap_err();
    assert!(err.contains("step cap"), "{err}");
    write_file(repo.path(), arch, &LEAN_AB_MANIFEST.replace("max_ast = 100", "max_ast = 110"));
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(repo.path(), "roadmap.toml", &format!("{roadmap}\n# more\n"));
    let err = raise(Some("ab"), "a grew").unwrap_err();
    assert!(err.contains("not \"roadmap.toml\""), "{err}");
    write_file(repo.path(), "roadmap.toml", &roadmap);
    write_file(repo.path(), arch, &LEAN_AB_MANIFEST.replace("max_ast = 100", "max_ast = 90"));
    let err = raise(None, "x").unwrap_err();
    assert!(err.contains("raise no budget"), "{err}");
    assert_eq!(repo.git().rev_parse("refs/heads/main").as_deref(), Some(base.as_str()));

    write_file(repo.path(), arch, &LEAN_AB_MANIFEST.replace("max_ast = 100", "max_ast = 110"));
    let out = raise(Some("ab"), "a grew\nthe operator agreed").expect("raise");
    assert_eq!(out.raises.len(), 1);
    assert_eq!(repo.git().rev_parse("refs/heads/main").as_deref(), Some(out.commit.as_str()));
    assert_eq!(repo.git().run(&["status", "--porcelain"]).unwrap(), "", "index and tree at the raise");
    assert_eq!(repo.git().commit_subject(&out.commit).as_deref(), Some("raise(ab): a grew"));
    let report = player.tick().expect("tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    let raises = records::budget_raises(&hub);
    assert_eq!((raises[0].budget.as_str(), raises[0].old, raises[0].new), ("module a max_ast", 100, 110));
    assert_eq!(raises[0].task.as_deref(), Some("ab"));
    assert_eq!(raises[0].reason, "a grew\nthe operator agreed");

    // Only on main at the hub's tip.
    repo.git().run(&["checkout", "-q", "-b", "elsewhere"]).unwrap();
    write_file(repo.path(), arch, &LEAN_AB_MANIFEST.replace("max_ast = 100", "max_ast = 120"));
    let err = raise(None, "x").unwrap_err();
    assert!(err.contains("a raise goes on main"), "{err}");
}
