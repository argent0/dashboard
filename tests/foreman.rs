// Integration tests for foremen (spec.md §17): the player launches one
// per foreman-managed task once it's ready, imports its post and send-backs
// from its own clone (`paths::foreman_clone`, branch `foreman/<slug>`) under the
// rules a foreman works by, asks it to review every tested tip, and turns
// the report it leaves into a note and an `foreman_report` event. Every test
// drives `Player::tick` with `NullSessions` and plays the foreman and the
// worker by committing in the clones the player itself created.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::events::{self, EventKind};
use dashboard::git::Git;
use dashboard::init;
use dashboard::marker;
use dashboard::paths::{self, Home};
use dashboard::player::{self, NullSessions, Player, Sessions, TickReport};
use dashboard::records::{self, Actor, Transition};
use dashboard::report;
use dashboard::state::{self, Status};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const PLAN: &str = "title = \"s\"\nphase = \"p1\"\nmodules = [\"app\"]\nbrief = \"mind the widget cache\"\n";

// --------------------------------------------------------------------------
// Fixture helpers
// --------------------------------------------------------------------------

/// A director repo whose roadmap turns foremen on (`foreman_cmd`),
/// with the worker's `agent_cmd` left at its default.
fn repo_with_foremen() -> DirectorRepo {
    let repo = director_repo();
    write_file(
        repo.path(),
        "roadmap.toml",
        "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\ngoal = \"ship the widget\"\n\n[player]\nruns = \"parallel\"\nforeman_cmd = \"fake-foreman\"\n",
    );
    commit(repo.path(), "turn foremen on");
    repo
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

fn task(hub: &Git, slug: &str) -> state::TaskState {
    let hub_state = state::derive(hub, &|_| false).expect("derive should succeed");
    hub_state
        .tasks
        .into_iter()
        .find(|t| t.slug == slug)
        .unwrap_or_else(|| panic!("no task state for {slug:?}"))
}

fn ensure_identity(dir: &Path) {
    let git = Git::new(dir);
    git.run(&["config", "user.name", "Test Foreman"])
        .expect("set user.name");
    git.run(&["config", "user.email", "foreman@example.com"])
        .expect("set user.email");
    git.run(&["config", "commit.gpgsign", "false"])
        .expect("disable gpgsign");
}

fn foreman_dir(repo: &DirectorRepo) -> PathBuf {
    paths::foreman_clone(&repo.director_path(), "s")
}

fn foreman_sent(player: &Player<NullSessions>) -> Vec<String> {
    player
        .sessions()
        .sent
        .iter()
        .filter(|(k, _)| k == "s_foreman")
        .map(|(_, m)| m.clone())
        .collect()
}

/// Plans `s` as the director, stages it and ticks: the task is ready at once
/// (no dependencies) but only the `stage:` launches its foreman.
fn plan(repo: &DirectorRepo, player: &mut Player<NullSessions>) -> TickReport {
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    commit_empty(repo.path(), "stage: go");
    let report = player.tick().expect("plan tick should succeed");
    assert!(report.rejected.is_empty(), "{report}");
    report
}

/// Commits `files` in the foreman's clone, on `foreman/s`.
fn foreman_commit(repo: &DirectorRepo, files: &[(&str, &str)], msg: &str) -> String {
    let dir = foreman_dir(repo);
    ensure_identity(&dir);
    for (rel, content) in files {
        write_file(&dir, rel, content);
    }
    commit(&dir, msg)
}

/// The foreman's post: the prompt and the task check in its suite
/// fragment.
fn foreman_post(repo: &DirectorRepo) -> String {
    foreman_commit(
        repo,
        &[
            (
                "tasks/s/prompt.md",
                "make src/feature.txt; tests you add must be pure\n",
            ),
            (
                &*tests_file("s"),
                &*task_suite_lean("[[check]]\nname = \"s-check\"\ncmd = \"test -f src/feature.txt\"\n"),
            ),
        ],
        "post: s",
    )
}

/// plan -> the foreman's post -> the worker's passing finish: `s` is
/// tested and its foreman has been asked to review it.
fn plan_post_finish(repo: &DirectorRepo, player: &mut Player<NullSessions>) {
    plan(repo, player);
    foreman_post(repo);
    let report = player.tick().expect("post tick should succeed");
    assert_eq!(report.launched, vec!["s".to_string()], "{report}");
    let dir = paths::worker_clone(&repo.director_path(), "s");
    ensure_identity(&dir);
    write_file(&dir, "src/feature.txt", "done\n");
    commit(&dir, "finish work");
    let report = player.tick().expect("finish tick should succeed");
    assert!(
        report
            .imported
            .iter()
            .any(|(_, _, t)| *t == Transition::Finish),
        "{report}"
    );
}

/// The foreman's worktree git directory, where `dashboard report`
/// files its report.
fn foreman_git_dir(repo: &DirectorRepo) -> PathBuf {
    let out = Git::new(foreman_dir(repo))
        .run(&["rev-parse", "--absolute-git-dir"])
        .expect("git dir");
    PathBuf::from(out)
}

/// `dashboard report <verdict> --why <why>` (and `--suite` on accept), as
/// the foreman runs it from its worktree.
fn file_report(repo: &DirectorRepo, verdict: Option<&str>, why: &str) -> Result<report::Filed, String> {
    report::submit(
        &foreman_git_dir(repo),
        &report::Draft {
            verdict: verdict.map(String::from),
            why: why.to_string(),
            check: Some("src/feature.txt exists".to_string()),
            suite: (verdict == Some("accept")).then(|| "s-check: keep".to_string()),
            concerns: None,
        },
    )
}

// --------------------------------------------------------------------------
// Launch
// --------------------------------------------------------------------------

#[test]
fn a_ready_task_launches_its_foreman_with_the_post_step() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    let report = plan(&repo, &mut player);
    assert_eq!(report.foremen_launched, vec!["s".to_string()], "{report}");
    let call = player
        .sessions()
        .launched
        .iter()
        .find(|c| c.slug == "s_foreman")
        .expect("a launch for the foreman")
        .clone();
    assert_eq!(call.agent_cmd, "fake-foreman");
    assert_eq!(call.clone_dir, foreman_dir(&repo));
    assert_eq!(
        Git::new(&call.clone_dir)
            .run(&["branch", "--show-current"])
            .unwrap(),
        "foreman/s"
    );

    let prompt = std::fs::read_to_string(&call.prompt_file).expect("prompt file");
    for needle in [
        "foreman for task `s`",
        "Never write a throwaway implementation",
        "must be pure",
        "mind the widget cache",
        "ship the widget",
        "Step: post",
        "dashboard report",
    ] {
        assert!(
            prompt.contains(needle),
            "prompt lacks {needle:?}:\n{prompt}"
        );
    }

    // A second tick launches nothing more.
    let report = player.tick().expect("tick");
    assert!(report.foremen_launched.is_empty(), "{report}");

    let kinds: Vec<EventKind> = events::read_all(&home).iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EventKind::ForemanLaunched), "{kinds:?}");
    assert!(
        !kinds.contains(&EventKind::Ready),
        "a foreman's ready task needs nothing from the director: {kinds:?}"
    );
}

