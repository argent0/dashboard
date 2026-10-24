// Integration tests for the step 5b transitions in src/player.rs (spec.md
// §9): post, send-back, accept, retire, and the marker/ownership dispatch
// on an existing `task/<slug>` branch. Every test builds a real director
// repo, a real hub via `dashboard::init::init`, and drives `Player::tick`
// against them with `NullSessions` (no tmux).
//
// Step 6 (real worker clones, launch, tmux) hasn't landed yet, so wherever
// a test needs a worker's finish claim, it simulates one by hand: pushing
// a commit onto the hub's task branch with plumbing
// (`common::worker_commit`/`worker_empty_commit`) and recording it as
// validated with `records::record_import(.., Actor::Worker, ..)`, exactly
// as tests/state.rs already does for its hand-built hub states.

mod common;

use common::*;
use dashboard::checks;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::player::{NullSessions, Player, Sessions};
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
/// `test -f src/feature.txt` (fails until a "worker" creates that file) and
/// the matching check in `architecture/Tests/<slug>.lean`. Ticks and asserts the post was
/// imported.
fn post(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) {
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
}

/// Simulates the worker finishing `slug`: pushes a commit adding
/// `src/feature.txt` straight onto the hub's task branch and records it as a
/// validated finish claim (step 6 isn't implemented yet — see the module
/// doc comment).
fn finish(home: &Home, hub: &Git, slug: &str) -> String {
    let sha = worker_commit(&home.hub(), slug, "src/feature.txt", "done\n", "finish work");
    records::record_import(hub, &sha, Actor::Worker, Transition::Finish, Some(slug))
        .expect("record finish import");
    sha
}

/// plan -> post -> finish, asserting `tested` at the end. Returns the finish
/// commit's sha.
fn post_and_finish(
    repo: &DirectorRepo,
    hub: &Git,
    home: &Home,
    player: &mut Player<NullSessions>,
    slug: &str,
) -> String {
    plan(
        repo,
        player,
        slug,
        &format!("title = \"{slug}\"\nphase = \"p1\"\nmodules = [\"app\"]\n"),
    );
    post(repo, player, slug);
    let sha = finish(home, hub, slug);
    assert_eq!(
        task_status(hub, slug),
        Status::Tested,
        "task/{slug} should be tested after finish"
    );
    sha
}

/// Hand-accepts `slug` directly in the hub via plumbing (no marker commit,
/// no suite run) — used only to set up an already-accepted dependency for a
/// different test's scenario, exactly as tests/player.rs's unblock-merge
/// test does.
fn hand_accept(hub: &Git, slug: &str) -> String {
    let tip = hub
        .rev_parse(&format!("refs/heads/task/{slug}"))
        .expect("task branch exists");
    let main_tip = hub.rev_parse("refs/heads/main").expect("hub has main");
    let tree = hub
        .merge_tree_write(&tip, &main_tip)
        .expect("merge-tree should run")
        .expect("no conflicts expected");
    let merge_sha = hub
        .commit_tree(
            &tree,
            &[&main_tip, &tip],
            &format!("Merge task/{slug}\n\nTask: {slug}"),
        )
        .expect("commit-tree should succeed");
    hub.update_ref_cas("refs/heads/main", &merge_sha, &main_tip)
        .expect("main should fast-forward");
    merge_sha
}

// --------------------------------------------------------------------------
// Post
// --------------------------------------------------------------------------

#[test]
fn post_happy_path() {
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
    assert_eq!(task_status(&hub, "foo"), Status::Ready);
    post(&repo, &mut player, "foo");
    assert_eq!(task_status(&hub, "foo"), Status::Posted);
}

