// Integration tests for src/state.rs (derived status, spec.md §10, §12) and
// the record writers/readers in src/records.rs that back it. Every test
// builds a hub by hand: a director repo plus `dashboard::init::init`, task
// branches pushed into the hub with plain git plumbing, record writers
// called directly, and `Task: <slug>` merges written with `git merge`
// ourselves — there's no player yet (that's steps 5-6), so nothing here goes
// through transition validation.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::records::{self, Actor, Transition};
use dashboard::state::{self, Status};
use std::path::Path;
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

/// Force-pushes local ref `refname` (e.g. `refs/heads/task/foo`) to the same
/// name in the hub. Force, not fast-forward-only: these tests build hub
/// states directly with plumbing, not through the player's history rules
/// (spec §5), which is a later step's job.
fn push_ref(repo: &Path, refname: &str) {
    Git::new(repo)
        .run(&["push", "-q", "-f", "hub", &format!("{refname}:{refname}")])
        .expect("push ref to hub");
}

/// Pushes a specific commit (not necessarily a local branch tip) to an
/// arbitrary ref in the hub — used to land an "offending" commit as an
/// object in the hub without ever importing it onto a task branch.
fn push_sha_to_ref(repo: &Path, sha: &str, dest_ref: &str) {
    Git::new(repo)
        .run(&["push", "-q", "-f", "hub", &format!("{sha}:{dest_ref}")])
        .expect("push sha to ref");
}

fn checkout_new_branch(repo: &Path, branch: &str) {
    Git::new(repo)
        .run(&["checkout", "-q", "-b", branch, "main"])
        .expect("checkout new branch");
}

fn checkout(repo: &Path, branch: &str) {
    Git::new(repo)
        .run(&["checkout", "-q", branch])
        .expect("checkout branch");
}

/// Plans a task on a fresh `task/<slug>` branch off `main`: writes
/// `tasks/<slug>/task.toml`, commits, pushes the branch to the hub, and
/// records the `plan` import note. Leaves the repo checked out on the new
/// branch (further commits stack on top of this one). Returns the plan
/// commit's sha.
fn plan_task(repo: &Path, hub: &Git, slug: &str, task_toml_body: &str) -> String {
    checkout_new_branch(repo, &format!("task/{slug}"));
    write_file(repo, &format!("tasks/{slug}/task.toml"), task_toml_body);
    let sha = commit(repo, &format!("plan: {slug}"));
    push_ref(repo, &format!("refs/heads/task/{slug}"));
    records::record_import(hub, &sha, Actor::Director, Transition::Plan, Some(slug)).unwrap();
    sha
}

fn task_state<'a>(tasks: &'a [state::TaskState], slug: &str) -> &'a state::TaskState {
    tasks
        .iter()
        .find(|t| t.slug == slug)
        .unwrap_or_else(|| panic!("no task state for slug {slug:?}"))
}

// --------------------------------------------------------------------------
// One status per case, derived together from a single hand-built hub
// --------------------------------------------------------------------------