#[test]
fn spikes_and_disabled_foremen_launch_none() {
    let repo = director_repo(); // foreman_cmd = ""
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    let report = plan(&repo, &mut player);
    assert!(report.foremen_launched.is_empty(), "{report}");

    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    director_commit_on_branch(
        repo.path(),
        "task/sp",
        "tasks/sp/task.toml",
        "title = \"sp\"\nkind = \"spike\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: sp",
    );
    let report = player.tick().expect("tick");
    assert!(
        report.foremen_launched.is_empty(),
        "a spike's question is the director's: {report}"
    );
}

// --------------------------------------------------------------------------
// The foreman's post
// --------------------------------------------------------------------------

#[test]
fn the_foremans_post_is_imported_and_launches_the_worker() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(&repo, &mut player);

    let sha = foreman_post(&repo);
    let report = player.tick().expect("post tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Post);
    let note = records::import_note(&hub, &sha).expect("import note");
    assert_eq!(note.actor, Actor::Foreman);
    assert_eq!(report.launched, vec!["s".to_string()], "{report}");
    assert!(
        foreman_sent(&player)
            .iter()
            .any(|m| m.contains("imported your post")),
        "{:?}",
        foreman_sent(&player)
    );
}

#[test]
fn the_foremans_tree_is_full_beside_a_sparse_worker() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);
    foreman_post(&repo);
    let report = player.tick().expect("post tick");
    assert_eq!(report.launched, vec!["s".to_string()], "{report}");
    let worker = paths::worker_clone(&repo.director_path(), "s");
    let foreman = foreman_dir(&repo);
    for f in ["roadmap.toml", "prompts.kdl"] {
        assert!(foreman.join(f).exists(), "{f} in the foreman's tree");
        assert!(!worker.join(f).exists(), "{f} absent from the worker's tree");
    }
    assert!(foreman.join("src").exists() && worker.join("src").exists());
    assert!(!Git::new(&foreman).is_sparse());
    assert!(Git::new(&worker).is_sparse());
}

#[test]
fn a_director_post_is_ownership_when_foremen_are_on() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);

    checkout_branch(repo.path(), "task/s");
    write_file(repo.path(), "tasks/s/prompt.md", "do it\n");
    write_file(
        repo.path(),
        &*tests_file("s"),
        &*task_suite_lean("[[check]]\nname = \"s-check\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    commit(repo.path(), "post: s");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert_eq!(report.rejected[0].actor, Actor::Director);
    assert!(
        report.rejected[0].message.contains("foreman"),
        "{}",
        report.rejected[0].message
    );
}

fn repo_with_lock_files() -> DirectorRepo {
    let repo = repo_with_foremen();
    write_file(
        repo.path(),
        "roadmap.toml",
        "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\ngoal = \"ship the widget\"\n\n[player]\nruns = \"parallel\"\nforeman_cmd = \"fake-foreman\"\nlock_files = [\"Cargo.lock\"]\n",
    );
    commit(repo.path(), "name the lock file");
    repo
}

#[test]
fn a_foreman_may_commit_a_lock_file_only_when_the_roadmap_names_it() {
    // Named: the post with its lock entry is not an ownership rejection.
    let repo = repo_with_lock_files();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);
    foreman_post(&repo);
    foreman_commit(&repo, &[("Cargo.lock", "# locked\n")], "lock the test dep");
    let report = player.tick().expect("tick");
    assert!(
        !report
            .rejected
            .iter()
            .any(|r| r.reason == Reason::Ownership),
        "{report}"
    );

    // Not named: the same commit is ownership.
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);
    foreman_post(&repo);
    foreman_commit(&repo, &[("Cargo.lock", "# locked\n")], "lock the test dep");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(report.rejected[0].message.contains("Cargo.lock"));
}

#[test]
fn a_foreman_never_writes_product_code_or_the_plan_or_a_marker() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(&repo, &mut player);
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();

    // Product code next to the post.
    foreman_post(&repo);
    foreman_commit(
        &repo,
        &[("src/feature.rs", "fn main() {}\n")],
        "a throwaway implementation",
    );
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert_eq!(report.rejected[0].actor, Actor::Foreman);
    assert!(
        report.rejected[0].message.contains("src/feature.rs"),
        "{}",
        report.rejected[0].message
    );
    assert!(
        foreman_sent(&player)
            .iter()
            .any(|m| m.contains("rejected your commit")),
        "rejections are relayed to the foreman"
    );

    // The plan: depends_on, phase and the rest are the director's.
    let dir = foreman_dir(&repo);
    Git::new(&dir)
        .run(&["reset", "-q", "--hard", &tip])
        .unwrap();
    foreman_commit(
        &repo,
        &[
            ("tasks/s/prompt.md", "p\n"),
            (
                "tasks/s/task.toml",
                "title = \"s, renamed\"\nphase = \"p1\"\nmodules = [\"app\"]\nbrief = \"mind the widget cache\"\n",
            ),
            (
                &*tests_file("s"),
                &*task_suite_lean("[[check]]\nname = \"s-check\"\ncmd = \"test -f src/feature.txt\"\n"),
            ),
        ],
        "post with a new title",
    );
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert!(
        report.rejected[0].message.contains("task.toml is the director's plan"),
        "{}",
        report.rejected[0].message
    );

    // A marker.
    Git::new(&dir)
        .run(&["reset", "-q", "--hard", &tip])
        .unwrap();
    commit_empty(&dir, "accept: s");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert!(
        report.rejected[0]
            .message
            .contains("never commits a marker"),
        "{}",
        report.rejected[0].message
    );

    assert_eq!(
        task(&hub, "s").status,
        Status::Escalated,
        "three ownership strikes exceed the default limit of 2"
    );
}