#[test]
fn post_rejected_when_the_task_check_already_passes() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "eager",
        "title = \"Eager\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );

    checkout_branch(repo.path(), "task/eager");
    write_file(repo.path(), "tasks/eager/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/eager/task.toml",
        "title = \"Eager\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(
        repo.path(),
        &*tests_file("eager"),
        &*task_suite_lean("[[check]]\nname = \"eager-check\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    // The director's mistake: the file already exists, so the check already
    // passes at post — it must fail (spec §3.1).
    write_file(repo.path(), "src/feature.txt", "oops, already done\n");
    commit(repo.path(), "post: eager (check already passes)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(
        report.rejected[0].message.contains("already passes"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "eager"), Status::Ready);
}

#[test]
fn post_rejected_when_dependencies_are_not_accepted() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(&repo, &mut player, "a", "title = \"A\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    plan(
        &repo,
        &mut player,
        "b",
        "title = \"B\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"a\"]\n",
    );
    assert_eq!(task_status(&hub, "b"), Status::Pending);

    checkout_branch(repo.path(), "task/b");
    write_file(repo.path(), "tasks/b/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/b/task.toml",
        "title = \"B\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"a\"]\n",
    );
    write_file(
        repo.path(),
        &*tests_file("b"),
        &*task_suite_lean("[[check]]\nname = \"b-check\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    commit(repo.path(), "post: b (a not accepted yet)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(
        report.rejected[0]
            .message
            .contains("dependencies not accepted"),
        "{}",
        report.rejected[0].message
    );
    assert!(
        report.rejected[0].message.contains('a'),
        "{}",
        report.rejected[0].message
    );
}

#[test]
fn post_rejected_when_the_suite_fragment_is_missing() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "notests",
        "title = \"No tests entry\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );

    checkout_branch(repo.path(), "task/notests");
    write_file(repo.path(), "tasks/notests/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/notests/task.toml",
        "title = \"No tests entry\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    // architecture/Tests/notests.lean deliberately not added.
    commit(repo.path(), "post: notests (forgot its Tests file)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(
        report.rejected[0]
            .message
            .contains("architecture/Tests/notests.lean has no check"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "notests"), Status::Ready);
}

#[test]
fn post_rejected_when_the_task_check_goes_in_tests_lean() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "shared",
        "title = \"Shared\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );

    checkout_branch(repo.path(), "task/shared");
    write_file(repo.path(), "tasks/shared/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/shared/task.toml",
        "title = \"Shared\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    // The old shape: appended to the file every parallel post would edit.
    write_file(
        repo.path(),
        TESTS_LEAN,
        &*suite_lean("[[check]]\nname = \"unit\"\ncmd = \"true\"\n\n[[check]]\nname = \"shared-check\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    commit(repo.path(), "post: shared (check in Tests.lean)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(
        report.rejected[0].message.contains("architecture/Tests/shared.lean"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "shared"), Status::Ready);
}

#[test]
fn post_rejected_when_a_check_name_is_declared_twice() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "dup",
        "title = \"Dup\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );

    checkout_branch(repo.path(), "task/dup");
    write_file(repo.path(), "tasks/dup/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/dup/task.toml",
        "title = \"Dup\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(
        repo.path(),
        &*tests_file("dup"),
        &*task_suite_lean("[[check]]\nname = \"unit\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    commit(repo.path(), "post: dup (reuses Tests.lean's check name)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(
        report.rejected[0].message.contains("unit"),
        "{}",
        report.rejected[0].message
    );
}

#[test]
fn post_rejected_when_a_dependency_is_accepted_but_not_yet_merged_into_the_branch() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    // Plan both up front, then hand-accept A purely in the hub (plumbing),
    // so the director's local clone (and B's branch, built on the
    // pre-acceptance main) never sees it — mirroring
    // tests/player.rs's unblock-merge test.
    plan(&repo, &mut player, "a", "title = \"A\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    plan(
        &repo,
        &mut player,
        "b",
        "title = \"B\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"a\"]\n",
    );
    hand_accept(&hub, "a");
    assert_eq!(
        task_status(&hub, "b"),
        Status::Ready,
        "b should be ready as soon as a is accepted, even before any merge"
    );

    checkout_branch(repo.path(), "task/b");
    write_file(repo.path(), "tasks/b/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/b/task.toml",
        "title = \"B\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"a\"]\n",
    );
    write_file(
        repo.path(),
        &*tests_file("b"),
        &*task_suite_lean("[[check]]\nname = \"b-check\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    commit(repo.path(), "post: b (branch never merged a's acceptance)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(
        report.rejected[0].message.contains("merge"),
        "{}",
        report.rejected[0].message
    );
}

// --------------------------------------------------------------------------
// Send-back
// --------------------------------------------------------------------------

#[test]
fn send_back_happy_path() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "x");
    director_pull_task_branch(repo.path(), "x");

    // Tighten the task check so the suite now fails: real evidence for the
    // send-back (spec §3.1).
    write_file(
        repo.path(),
        &*tests_file("x"),
        &*task_suite_lean("[[check]]\nname = \"x-check\"\ncmd = \"test -f src/feature.txt && test -f src/feature2.txt\"\n"),
    );
    write_file(
        repo.path(),
        "tasks/x/reviews/r1.md",
        "Please also add src/feature2.txt\n",
    );
    commit(repo.path(), "send-back: x r1");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::SendBack);
    assert_eq!(task_status(&hub, "x"), Status::InRevision);

    let sent = &player.sessions().sent;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "x");
    assert!(sent[0].1.contains("Review round 1"), "{}", sent[0].1);
    assert!(
        sent[0].1.contains("Please also add src/feature2.txt"),
        "{}",
        sent[0].1
    );

    let candidate = hub.rev_parse("refs/heads/task/x").unwrap();
    assert_eq!(
        hub.rev_parse("refs/dashboard/pins/x").as_deref(),
        Some(candidate.as_str())
    );
}

#[test]
fn send_back_rejected_when_the_suite_still_passes() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "y");
    director_pull_task_branch(repo.path(), "y");

    // A review note with no new evidence: the suite still passes.
    write_file(
        repo.path(),
        "tasks/y/reviews/r1.md",
        "I don't like this, but I have no failing test.\n",
    );
    commit(repo.path(), "send-back: y r1 (no evidence)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Review);
    assert!(
        report.rejected[0].message.contains("still passes"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "y"), Status::Tested);
}

#[test]
fn send_back_rejected_with_the_wrong_round_number() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "z");
    director_pull_task_branch(repo.path(), "z");

    // Round should be r1 (review_round is 0), not r2.
    write_file(
        repo.path(),
        &*tests_file("z"),
        &*task_suite_lean("[[check]]\nname = \"z-check\"\ncmd = \"test -f src/feature.txt && test -f src/feature2.txt\"\n"),
    );
    write_file(repo.path(), "tasks/z/reviews/r2.md", "skipping ahead\n");
    commit(repo.path(), "send-back: z r2 (wrong round)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Review);
    assert!(
        report.rejected[0].message.contains("round"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "z"), Status::Tested);
}