#[test]
fn derive_computes_expected_status_for_every_case() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let path = repo.path();

    // ---- accepted-task: planned, then merged into main with a Task: trailer ----
    plan_task(
        path,
        &hub,
        "accepted-task",
        "title = \"Accepted\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    checkout(path, "main");
    Git::new(path)
        .run(&[
            "merge",
            "-q",
            "--no-ff",
            "task/accepted-task",
            "-m",
            "Merge task/accepted-task\n\nTask: accepted-task",
        ])
        .unwrap();
    push_main_to_hub(path);

    // ---- ready-nodeps: no dependencies at all ----
    plan_task(
        path,
        &hub,
        "ready-nodeps",
        "title = \"Ready no deps\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );

    // ---- ready-dep-accepted: depends on the already-accepted task ----
    plan_task(
        path,
        &hub,
        "ready-dep-accepted",
        "title = \"Ready dep accepted\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"accepted-task\"]\n",
    );

    // ---- pending-dep-unmet: depends on a task that's only ready, not accepted ----
    plan_task(
        path,
        &hub,
        "pending-dep-unmet",
        "title = \"Pending\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"ready-nodeps\"]\n",
    );

    // ---- retired-task: planned, retired, moved under refs/retired/ ----
    plan_task(
        path,
        &hub,
        "retired-task",
        "title = \"Retired\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    let retire_sha = commit_empty(
        path,
        "retire: retired-task\n\nThe approach was a dead end; replanning around it.",
    );
    push_ref(path, "refs/heads/task/retired-task");
    records::record_import(
        &hub,
        &retire_sha,
        Actor::Director,
        Transition::Retire,
        Some("retired-task"),
    )
    .unwrap();
    hub.update_ref("refs/retired/retired-task", &retire_sha)
        .unwrap();
    hub.delete_ref("refs/heads/task/retired-task").unwrap();

    // ---- orphaned-dep-retired: depends on the now-retired task ----
    plan_task(
        path,
        &hub,
        "orphaned-dep-retired",
        "title = \"Orphaned\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"retired-task\"]\n",
    );

    // ---- posted-task: posted, no live worker session yet ----
    plan_task(
        path,
        &hub,
        "posted-task",
        "title = \"Posted\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(path, "tasks/posted-task/prompt.md", "do the thing\n");
    let post_sha = commit(path, "post: posted-task");
    push_ref(path, "refs/heads/task/posted-task");
    records::record_import(
        &hub,
        &post_sha,
        Actor::Director,
        Transition::Post,
        Some("posted-task"),
    )
    .unwrap();

    // ---- tested-task: worker finish claim validated ----
    plan_task(
        path,
        &hub,
        "tested-task",
        "title = \"Tested\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(path, "tasks/tested-task/prompt.md", "do it\n");
    commit(path, "post: tested-task");
    write_file(path, "src/thing.txt", "done\n");
    let finish_sha = commit(path, "finish work");
    push_ref(path, "refs/heads/task/tested-task");
    records::record_import(
        &hub,
        &finish_sha,
        Actor::Worker,
        Transition::Finish,
        Some("tested-task"),
    )
    .unwrap();

    // ---- revision-task: sent back twice; tip is round 2's send-back ----
    plan_task(
        path,
        &hub,
        "revision-task",
        "title = \"Revision\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(path, "tasks/revision-task/prompt.md", "do it\n");
    commit(path, "post: revision-task");
    write_file(path, "tasks/revision-task/reviews/r1.md", "round 1 notes\n");
    commit(path, "send-back: revision-task r1");
    write_file(path, "tasks/revision-task/reviews/r2.md", "round 2 notes\n");
    let sendback_sha = commit(path, "send-back: revision-task r2");
    push_ref(path, "refs/heads/task/revision-task");
    records::record_import(
        &hub,
        &sendback_sha,
        Actor::Director,
        Transition::SendBack,
        Some("revision-task"),
    )
    .unwrap();

    // ---- escalated-strikes: two `suite` rejections hit the default limit of 2 ----
    let strikes_tip1 = plan_task(
        path,
        &hub,
        "escalated-strikes",
        "title = \"Escalated strikes\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    records::record_rejection(
        &hub,
        "escalated-strikes",
        &strikes_tip1,
        Reason::Suite,
        Actor::Worker,
        "suite failed",
    )
    .unwrap();
    write_file(path, "attempt.txt", "second finish attempt\n");
    let strikes_tip2 = commit(path, "another finish attempt");
    push_ref(path, "refs/heads/task/escalated-strikes");
    records::record_rejection(
        &hub,
        "escalated-strikes",
        &strikes_tip2,
        Reason::Suite,
        Actor::Worker,
        "suite failed again",
    )
    .unwrap();

    // ---- escalated-block: worker blocks ----
    plan_task(
        path,
        &hub,
        "escalated-block",
        "title = \"Escalated block\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    let block_sha = commit_empty(
        path,
        "block: escalated-block\n\nThe pinned test conflicts with the new requirement.",
    );
    push_ref(path, "refs/heads/task/escalated-block");
    records::record_import(
        &hub,
        &block_sha,
        Actor::Worker,
        Transition::Block,
        Some("escalated-block"),
    )
    .unwrap();

    // ---- escalated-record: escalation record independent of the tip's own note ----
    plan_task(
        path,
        &hub,
        "escalated-record",
        "title = \"Escalated record\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(path, "tasks/escalated-record/prompt.md", "do it\n");
    let post_sha2 = commit(path, "post: escalated-record");
    push_ref(path, "refs/heads/task/escalated-record");
    records::record_import(
        &hub,
        &post_sha2,
        Actor::Director,
        Transition::Post,
        Some("escalated-record"),
    )
    .unwrap();
    // The offending commit (a send-back that would exceed max_review_rounds)
    // lives in the hub as an object but is never imported onto the branch.
    checkout_new_branch(path, "scratch/escalated-record");
    write_file(path, "scratch.txt", "would-be third send-back\n");
    let offending_sha = commit(path, "attempted third send-back");
    push_sha_to_ref(
        path,
        &offending_sha,
        "refs/dashboard/scratch/escalated-record",
    );
    records::record_escalation(
        &hub,
        "escalated-record",
        &offending_sha,
        "would exceed max_review_rounds",
    )
    .unwrap();

    // ---- derive, once with no live sessions ----
    let not_live = state::derive(&hub, &|_slug| false).expect("derive should succeed");

    assert_eq!(
        task_state(&not_live.tasks, "accepted-task").status,
        Status::Accepted
    );
    assert_eq!(
        task_state(&not_live.tasks, "ready-nodeps").status,
        Status::Ready
    );
    assert_eq!(
        task_state(&not_live.tasks, "ready-dep-accepted").status,
        Status::Ready
    );
    assert_eq!(
        task_state(&not_live.tasks, "pending-dep-unmet").status,
        Status::Pending
    );

    let retired = task_state(&not_live.tasks, "retired-task");
    assert_eq!(retired.status, Status::Retired);
    assert!(
        retired
            .post_mortem
            .as_deref()
            .unwrap_or("")
            .contains("dead end"),
        "{:?}",
        retired.post_mortem
    );

    assert_eq!(
        task_state(&not_live.tasks, "orphaned-dep-retired").status,
        Status::Orphaned
    );
    assert_eq!(
        task_state(&not_live.tasks, "posted-task").status,
        Status::Posted
    );
    assert_eq!(
        task_state(&not_live.tasks, "tested-task").status,
        Status::Tested
    );

    let revision = task_state(&not_live.tasks, "revision-task");
    assert_eq!(revision.status, Status::InRevision);
    assert_eq!(revision.review_round, 2);

    let strikes = task_state(&not_live.tasks, "escalated-strikes");
    assert_eq!(strikes.status, Status::Escalated);
    assert_eq!(strikes.strikes.get(&Reason::Suite), Some(&2));
    assert_eq!(
        strikes.escalation.as_deref(),
        Some("strike limit: suite 2/2")
    );

    let block = task_state(&not_live.tasks, "escalated-block");
    assert_eq!(block.status, Status::Escalated);
    let block_cause = block.escalation.as_deref().unwrap_or("");
    assert!(block_cause.starts_with("blocked:"), "{block_cause}");
    assert!(
        block_cause.contains("pinned test conflicts"),
        "{block_cause}"
    );

    let record = task_state(&not_live.tasks, "escalated-record");
    assert_eq!(record.status, Status::Escalated);
    assert_eq!(
        record.escalation.as_deref(),
        Some("would exceed max_review_rounds")
    );

    // Task list order: same phase for everything here, so alphabetical by slug.
    let slugs: Vec<&str> = not_live.tasks.iter().map(|t| t.slug.as_str()).collect();
    let mut sorted = slugs.clone();
    sorted.sort();
    assert_eq!(slugs, sorted, "same-phase tasks should be ordered by slug");

    // ---- derive again, with a live session for posted-task only ----
    let live = state::derive(&hub, &|slug| slug == "posted-task").expect("derive should succeed");
    assert_eq!(
        task_state(&live.tasks, "posted-task").status,
        Status::InProgress
    );
    // Unrelated tasks are unaffected by live_session.
    assert_eq!(
        task_state(&live.tasks, "tested-task").status,
        Status::Tested
    );
}

// --------------------------------------------------------------------------
// Strikes: per-reason counting, `_main` rejections excluded
// --------------------------------------------------------------------------

#[test]
fn strikes_are_counted_per_reason_and_main_rejections_are_excluded() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let path = repo.path();

    let plan_sha = plan_task(
        path,
        &hub,
        "strikey",
        "title = \"Strikey\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(path, "attempt-1.txt", "first finish attempt\n");
    let attempt_1 = commit(path, "finish attempt 1");
    write_file(path, "attempt-2.txt", "second finish attempt\n");
    let attempt_2 = commit(path, "finish attempt 2");
    push_ref(path, "refs/heads/task/strikey");

    // Each rejection targets a distinct commit — a git note is one-per-commit,
    // so the same commit can't carry two independent rejection verdicts.
    records::record_rejection(
        &hub,
        "strikey",
        &attempt_1,
        Reason::Pins,
        Actor::Worker,
        "pin violated",
    )
    .unwrap();
    records::record_rejection(
        &hub,
        "strikey",
        &attempt_2,
        Reason::History,
        Actor::Worker,
        "force pushed",
    )
    .unwrap();
    // A director commit on main, rejected for a reason that isn't tied to
    // any task: recorded (dashboard shows it), but never a strike (spec §10).
    records::record_rejection(
        &hub,
        "_main",
        &plan_sha,
        Reason::Ownership,
        Actor::Director,
        "touched a forbidden path",
    )
    .unwrap();

    let hub_state = state::derive(&hub, &|_| false).expect("derive should succeed");
    let task = task_state(&hub_state.tasks, "strikey");

    assert_eq!(task.strikes.get(&Reason::Pins), Some(&1));
    assert_eq!(task.strikes.get(&Reason::History), Some(&1));
    assert_eq!(
        task.strikes.len(),
        2,
        "the _main rejection must not show up as a strike on any task"
    );
    assert_eq!(
        task.status,
        Status::Ready,
        "one strike each is below the default limit of 2"
    );

    assert_eq!(
        hub_state.rejections.len(),
        3,
        "the _main rejection is still recorded for rendering"
    );
    assert!(hub_state.rejections.iter().any(|r| r.scope == "_main"));
}

// --------------------------------------------------------------------------
// Pins
// --------------------------------------------------------------------------

#[test]
fn pins_are_recomputed_and_violations_detected() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let path = repo.path();
    let git = Git::new(path);

    // Extend main's Tests.lean with a test_paths entry over a real test tree.
    write_file(
        path,
        TESTS_LEAN,
        &*suite_lean("test_paths = [\"tests/\"]\n\n[[check]]\nname = \"unit\"\ncmd = \"true\"\n"),
    );
    write_file(path, "tests/test_foo.py", "def test_foo(): assert True\n");
    commit(path, "add test_paths");
    push_main_to_hub(path);

    plan_task(
        path,
        &hub,
        "pinned-task",
        "title = \"Pinned\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(path, "tasks/pinned-task/prompt.md", "do it\n");
    write_file(
        path,
        &*tests_file("pinned-task"),
        &*task_suite_lean("[[check]]\nname = \"pinned-task-check\"\ncmd = \"true\"\n"),
    );
    let pin_commit = commit(path, "post: pinned-task");
    push_ref(path, "refs/heads/task/pinned-task");

    records::set_pins(&hub, "pinned-task", &pin_commit).expect("set_pins should succeed");
    // The player exports the package on import; pins read that note.
    let runner = dashboard::checks::Runner::new(&home);
    records::exported_at(&hub, &pin_commit, Some(runner.exporter())).expect("export");
    let pinned = records::pinned_files(&hub, "pinned-task").expect("pins were just set");
    assert!(pinned.contains_key(TESTS_LEAN));
    assert!(pinned.contains_key("tests/test_foo.py"));
    assert!(pinned.contains_key(&tests_file("pinned-task")));
    assert!(pinned.contains_key("tasks/pinned-task/prompt.md"));

    // Unmodified candidate: no violations.
    assert!(records::check_pins(&hub, "pinned-task", &pin_commit).is_empty());

    // Case 1: a pinned test file is modified.
    git.run(&["reset", "-q", "--hard", pin_commit.as_str()])
        .unwrap();
    write_file(
        path,
        "tests/test_foo.py",
        "def test_foo(): assert False  # tampered\n",
    );
    let modified = commit(path, "tamper with pinned test");
    push_ref(path, "refs/heads/task/pinned-task");
    let violations = records::check_pins(&hub, "pinned-task", &modified);
    assert!(
        violations.contains(&"tests/test_foo.py".to_string()),
        "{violations:?}"
    );

    // Case 2: a pinned file is deleted.
    git.run(&["reset", "-q", "--hard", pin_commit.as_str()])
        .unwrap();
    std::fs::remove_file(path.join("tasks/pinned-task/prompt.md")).unwrap();
    let deleted = commit(path, "delete pinned prompt.md");
    push_ref(path, "refs/heads/task/pinned-task");
    let violations = records::check_pins(&hub, "pinned-task", &deleted);
    assert!(
        violations.contains(&"tasks/pinned-task/prompt.md".to_string()),
        "{violations:?}"
    );

    // Case 3: a brand-new file is never a violation.
    git.run(&["reset", "-q", "--hard", pin_commit.as_str()])
        .unwrap();
    write_file(path, "tasks/pinned-task/extra.txt", "new file, fine\n");
    let added = commit(path, "add a new file");
    push_ref(path, "refs/heads/task/pinned-task");
    let violations = records::check_pins(&hub, "pinned-task", &added);
    assert!(
        violations.is_empty(),
        "a new file must not be treated as a pin violation: {violations:?}"
    );
}

#[test]
fn pinned_files_is_none_when_never_set() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let path = repo.path();

    plan_task(
        path,
        &hub,
        "unpinned-task",
        "title = \"Unpinned\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    assert!(records::pinned_files(&hub, "unpinned-task").is_none());
    assert!(records::check_pins(&hub, "unpinned-task", "HEAD").is_empty());
}

// --------------------------------------------------------------------------
// tmux session naming
// --------------------------------------------------------------------------

#[test]
fn tmux_session_name_replaces_dots_and_colons() {
    assert_eq!(
        state::tmux_session_name("my.proj", "slug:with:colons"),
        "dash-my-proj-slug-with-colons"
    );
    assert_eq!(
        state::tmux_session_name("plain", "plain-slug"),
        "dash-plain-plain-slug"
    );
}