/// The post with its check's `files` declared and written beside it.
fn foreman_post_with_files(repo: &DirectorRepo, files: &str, written: &[(&str, &str)]) {
    let check = format!("[[check]]\nname = \"s-check\"\ncmd = \"test -f src/feature.txt\"\nfiles = {files}\n");
    let (suite, tests) = (task_suite_lean(&check), tests_file("s"));
    let mut all = vec![
        ("tasks/s/prompt.md", "make src/feature.txt; tests you add must be pure\n"),
        (&*tests, &*suite),
    ];
    all.extend_from_slice(written);
    foreman_commit(repo, &all, "post: s");
}

#[test]
fn a_foreman_adds_its_declared_test_files_inside_a_module() {
    // A new file under the post's own declared `files`, inside `app`'s `src/`.
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);
    foreman_post_with_files(&repo, "[\"src/tests/\"]", &[("src/tests/s.rs", "#[test] fn s() {}\n")]);
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.imported[0].2, Transition::Post, "{report}");

    // Declaring an existing file of the module keeps it the worker's.
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);
    foreman_post_with_files(&repo, "[\"src/\"]", &[("src/.keep", "mine\n")]);
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(report.rejected[0].message.contains("src/.keep"), "{}", report.rejected[0].message);
}

#[test]
fn a_hub_move_under_the_foremans_commit_is_rebased_by_the_player_without_a_strike() {
    hub_move_under_the_foremans_commit(false);
}

#[test]
fn a_hub_move_under_a_dirty_foreman_tree_asks_for_a_rebase_without_a_strike() {
    hub_move_under_the_foremans_commit(true);
}

/// Main moves under the staged task's foreman before it posts: a clean
/// worktree is rebased by the player and the post imported; a `dirty` one
/// is asked to rebase, once per commit.
fn hub_move_under_the_foremans_commit(dirty: bool) {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(&repo, &mut player);

    // Main moves and the player merges it into the staged branch; the task
    // foreman, still on the old tip, then posts (a replan is refused once the
    // foreman exists, so the hub moves by a merge).
    director_commit_on_branch(
        repo.path(),
        "main",
        "roadmap.toml",
        "title = \"Retitled\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\ngoal = \"ship the widget\"\n\n[player]\nruns = \"parallel\"\nforeman_cmd = \"fake-foreman\"\n",
        "retitle",
    );
    push_main_to_hub(repo.path());
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.imported.len(), 1, "only the merge: {report}");
    assert_eq!(report.imported[0].2, Transition::MergeUnblock);
    let post = foreman_post(&repo);
    if dirty {
        write_file(&foreman_dir(&repo), "scratch.txt", "mine\n");
    }
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert!(report.imported.is_empty(), "{report}");
    if !dirty {
        let foreman = Git::new(foreman_dir(&repo));
        let head = foreman.rev_parse("HEAD").unwrap();
        assert_ne!(head, post, "rebased");
        assert_eq!(
            foreman.run(&["rev-parse", "HEAD~1"]).ok(),
            hub.rev_parse("refs/heads/task/s")
        );
        let told = foreman_sent(&player).pop().unwrap();
        assert!(
            told.contains(&format!("The player rebased your commit onto it: foreman/s is at {}", &head[..12])),
            "{told}"
        );
        let report = player.tick().expect("tick");
        assert_eq!(
            report.imported.first().map(|i| i.2),
            Some(Transition::Post),
            "{report}"
        );
        assert!(task(&hub, "s").strikes.is_empty());
        return;
    }
    assert_eq!(Git::new(foreman_dir(&repo)).rev_parse("HEAD"), Some(post), "untouched");
    let asked: Vec<String> = foreman_sent(&player)
        .into_iter()
        .filter(|m| m.contains("git rebase origin/task/s"))
        .collect();
    assert_eq!(asked.len(), 1, "{asked:?}");

    // Said once, not every tick.
    player.tick().expect("tick");
    assert_eq!(
        foreman_sent(&player)
            .iter()
            .filter(|m| m.contains("git rebase origin/task/s"))
            .count(),
        1
    );

    // The foreman rebases its post onto the merge.
    let dir = foreman_dir(&repo);
    std::fs::remove_file(dir.join("scratch.txt")).unwrap();
    Git::new(&dir).run(&["fetch", "-q", "origin"]).unwrap();
    Git::new(&dir)
        .run(&["reset", "-q", "--hard", "origin/task/s"])
        .unwrap();
    foreman_commit(
        &repo,
        &[
            ("tasks/s/prompt.md", "make src/feature.txt\n"),
            (
                &*tests_file("s"),
                &*task_suite_lean("[[check]]\nname = \"s-check\"\ncmd = \"test -f src/feature.txt\"\n"),
            ),
        ],
        "post: s",
    );
    let report = player.tick().expect("tick");
    assert_eq!(
        report.imported.first().map(|i| i.2),
        Some(Transition::Post),
        "{report}"
    );
    assert!(task(&hub, "s").strikes.is_empty());
}

// --------------------------------------------------------------------------
// Review
// --------------------------------------------------------------------------

#[test]
fn a_tested_task_is_handed_to_its_foreman_for_review_once() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    let reviews: Vec<String> = foreman_sent(&player)
        .into_iter()
        .filter(|m| m.starts_with("Step: review round 1"))
        .collect();
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    assert!(reviews[0].contains("reviews/r1.md"), "{}", reviews[0]);
    // The player brought the foreman's clean worktree to the worker's work.
    let tip = Git::new(home.hub()).rev_parse("refs/heads/task/s").unwrap();
    assert_eq!(Git::new(foreman_dir(&repo)).rev_parse("HEAD"), Some(tip.clone()));
    assert!(
        reviews[0].contains(&format!("Your branch is already at {}, the tip of task/s.", &tip[..12])),
        "{}",
        reviews[0]
    );
    player.tick().expect("tick");
    assert_eq!(
        foreman_sent(&player)
            .iter()
            .filter(|m| m.starts_with("Step: review round 1"))
            .count(),
        1,
        "asked once per tested tip"
    );

    let kinds: Vec<EventKind> = events::read_all(&home).iter().map(|e| e.kind).collect();
    assert!(
        !kinds.contains(&EventKind::Tested),
        "the review is the foreman's: {kinds:?}"
    );
}