#[test]
fn send_back_rejected_when_touching_a_non_test_path() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "w");
    director_pull_task_branch(repo.path(), "w");

    write_file(
        repo.path(),
        &*tests_file("w"),
        &*task_suite_lean("[[check]]\nname = \"w-check\"\ncmd = \"test -f src/feature.txt && test -f src/feature2.txt\"\n"),
    );
    write_file(
        repo.path(),
        "tasks/w/reviews/r1.md",
        "also fix this other file\n",
    );
    write_file(repo.path(), "src/unrelated.rs", "// not a test path\n");
    commit(repo.path(), "send-back: w r1 (touches src/unrelated.rs)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Review);
    assert!(
        report.rejected[0].message.contains("src/unrelated.rs"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "w"), Status::Tested);
}

#[test]
fn send_back_exceeding_max_review_rounds_escalates_instead_of_importing() {
    let repo = director_repo();
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!(
            "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n\n[limits]\nmax_review_rounds = 1\n{NO_FOREMEN}"
        ),
    );
    commit(repo.path(), "set max_review_rounds = 1");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "v");

    // Round 1: within the limit, imports normally.
    director_pull_task_branch(repo.path(), "v");
    write_file(
        repo.path(),
        &*tests_file("v"),
        &*task_suite_lean("[[check]]\nname = \"v-check\"\ncmd = \"test -f src/feature.txt && test -f src/feature2.txt\"\n"),
    );
    write_file(
        repo.path(),
        "tasks/v/reviews/r1.md",
        "add src/feature2.txt too\n",
    );
    commit(repo.path(), "send-back: v r1");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(task_status(&hub, "v"), Status::InRevision);

    // The worker fixes it and finishes again.
    let finish2 = worker_commit(
        &home.hub(),
        "v",
        "src/feature2.txt",
        "done\n",
        "finish work round 2",
    );
    records::record_import(&hub, &finish2, Actor::Worker, Transition::Finish, Some("v")).unwrap();
    assert_eq!(task_status(&hub, "v"), Status::Tested);

    // Round 2 exceeds max_review_rounds = 1.
    director_pull_task_branch(repo.path(), "v");
    write_file(repo.path(), "tasks/v/reviews/r2.md", "one more round\n");
    commit(repo.path(), "send-back: v r2 (exceeds the limit)");
    let tip_before = hub.rev_parse("refs/heads/task/v").unwrap();
    let report = player.tick().expect("tick should succeed");
    assert_eq!(
        report.rejected.len(),
        0,
        "an escalated send-back isn't recorded as a rejection: {report}"
    );
    assert_eq!(report.escalated.len(), 1, "{report}");
    assert_eq!(report.escalated[0].0, "v");
    assert!(
        report.escalated[0].1.contains("review rounds exceeded"),
        "{}",
        report.escalated[0].1
    );
    assert_eq!(
        hub.rev_parse("refs/heads/task/v").as_deref(),
        Some(tip_before.as_str()),
        "the offending commit must not be imported"
    );
    assert_eq!(task_status(&hub, "v"), Status::Escalated);
}