#[test]
fn the_foremans_send_back_is_imported_and_relayed_to_the_worker() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    let dir = foreman_dir(&repo);
    Git::new(&dir).run(&["fetch", "-q", "origin"]).unwrap();
    Git::new(&dir)
        .run(&["merge", "-q", "--ff-only", "origin/task/s"])
        .unwrap();
    let sha = foreman_commit(
        &repo,
        &[
            ("tasks/s/reviews/r1.md", "also src/feature2.txt\n"),
            (
                &*tests_file("s"),
                &*task_suite_lean("[[check]]\nname = \"s-check\"\ncmd = \"test -f src/feature.txt && test -f src/feature2.txt\"\n"),
            ),
        ],
        "send back: s r1",
    );
    let report = player.tick().expect("tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::SendBack);
    assert_eq!(
        records::import_note(&hub, &sha).unwrap().actor,
        Actor::Foreman
    );
    assert_eq!(task(&hub, "s").status, Status::InRevision);
    assert!(
        player
            .sessions()
            .sent
            .iter()
            .any(|(k, m)| k == "s" && m.contains("also src/feature2.txt")),
        "the worker gets the review"
    );

    // The import is the verdict: the report adds the why.
    file_report(&repo, None, "feature2 is missing.").expect("implied sent-back");
    let report = player.tick().expect("tick");
    let text = &report.foreman_reports[0].1.text;
    assert!(
        text.contains(&format!("step: review r1\ncommit: {sha}\n")) && text.contains("verdict: sent-back"),
        "{text}"
    );
}

#[test]
fn a_report_becomes_a_note_and_an_foreman_report_event() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    // The review takes accept or unsure; the command names them.
    for (verdict, needle) in [
        (Some("sent-back"), "review r1 (accept | unsure)"),
        (Some("maybe"), "no verdict"),
        (None, "name your verdict"),
    ] {
        let err = file_report(&repo, verdict, "it does it.").expect_err("refused");
        assert!(err.contains(needle), "{err}");
    }
    file_report(&repo, Some("accept"), "it does it.").expect("accept");
    let report = player.tick().expect("tick");
    assert_eq!(report.foreman_reports.len(), 1, "{report}");
    assert_eq!(report.foreman_reports[0].1.verdict, "accept");
    let text = &report.foreman_reports[0].1.text;
    assert!(
        text.starts_with("slug: s\nstep: review r1\ncommit: none\ncheck: src/feature.txt exists\nverdict: accept\nwhy: it does it.\nsuite: s-check: keep\nconcerns: none"),
        "{text}"
    );
    let err = file_report(&repo, Some("unsure"), "again").expect_err("closed");
    assert!(err.contains("no step waits"), "{err}");

    let t = task(&hub, "s");
    assert_eq!(
        t.foreman.and_then(|a| a.report).map(|r| r.verdict).as_deref(),
        Some("accept")
    );
    let ev = events::read_all(&home)
        .into_iter()
        .find(|e| e.kind == EventKind::ForemanReport)
        .expect("a foreman_report event");
    assert!(ev.message.contains("verdict: accept"), "{}", ev.message);

    // The director accepts on it; the foreman's session ends with the task.
    director_pull_task_branch(repo.path(), "s");
    commit_empty(repo.path(), "accept: s");
    let report = player.tick().expect("tick");
    assert!(
        report
            .imported
            .iter()
            .any(|(_, _, t)| *t == Transition::MergeAccept),
        "{report}"
    );
    assert!(player.sessions().killed.contains(&"s_foreman".to_string()));
    assert!(
        !foreman_dir(&repo).exists(),
        "the foreman's worktree is removed"
    );
}

// --------------------------------------------------------------------------
// ask / relaunch / death
// --------------------------------------------------------------------------

#[test]
fn ask_relaunch_and_death_of_a_foreman() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);

    player.ask("s", "why that check?").expect("ask");
    assert!(
        foreman_sent(&player)
            .iter()
            .any(|m| m.contains("Question from the director: why that check?"))
    );
    let err = file_report(&repo, Some("posted"), "early").expect_err("not imported");
    assert!(err.contains("follows from your commit"), "{err}");
    assert_eq!(file_report(&repo, Some("answer"), "it guards the cache.").unwrap().step, "question");
    let report = player.tick().expect("tick");
    assert_eq!(report.foreman_reports[0].1.verdict, "answer");
    assert!(
        player.relaunch_foreman("s").is_err(),
        "alive: nothing to relaunch"
    );

    player.sessions_mut().alive.remove("s_foreman");
    player.tick().expect("tick");
    let died = events::read_all(&home)
        .into_iter()
        .filter(|e| e.kind == EventKind::ForemanDied)
        .count();
    assert_eq!(died, 1);
    assert!(
        player.ask("s", "still there?").is_err(),
        "dead: ask must say so"
    );

    player.relaunch_foreman("s").expect("relaunch");
    let call = player
        .sessions()
        .launched
        .iter()
        .rfind(|c| c.slug == "s_foreman")
        .unwrap()
        .clone();
    let prompt = std::fs::read_to_string(&call.prompt_file).unwrap();
    assert!(
        prompt.contains("You were relaunched") && prompt.contains("Step: post"),
        "{prompt}"
    );
    assert!(player.sessions().alive("s_foreman"));
}