// --------------------------------------------------------------------------
// Trim (spec §6.5)
// --------------------------------------------------------------------------

#[test]
fn trim_drops_the_task_check_from_the_suite_and_accept_lands_without_it() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "tr");
    director_pull_task_branch(repo.path(), "tr");
    std::fs::remove_file(repo.path().join(tests_file("tr"))).unwrap();
    commit(repo.path(), "trim: tr's check only drove this task");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Trim);
    assert_eq!(task_status(&hub, "tr"), Status::Tested);
    let trimmed = hub.rev_parse("refs/heads/task/tr").unwrap();
    assert_eq!(
        hub.rev_parse("refs/dashboard/pins/tr").as_deref(),
        Some(trimmed.as_str())
    );
    assert!(player.sessions().sent.is_empty(), "a trim relays nothing");

    commit_empty(repo.path(), "accept: looks good");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(task_status(&hub, "tr"), Status::Accepted, "{report}");
    let main_tip = hub.rev_parse("refs/heads/main").unwrap();
    assert!(hub.blob_hash(&main_tip, &tests_file("tr")).is_none());
    assert!(hub.blob_hash(&main_tip, "tasks/tr/task.toml").is_some());
}

#[test]
fn trim_rejected_when_touching_a_file_already_on_main() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "tm");
    director_pull_task_branch(repo.path(), "tm");
    write_file(repo.path(), TESTS_LEAN, &*suite_lean("[[check]]\nname = \"unit\"\ncmd = \"true\"\nruns = 2\n"));
    commit(repo.path(), "trim: edits the project suite");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Accept);
    assert!(
        report.rejected[0].message.contains("Tests.lean"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "tm"), Status::Tested);
}

#[test]
fn trim_rejected_when_the_fragment_gains_a_check() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "tg");
    director_pull_task_branch(repo.path(), "tg");
    write_file(
        repo.path(),
        &*tests_file("tg"),
        &*task_suite_lean("[[check]]\nname = \"tg-check\"\ncmd = \"true\"\n"),
    );
    commit(repo.path(), "trim: weakens the check instead");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Accept);
    assert!(
        report.rejected[0].message.contains("tg-check"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "tg"), Status::Tested);
}

// --------------------------------------------------------------------------
// Accept
// --------------------------------------------------------------------------

#[test]
fn accept_happy_path() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "acc");
    director_pull_task_branch(repo.path(), "acc");
    commit_empty(repo.path(), "accept: looks good");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 2, "{report}"); // the accept marker, then the merge_accept
    assert_eq!(report.imported[0].2, Transition::Accept);
    assert_eq!(report.imported[1].2, Transition::MergeAccept);
    assert_eq!(task_status(&hub, "acc"), Status::Accepted);

    let main_tip = hub.rev_parse("refs/heads/main").unwrap();
    let accepted = records::accepted_tasks(&hub, &main_tip);
    assert_eq!(accepted.get("acc"), Some(&main_tip));

    assert_eq!(player.sessions().killed, vec!["acc".to_string()]);
    assert!(
        !checks::cached(&hub, &main_tip).is_empty(),
        "the suite should have run on the merge commit itself"
    );
}

#[test]
fn accept_rejected_when_not_tested() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "early",
        "title = \"Early\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "early");
    // Still just posted — no finish claim yet.
    commit_empty(repo.path(), "accept: too soon");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Accept);
    assert_eq!(task_status(&hub, "early"), Status::Posted);
}

#[test]
fn accept_stops_at_a_conflict_with_a_main_that_moved() {
    let repo = director_repo();
    // docs_paths includes docs/, so the director can change it from main
    // directly (an ordinary, allowed main-only commit) on the same file the
    // worker's finish writes.
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!(
            "title = \"Test Project\"\ndocs_paths = [\"docs\"]\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n{NO_FOREMEN}"
        ),
    );
    commit(repo.path(), "declare docs_paths over docs/");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "conflicted",
        "title = \"conflicted\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "conflicted");
    worker_commit(
        &home.hub(),
        "conflicted",
        "docs/notes.md",
        "the worker's notes\n",
        "notes",
    );
    finish(&home, &hub, "conflicted");
    assert_eq!(task_status(&hub, "conflicted"), Status::Tested);

    // Main writes the same file differently.
    checkout_branch(repo.path(), "main");
    write_file(repo.path(), "docs/notes.md", "the director's notes\n");
    commit(repo.path(), "docs: notes");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 0, "{report}");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Docs);

    director_pull_task_branch(repo.path(), "conflicted");
    commit_empty(repo.path(), "accept: conflicted");

    // Both sides wrote docs/notes.md: the player can't merge main in, so the
    // accept stops (no strike) for the director to resolve (spec §11).
    let report = player.tick().expect("tick should succeed");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.conflicts.len(), 1, "{report}");
    assert_eq!(task_status(&hub, "conflicted"), Status::Conflicted);
}

// --------------------------------------------------------------------------
// Retire
// --------------------------------------------------------------------------

#[test]
fn retire_of_a_pending_task_moves_the_branch_and_shows_the_post_mortem() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "deadend",
        "title = \"Dead end\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    commit_empty(
        repo.path(),
        "retire: deadend\n\nThe approach doesn't work; replanning.",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Retire);
    assert!(hub.ref_exists("refs/retired/deadend"));
    assert!(!hub.ref_exists("refs/heads/task/deadend"));
    assert_eq!(task_status(&hub, "deadend"), Status::Retired);

    let hub_state = state::derive(&hub, &|_| false).unwrap();
    let task = hub_state
        .tasks
        .iter()
        .find(|t| t.slug == "deadend")
        .unwrap();
    assert!(
        task.post_mortem
            .as_deref()
            .unwrap_or("")
            .contains("doesn't work"),
        "{:?}",
        task.post_mortem
    );
}

#[test]
fn retire_of_an_escalated_task_works() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "blocked",
        "title = \"Blocked\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "blocked");
    let block_sha = worker_empty_commit(
        &home.hub(),
        "blocked",
        "block: this can't be done as specified",
    );
    records::record_import(
        &hub,
        &block_sha,
        Actor::Worker,
        Transition::Block,
        Some("blocked"),
    )
    .unwrap();
    assert_eq!(task_status(&hub, "blocked"), Status::Escalated);

    director_pull_task_branch(repo.path(), "blocked");
    commit_empty(
        repo.path(),
        "retire: blocked\n\nGiving up on this approach.",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Retire);
    assert!(hub.ref_exists("refs/retired/blocked"));
    assert_eq!(task_status(&hub, "blocked"), Status::Retired);
}

#[test]
fn retire_of_a_tested_task_is_rejected() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    post_and_finish(&repo, &hub, &home, &mut player, "solid");
    director_pull_task_branch(repo.path(), "solid");
    commit_empty(repo.path(), "retire: solid (shouldn't be allowed)");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Retire);
    assert!(hub.ref_exists("refs/heads/task/solid"));
    assert_eq!(task_status(&hub, "solid"), Status::Tested);
}

// --------------------------------------------------------------------------
// Ownership fallback: markers and commits during states the director may
// not touch
// --------------------------------------------------------------------------

#[test]
fn director_commit_while_posted_is_rejected_with_ownership() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "midflight",
        "title = \"Mid flight\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "midflight");
    // `post`'s tick also launches the worker (step 6): `NullSessions`
    // marks the session alive synchronously, exactly as a real `tmux
    // has-session` would right after `tmux new-session`. The player's own
    // status derivation (used below in the rejection message) sees this and
    // reports `in_progress`; `task_status` here hardcodes every session as
    // dead (it's shared by tests that predate step 6), so it still reports
    // `posted` for the exact same hub state.
    assert!(
        player.sessions().alive("midflight"),
        "post should have launched a worker session"
    );

    checkout_branch(repo.path(), "task/midflight");
    write_file(
        repo.path(),
        "tasks/midflight/prompt.md",
        "changed my mind\n",
    );
    commit(repo.path(), "meddling with a posted task");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(
        report.rejected[0].message.contains("in_progress"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task_status(&hub, "midflight"), Status::Posted);
}

#[test]
fn director_block_marker_is_rejected_with_ownership() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "notmine",
        "title = \"Not mine to block\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    let tip_before = hub.rev_parse("refs/heads/task/notmine").unwrap();
    commit_empty(repo.path(), "block: the director may not write this");

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert_eq!(
        hub.rev_parse("refs/heads/task/notmine").as_deref(),
        Some(tip_before.as_str())
    );
}