#[test]
fn worker_and_foreman_relaunches_are_capped_apart() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(&repo, &mut player);
    foreman_post(&repo);
    let report = player.tick().expect("post tick");
    assert_eq!(report.launched, vec!["s".to_string()], "{report}");
    let counts = || {
        let t = task(&hub, "s");
        (t.relaunches, t.foreman.map(|a| a.relaunches).unwrap_or(99))
    };

    for n in 1..=2 {
        player.sessions_mut().alive.remove("s_foreman");
        open_player(&home)
            .relaunch_foreman("s")
            .expect("relaunch --foreman");
        assert_eq!(counts(), (0, n));
    }
    // The worker's own count is untouched by its foreman's.
    player.sessions_mut().alive.remove("s");
    player.relaunch("s").expect("worker relaunch");
    assert_eq!(counts(), (1, 2));

    let err = player
        .relaunch_foreman("s")
        .expect_err("a third foreman relaunch");
    assert!(
        err.contains("foreman") && err.contains("max_relaunches"),
        "{err}"
    );
    let t = task(&hub, "s");
    assert_eq!(t.status, Status::Escalated);
    assert!(
        t.escalation
            .as_deref()
            .is_some_and(|c| c.contains("relaunch limit (2) reached: the foreman")),
        "{:?}",
        t.escalation
    );
    player.tick().expect("tick");
    assert!(
        events::read_all(&home)
            .iter()
            .any(|e| e.kind == EventKind::Escalated && e.message.contains("relaunch limit")),
        "{:?}",
        events::read_all(&home)
    );
    // Already escalated, another try is refused without a new escalation.
    let err = player.relaunch_foreman("s").expect_err("still capped");
    assert!(err.contains("already escalated"), "{err}");

    // `--reset` must name the session that hit its limit.
    player.sessions_mut().alive.remove("s");
    let err = player
        .reset_relaunch("s", false)
        .expect_err("not the worker's");
    assert!(
        err.contains("dashboard relaunch s --foreman --reset"),
        "{err}"
    );
    let launched = player.sessions().launched.len();
    player.reset_relaunch("s", true).expect("reset --foreman");
    assert_eq!(player.sessions().launched.len(), launched + 1);
    assert!(player.sessions().alive("s_foreman"));
    let t = task(&hub, "s");
    assert_ne!(t.status, Status::Escalated);
    assert_eq!(counts(), (1, 1), "only the foreman's count starts over");
    let cleared = records::cleared_escalations(&hub, "s");
    assert_eq!(cleared.len(), 1, "{cleared:?}");
    assert_eq!(cleared[0].session, "foreman");
    player.tick().expect("tick");
    assert!(
        events::read_all(&home)
            .iter()
            .any(|e| e.kind == EventKind::Resumed),
        "{:?}",
        events::read_all(&home)
    );
}

#[test]
fn a_collected_report_starts_the_foremen_relaunch_count_over() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);
    let foreman_relaunches = || task(&hub, "s").foreman.map(|a| a.relaunches).unwrap();

    for _ in 0..2 {
        player.sessions_mut().alive.remove("s_foreman");
        player.relaunch_foreman("s").expect("relaunch --foreman");
    }
    assert_eq!(foreman_relaunches(), 2);

    file_report(&repo, Some("unsure"), "hm.").expect("unsure");
    let report = player.tick().expect("tick");
    assert_eq!(report.foreman_reports.len(), 1, "{report}");
    assert_eq!(foreman_relaunches(), 0, "a report is progress");

    player.sessions_mut().alive.remove("s_foreman");
    player.relaunch_foreman("s").expect("relaunch after progress");
    assert_eq!(foreman_relaunches(), 1);
    assert_eq!(task(&hub, "s").status, Status::Tested);
}

#[test]
fn a_foreman_stopped_while_it_owes_work_is_reported_idle_once_per_stop() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    let idle = || {
        events::read_all(&home)
            .into_iter()
            .filter(|e| e.kind == EventKind::ForemanIdle)
            .collect::<Vec<_>>()
    };
    plan(&repo, &mut player);
    let marker = player::stop_marker(&foreman_dir(&repo)).expect("stop marker path");
    assert!(
        foreman_dir(&repo)
            .join(".claude/settings.local.json")
            .exists()
    );

    // Staged, it owes the post: a stop without one is reported, once.
    std::fs::write(&marker, "stop 100 1\n").unwrap();
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1, "{:?}", events::read_all(&home));
    assert!(
        idle()[0].message.contains("staged"),
        "{}",
        idle()[0].message
    );
    player.tick().expect("tick");
    drop(player);
    let mut player = open_player(&home);
    // Its session outlived the player.
    player.sessions_mut().alive.insert("s_foreman".to_string());
    player.tick().expect("tick after a restart");
    assert_eq!(idle().len(), 1);

    // A stop after leaving a report is the wait for the director.
    file_report(&repo, Some("unsure"), "which widget?").expect("unsure on the post");
    std::fs::write(&marker, "stop 101 2\n").unwrap();
    let report = player.tick().expect("tick");
    assert_eq!(report.foreman_reports.len(), 1, "{report}");
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1, "a report answers it");

    // The director's ask re-arms it; stopping without an answer is idle.
    player.ask("s", "the cached one; post it").expect("ask");
    assert!(!marker.exists(), "a relay clears the stop");
    std::fs::write(&marker, "stop 102 3\n").unwrap();
    player.tick().expect("tick");
    assert_eq!(idle().len(), 2);

    // Its post imported, the task waits on the worker: no longer owed.
    foreman_post(&repo);
    player.tick().expect("post tick");
    std::fs::write(&marker, "stop 103 4\n").unwrap();
    player.tick().expect("tick");
    assert_eq!(idle().len(), 2, "{:?}", events::read_all(&home));
}

#[test]
fn a_foreman_whose_screen_goes_stale_while_it_owes_work_is_reported_idle() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    let idle = || {
        events::read_all(&home)
            .into_iter()
            .filter(|e| e.kind == EventKind::ForemanIdle)
            .collect::<Vec<_>>()
    };
    plan(&repo, &mut player);
    player.sessions_mut().panes.insert(
        "s_foreman".to_string(),
        "Do you trust this folder? [y/n]".to_string(),
    );

    // Staged, it owes the post; a screen stuck on a dialog is reported once.
    player.set_clock(1000);
    player.tick().expect("tick");
    player.set_clock(1900);
    player.tick().expect("tick");
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1, "{:?}", events::read_all(&home));
    assert!(
        idle()[0]
            .message
            .contains("no new output for 15 min while the task is staged"),
        "{}",
        idle()[0].message
    );

    // Its post imported, the task waits on the worker: no longer owed.
    foreman_post(&repo);
    player.tick().expect("post tick");
    player.set_clock(10_000);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1, "{:?}", events::read_all(&home));
}