#[test]
fn empty_marker_on_top_of_a_rejected_non_empty_commit_is_ownership() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "stack",
        "title = \"Stack\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );

    checkout_branch(repo.path(), "task/stack");
    write_file(repo.path(), "tasks/stack/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/stack/task.toml",
        "title = \"Stack\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(
        repo.path(),
        &*tests_file("stack"),
        &*task_suite_lean("[[check]]\nname = \"stack-check\"\ncmd = \"true\"\n"),
    );
    // The task check trivially always passes — this post will be rejected.
    commit(repo.path(), "post: stack (bad, check already passes)");
    commit_empty(
        repo.path(),
        "accept: stack (marker stacked on the rejected post)",
    );

    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 2, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert_eq!(report.rejected[1].reason, Reason::Ownership);
    assert_eq!(task_status(&hub, "stack"), Status::Ready);
}

// --------------------------------------------------------------------------
// Strikes -> escalation
// --------------------------------------------------------------------------

#[test]
fn two_post_rejections_escalate_the_task() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "strikeout",
        "title = \"Strikeout\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );

    checkout_branch(repo.path(), "task/strikeout");
    write_file(repo.path(), "tasks/strikeout/prompt.md", "attempt 1\n");
    write_file(
        repo.path(),
        "tasks/strikeout/task.toml",
        "title = \"Strikeout\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(
        repo.path(),
        &*tests_file("strikeout"),
        &*task_suite_lean("[[check]]\nname = \"strikeout-check\"\ncmd = \"true\"\n"),
    );
    commit(repo.path(), "post: strikeout attempt 1 (always passes)");
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert_eq!(
        task_status(&hub, "strikeout"),
        Status::Ready,
        "one strike is below the default limit of 2"
    );

    write_file(repo.path(), "tasks/strikeout/prompt.md", "attempt 2\n");
    commit(
        repo.path(),
        "post: strikeout attempt 2 (still always passes)",
    );
    let report = player.tick().expect("tick should succeed");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert_eq!(task_status(&hub, "strikeout"), Status::Escalated);
}

// --------------------------------------------------------------------------
// modules (spec §8.2)
// --------------------------------------------------------------------------

/// Commits `task_toml` as the plan of `slug` and ticks once.
fn plan_once(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str, task_toml: &str) -> dashboard::player::TickReport {
    director_commit_on_branch(
        repo.path(),
        &format!("task/{slug}"),
        &format!("tasks/{slug}/task.toml"),
        task_toml,
        &format!("plan: {slug}"),
    );
    player.tick().expect("tick should succeed")
}

#[test]
fn a_plan_names_modules_main_declares() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    let report = plan_once(&repo, &mut player, "bare", "title = \"Bare\"\nphase = \"p1\"\n");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(report.rejected[0].message.contains("modules"), "{}", report.rejected[0].message);
    assert!(!hub.ref_exists("refs/heads/task/bare"));

    let report = plan_once(&repo, &mut player, "ghost", "title = \"Ghost\"\nphase = \"p1\"\nmodules = [\"app\", \"ghost\"]\n");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert!(report.rejected[0].message.contains("\"ghost\""), "{}", report.rejected[0].message);
    assert!(!hub.ref_exists("refs/heads/task/ghost"));

    // A spike changes no code: it names no modules.
    let report = plan_once(&repo, &mut player, "probe", "kind = \"spike\"\ntitle = \"Probe\"\nphase = \"p1\"\n");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(task_status(&hub, "probe"), Status::Ready);

    // The run set of a plan is noted on main's architecture tree.
    let report = plan_once(&repo, &mut player, "ok", "title = \"Ok\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    assert!(report.rejected.is_empty(), "{report}");
    let tree = hub.rev_parse("refs/heads/main:architecture").unwrap();
    let note = hub.notes_show(records::RUNSETS_NOTES, &tree).expect("run set noted");
    assert_eq!(note.trim(), "{\"app\":[\"unit\"]}");
}

#[test]
fn a_send_back_stays_inside_the_task_scope() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    post_and_finish(&repo, &hub, &home, &mut player, "sb");
    director_pull_task_branch(repo.path(), "sb");
    write_file(repo.path(), "tasks/sb/reviews/r1.md", "more\n");
    write_file(repo.path(), "architecture/Architecture.lean", &format!("{STARTER_MANIFEST}-- note\n"));
    commit(repo.path(), "send-back: sb r1");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Review, "{report}");
    assert!(report.rejected[0].message.contains("architecture/Architecture.lean"), "{report}");
}