#[test]
fn a_foreman_dead_before_any_tick_saw_it_is_reported_once_per_launch() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    let died = || {
        events::read_all(&home)
            .into_iter()
            .filter(|e| e.kind == EventKind::ForemanDied)
            .count()
    };

    // Past the first run, which stays silent about whatever it finds.
    player.tick().expect("first tick");
    // `foreman_cmd` crashes at startup: dead by the end of the tick that
    // launched it.
    player
        .sessions_mut()
        .stillborn
        .insert("s_foreman".to_string());
    plan(&repo, &mut player);
    assert!(task(&Git::new(home.hub()), "s").foreman.is_some());
    assert_eq!(died(), 1);
    player.tick().expect("tick");
    assert_eq!(died(), 1, "the same death isn't reported again");
    drop(player);
    let mut player = open_player(&home);
    player.tick().expect("tick after a restart");
    assert_eq!(died(), 1, "nor after a player restart");

    // `dashboard relaunch --foreman`, from its own process, dies again.
    open_player(&home).relaunch_foreman("s").expect("relaunch");
    player.tick().expect("tick after the relaunch");
    assert_eq!(died(), 2, "a relaunch that dies is a new death");
    player.tick().expect("tick");
    assert_eq!(died(), 2);
}

// --------------------------------------------------------------------------
// Contests (spec §18)
// --------------------------------------------------------------------------

/// plan -> the foreman's post -> the worker contests the task check.
fn plan_post_contest(repo: &DirectorRepo, player: &mut Player<NullSessions>) -> TickReport {
    plan(repo, player);
    foreman_post(repo);
    player.tick().expect("post tick");
    let dir = paths::worker_clone(&repo.director_path(), "s");
    ensure_identity(&dir);
    Git::new(&dir)
        .run(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "contest: s-check",
            "-m",
            "src/feature.txt can't exist in a pure test",
        ])
        .unwrap();
    player.tick().expect("contest tick")
}

#[test]
fn a_contest_goes_to_the_foreman_and_its_reply_back_to_the_worker() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    let report = plan_post_contest(&repo, &mut player);
    assert_eq!(
        report.imported.first().map(|i| i.2),
        Some(Transition::Contest),
        "{report}"
    );
    let t = task(&hub, "s");
    assert_eq!(t.status, Status::Contested);
    assert!(
        t.contest.as_deref().unwrap_or("").contains("pure test"),
        "{:?}",
        t.contest
    );
    let asked: Vec<String> = foreman_sent(&player)
        .into_iter()
        .filter(|m| m.starts_with("Step: contest c1"))
        .collect();
    assert_eq!(asked.len(), 1, "{:?}", foreman_sent(&player));
    assert!(asked[0].contains("pure test"), "{}", asked[0]);
    assert!(
        !events::read_all(&home)
            .iter()
            .any(|e| e.kind == EventKind::Contested),
        "the foreman's to answer"
    );

    let dir = foreman_dir(&repo);
    Git::new(&dir).run(&["fetch", "-q", "origin"]).unwrap();
    Git::new(&dir)
        .run(&["merge", "-q", "--ff-only", "origin/task/s"])
        .unwrap();
    let sha = foreman_commit(
        &repo,
        &[(
            "tasks/s/contests/c1.md",
            "fair: check src/feature.txt is created by the build\n",
        )],
        "reply: s c1",
    );
    let report = player.tick().expect("reply tick");
    assert_eq!(
        report.imported.first().map(|i| i.2),
        Some(Transition::ContestReply),
        "{report}"
    );
    assert_eq!(
        records::import_note(&hub, &sha).unwrap().actor,
        Actor::Foreman
    );
    let t = task(&hub, "s");
    assert_eq!(t.status, Status::InRevision);
    assert_eq!(
        (t.contest_round, t.review_round),
        (1, 0),
        "a reply spends no review round"
    );
    assert_eq!(
        hub.rev_parse("refs/dashboard/pins/s").as_deref(),
        Some(sha.as_str()),
        "re-pinned at the reply"
    );
    assert!(player.sessions().sent.iter().any(|(k, m)| k == "s"
        && m.contains("Reply to your contest c1")
        && m.contains("created by the build")));
    assert_eq!(file_report(&repo, None, "fair point.").unwrap().verdict, report::Verdict::Replied);
    let report = player.tick().expect("tick");
    assert!(report.foreman_reports[0].1.text.contains("step: contest c1\n"), "{report}");
}

#[test]
fn without_foremen_the_director_replies_and_a_contest_past_the_limit_escalates() {
    let repo = director_repo(); // foremen off
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!(
            "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\n\n[limits]\nmax_contest_rounds = 1\n{NO_FOREMEN}"
        ),
    );
    commit(repo.path(), "one contest only");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    player.tick().expect("plan tick");
    checkout_branch(repo.path(), "task/s");
    write_file(repo.path(), "tasks/s/prompt.md", "make src/feature.txt\n");
    write_file(
        repo.path(),
        &*tests_file("s"),
        &*task_suite_lean("[[check]]\nname = \"s-check\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    commit(repo.path(), "post: s");
    player.tick().expect("post tick");

    let clone = paths::worker_clone(&repo.director_path(), "s");
    ensure_identity(&clone);
    commit_empty(&clone, "contest: s-check");
    player.tick().expect("contest tick");
    assert_eq!(task(&hub, "s").status, Status::Contested);
    assert!(
        events::read_all(&home)
            .iter()
            .any(|e| e.kind == EventKind::Contested),
        "the director's to answer"
    );

    // The reply needs its file.
    director_pull_task_branch(repo.path(), "s");
    write_file(
        repo.path(),
        &*tests_file("s"),
        &*task_suite_lean("[[check]]\nname = \"s-check\"\ncmd = \"test -f src/feature.txt\"\nruns = 1\n"),
    );
    commit(repo.path(), "a reply without a reply file");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert!(
        report.rejected[0].message.contains("contests/c1.md"),
        "{}",
        report.rejected[0].message
    );

    write_file(
        repo.path(),
        "tasks/s/contests/c1.md",
        "the test stands: create src/feature.txt\n",
    );
    commit(repo.path(), "reply: s c1");
    let report = player.tick().expect("tick");
    assert_eq!(
        report.imported.first().map(|i| i.2),
        Some(Transition::ContestReply),
        "{report}"
    );

    Git::new(&clone)
        .run(&["pull", "-q", "--ff-only", "origin", "task/s"])
        .unwrap();
    commit_empty(&clone, "contest: s-check again");
    let report = player.tick().expect("tick");
    assert_eq!(report.escalated.len(), 1, "{report}");
    assert!(
        report.escalated[0]
            .1
            .contains("contest rounds exceeded: 2 > 1"),
        "{}",
        report.escalated[0].1
    );
    assert_eq!(task(&hub, "s").status, Status::Escalated);
}

#[test]
fn another_tasks_foreman_commits_merged_in_from_main_still_take_a_rebase() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    // `t` is planned and its foreman launched while `s` is still tested.
    director_commit_on_branch(
        repo.path(),
        "task/t",
        "tasks/t/task.toml",
        "title = \"t\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: t",
    );
    commit_empty(repo.path(), "stage: go");
    let report = player.tick().expect("plan tick");
    assert!(report.rejected.is_empty(), "{report}");
    let t_dir = paths::foreman_clone(&repo.director_path(), "t");
    assert!(t_dir.exists(), "t's foreman clone: {report}");

    // Accepting `s` lands its foreman's post on main, and the player
    // merges main into the not-yet-posted task/t under t's foreman.
    director_pull_task_branch(repo.path(), "s");
    commit_empty(repo.path(), "accept: s");
    let report = player.tick().expect("accept tick");
    assert_eq!(task(&hub, "s").status, Status::Accepted, "{report}");
    if !report.merges.iter().any(|m| m.starts_with("task/t:")) {
        player.tick().expect("unblock tick");
    }
    let t_tip = hub.rev_parse("refs/heads/task/t").unwrap();
    let main_tip = hub.rev_parse("refs/heads/main").unwrap();
    assert!(
        hub.is_ancestor(&main_tip, &t_tip),
        "main merged into task/t"
    );

    // t's foreman posts on the tip it was launched at: the hub moved under
    // it, which asks for a rebase, not a `history` rejection.
    ensure_identity(&t_dir);
    write_file(&t_dir, "tasks/t/prompt.md", "make t.txt\n");
    write_file(
        &t_dir,
        "tasks/t/task.toml",
        "title = \"t\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(
        &t_dir,
        &*tests_file("t"),
        &*task_suite_lean("[[check]]\nname = \"t-check\"\ncmd = \"test -f t.txt\"\n"),
    );
    commit(&t_dir, "post: t");
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert!(
        player
            .sessions()
            .sent
            .iter()
            .any(|(k, m)| k == "t_foreman"
                && m.contains("The player rebased your commit onto it: foreman/t is at")),
        "t's foreman's post is rebased"
    );
    assert!(task(&hub, "t").strikes.is_empty());
}

#[test]
fn a_lock_file_under_test_paths_is_refused_at_init() {
    let repo = repo_with_lock_files();
    write_file(repo.path(), TESTS_LEAN, &*suite_lean("test_paths = [\"Cargo.lock\"]\n"));
    commit(repo.path(), "pin the lock file");
    let home_dir = temp_home();
    let err = init::init(
        repo.path(),
        Some("proj"),
        Some(home_dir.path().join("home")),
    )
    .expect_err("overlap is refused");
    assert!(
        err.contains("lock_files") && err.contains("test_paths"),
        "{err}"
    );
}

// --------------------------------------------------------------------------
// The status gate (src/fsm.rs) as the player applies it
// --------------------------------------------------------------------------

#[test]
fn a_director_send_back_is_ownership_when_foremen_are_on() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    director_pull_task_branch(repo.path(), "s");
    write_file(repo.path(), "tasks/s/reviews/r1.md", "do more\n");
    commit(repo.path(), "send back: s r1");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert_eq!(report.rejected[0].actor, Actor::Director);
    assert!(
        report.rejected[0].message.contains("send-backs"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task(&hub, "s").status, Status::Tested);
}

#[test]
fn a_director_contest_reply_is_ownership_when_foremen_are_on() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_contest(&repo, &mut player);

    director_pull_task_branch(repo.path(), "s");
    write_file(repo.path(), "tasks/s/contests/c1.md", "no\n");
    commit(repo.path(), "reply: s c1");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(
        report.rejected[0].message.contains("contest"),
        "{}",
        report.rejected[0].message
    );
    assert_eq!(task(&hub, "s").status, Status::Contested);
}

#[test]
fn a_director_replan_after_posting_is_ownership() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    director_pull_task_branch(repo.path(), "s");
    write_file(
        repo.path(),
        "tasks/s/task.toml",
        &format!("{PLAN}# edited\n"),
    );
    commit(repo.path(), "replan: s");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
}

#[test]
fn a_worker_commit_outside_its_statuses_is_ignored_not_struck() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());

    // Tested: the worker is not listened to.
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();
    let dir = paths::worker_clone(&repo.director_path(), "s");
    write_file(&dir, "extra.txt", "late\n");
    commit(&dir, "late work");
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert!(report.imported.is_empty(), "{report}");
    assert_eq!(hub.rev_parse("refs/heads/task/s").unwrap(), tip);
    assert_eq!(task(&hub, "s").status, Status::Tested);
}

#[test]
fn a_worker_push_while_contested_is_ignored_not_struck() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_contest(&repo, &mut player);
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();

    let dir = paths::worker_clone(&repo.director_path(), "s");
    write_file(&dir, "src/feature.txt", "done anyway\n");
    commit(&dir, "finish while contested");
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert!(report.imported.is_empty(), "{report}");
    assert_eq!(hub.rev_parse("refs/heads/task/s").unwrap(), tip);
    assert_eq!(task(&hub, "s").status, Status::Contested);
}

/// Main's suite on the `a`/`b` architecture: `unit` (all) and `story`
/// covering `b` with its own file `stories/b.sh`.
const AB_SUITE: &str = "import Architecture\nimport Architect.Tests\nopen Dashboard\n\n\
check unit where\n  cmd = \"true\"\n  covers = all\n\n\
check story where\n  cmd = \"sh stories/b.sh\"\n  covers = [b]\n  files = [\"stories/b.sh\"]\n";

/// A foreman repo on the `a`/`b` architecture with `s` planned on
/// `modules` and staged; the foreman then commits its post plus
/// `extra` (path, content). Returns the post tick's report.
fn post_with(modules: &str, extra: &[(&str, &str)]) -> TickReport {
    let repo = repo_with_foremen();
    write_lean_arch(repo.path(), &format!("{LEAN_AB_MANIFEST}{APP_ROOT}"), &[("ab", LEAN_AB_IDL)], &[("Tests", AB_SUITE)]);
    for (f, t) in [("a/lib.rs", ""), ("b/lib.rs", ""), ("api/ab.rs", "pub enum Ab { X }\n"), ("stories/b.sh", "true\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "modules a and b");
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    let plan_toml = format!("title = \"s\"\nphase = \"p1\"\nmodules = {modules}\n");
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", &plan_toml, "plan: s");
    commit_empty(repo.path(), "stage: go");
    let report = player.tick().expect("plan tick");
    assert!(report.rejected.is_empty(), "{report}");
    let mut files: Vec<(String, String)> = vec![
        ("tasks/s/prompt.md".into(), "make a/feature.txt\n".into()),
        (tests_file("s"), lean_task_tests("s-check", "test -f a/feature.txt", "[a]")),
    ];
    files.extend(extra.iter().map(|(p, t)| (p.to_string(), t.to_string())));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    foreman_commit(&repo, &refs, "post: s");
    player.tick().expect("post tick")
}

#[test]
fn a_foreman_writes_apis_and_test_files_inside_its_scope() {
    let ownership = |r: &TickReport| r.rejected.iter().any(|x| x.reason == Reason::Ownership);
    // Its own Tests file (in every post) is its to write.
    let r = post_with("[\"a\"]", &[]);
    assert!(r.rejected.is_empty(), "{r}");
    // An API whose consumer is outside the task's modules, the manifest, and
    // the files of a check outside its run set are not.
    for (path, text) in [
        ("api/ab.rs", "pub enum Ab { X, Y }\n"),
        ("architecture/Apis/ab.lean", "-- changed\n"),
        ("architecture/Architecture.lean", "-- changed\n"),
        ("stories/b.sh", "false\n"),
    ] {
        let r = post_with("[\"a\"]", &[(path, text)]);
        assert!(ownership(&r), "{path}: {r}");
        assert!(r.rejected.iter().any(|x| x.message.contains(path)), "{path}: {r}");
    }
    // With both ends of `ab` in its modules, the API and `story`'s file are.
    for (path, text) in [("api/ab.rs", "pub enum Ab { X, Y }\n"), ("stories/b.sh", "true # still\n")] {
        let r = post_with("[\"a\", \"b\"]", &[(path, text)]);
        assert!(!ownership(&r), "{path}: {r}");
    }
}

// --------------------------------------------------------------------------
// `dashboard stage | unstage | accept` (src/marker.rs)
// --------------------------------------------------------------------------

fn marker(repo: &DirectorRepo, home: &Home, m: marker::Marker, why: Option<&str>) -> Result<marker::Outcome, String> {
    let args = marker::Args { answer: None, why: why.map(String::from) };
    marker::run(&Git::new(home.hub()), &repo.git(), m, "s", &args)
}

#[test]
fn the_director_stages_and_accepts_with_commands_and_an_ill_timed_marker_is_refused() {
    let repo = repo_with_foremen();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    player.tick().expect("plan tick");
    assert_eq!(task(&hub, "s").status, Status::Ready);

    // Refused with the player's own words; nothing is committed.
    let before = repo.git().rev_parse("refs/heads/task/s");
    for (m, needle) in [
        (marker::Marker::Unstage, "unstage: task/s is ready — only a staged task can be unstaged"),
        (marker::Marker::Accept, "accept: task/s is ready — it must be tested"),
        (marker::Marker::Conclude, "conclude: task/s is not a spike"),
    ] {
        let err = marker(&repo, &home, m, None).expect_err("ill-timed");
        assert!(err.contains(needle), "{err}");
    }
    let err = marker(&repo, &home, marker::Marker::Retire, None).expect_err("no post-mortem");
    assert!(err.contains("retire needs --why"), "{err}");
    assert_eq!(repo.git().rev_parse("refs/heads/task/s"), before);

    let out = marker(&repo, &home, marker::Marker::Stage, Some("next up")).expect("stage");
    assert_eq!(out.message, "stage: s\n\nnext up");
    assert_eq!(repo.git().rev_parse("HEAD").as_deref(), Some(out.commit.as_str()), "the checked-out branch moves");
    let report = player.tick().expect("stage tick");
    assert!(report.imported.iter().any(|(_, c, t)| *t == Transition::Stage && *c == out.commit), "{report}");
    assert!(report.foremen_launched.contains(&"s".to_string()), "{report}");
    let err = marker(&repo, &home, marker::Marker::Stage, None).expect_err("staged twice");
    assert!(err.contains("stage: task/s is staged — only a ready task can be staged"), "{err}");
    let err = marker(&repo, &home, marker::Marker::Unstage, None).expect_err("foreman running");
    assert!(err.contains("foreman is already running"), "{err}");

    // Post, finish, the foreman's accept report: the accept takes its why.
    foreman_post(&repo);
    player.tick().expect("post tick");
    let dir = paths::worker_clone(&repo.director_path(), "s");
    ensure_identity(&dir);
    write_file(&dir, "src/feature.txt", "done\n");
    commit(&dir, "finish work");
    player.tick().expect("finish tick");
    assert_eq!(task(&hub, "s").status, Status::Tested);
    let err = marker(&repo, &home, marker::Marker::Accept, None).expect_err("no report yet");
    assert!(err.contains("pass --why"), "{err}");
    file_report(&repo, Some("accept"), "it does it.\nAnd only that.").expect("accept report");
    player.tick().expect("report tick");
    let out = marker(&repo, &home, marker::Marker::Accept, None).expect("accept");
    assert_eq!(out.message, "accept: s\n\nit does it.\nAnd only that.");
    let report = player.tick().expect("accept tick");
    assert!(report.imported.iter().any(|(_, _, t)| *t == Transition::MergeAccept), "{report}");
    assert_eq!(task(&hub, "s").status, Status::Accepted);
}
