// Integration tests for an impure `main` (spec §20): the player verifies
// main's suite under the sandbox, reports impure checks to the director once,
// holds (doesn't strike) worker finishes that fail only on them, lets the
// director keep committing on main, and, once a fix lands, merges main into
// the held task and has the worker rebase.
//
// The fixture's `env` check is impure until roadmap.toml has a `# clean`
// line: that's a file the director may change on main, so the "fix" is one
// ordinary director commit.

mod common;

use common::*;
use dashboard::events::{self, EventKind};
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{NullSessions, Player};
use dashboard::records::{self, Transition};
use dashboard::state;
use tempfile::TempDir;

const IMPURE: &str = "grep -q '^# clean' roadmap.toml || touch leak.txt";

fn impure_repo() -> DirectorRepo {
    let repo = director_repo();
    write_file(
        repo.path(),
        TESTS_LEAN,
        &*suite_lean(&format!(
            "[[check]]\nname = \"unit\"\ncmd = \"true\"\n\n[[check]]\nname = \"env\"\ncmd = {IMPURE:?}\n"
        )),
    );
    commit(repo.path(), "an impure check");
    repo
}

fn setup() -> (DirectorRepo, TempDir, Home, Player<NullSessions>) {
    let repo = impure_repo();
    let home_dir = temp_home();
    let report = init::init(
        repo.path(),
        Some("proj"),
        Some(home_dir.path().join("home")),
    )
    .expect("init should succeed");
    let home = Home { root: report.home };
    let player = Player::open(
        Home {
            root: home.root.clone(),
        },
        NullSessions::default(),
    )
    .unwrap();
    (repo, home_dir, home, player)
}

fn event_kinds(home: &Home) -> Vec<EventKind> {
    events::read_all(home).into_iter().map(|e| e.kind).collect()
}

/// plan + post `a` (task check `test -f src/feature.txt`), launching its worker.
fn plan_and_post(repo: &DirectorRepo, player: &mut Player<NullSessions>) {
    director_commit_on_branch(
        repo.path(),
        "task/a",
        "tasks/a/task.toml",
        "title = \"a\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: a",
    );
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    write_file(repo.path(), "tasks/a/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/a/task.toml",
        "title = \"a\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    write_file(
        repo.path(),
        &*tests_file("a"),
        &*task_suite_lean("[[check]]\nname = \"a-check\"\ncmd = \"test -f src/feature.txt\"\n"),
    );
    commit(repo.path(), "post: a");
    let report = player.tick().unwrap();
    assert_eq!(report.launched, vec!["a".to_string()], "{report}");
}

fn worker_git(repo: &DirectorRepo) -> Git {
    let dir = paths::worker_clone(&repo.director_path(), "a");
    let git = Git::new(&dir);
    git.run(&["config", "user.name", "Test Worker"]).unwrap();
    git.run(&["config", "user.email", "worker@example.com"])
        .unwrap();
    git.run(&["config", "commit.gpgsign", "false"]).unwrap();
    git
}

/// The director's fix: a `# clean` line in roadmap.toml, on main.
fn fix_main(repo: &DirectorRepo) {
    checkout_branch(repo.path(), "main");
    let git = repo.git();
    git.run(&["fetch", "-q", "hub"]).unwrap();
    git.run(&["merge", "-q", "--ff-only", "hub/main"]).unwrap();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(repo.path(), "roadmap.toml", &format!("# clean\n{roadmap}"));
    commit(repo.path(), "mark the environment clean");
}

#[test]
fn an_impure_main_is_reported_once_and_shown_in_status() {
    let (_repo, _home_dir, home, mut player) = setup();
    player.check_sandbox().unwrap();
    let impure = player.verify_main().unwrap();
    assert_eq!(impure.keys().collect::<Vec<_>>(), ["env"]);

    let report = player.tick().unwrap();
    assert_eq!(report.main_impure.len(), 1, "{report}");
    assert!(report.main_impure[0].contains("leak.txt"), "{report}");
    let report = player.tick().unwrap();
    assert!(report.main_impure.is_empty(), "reported once: {report}");
    let kinds = event_kinds(&home);
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EventKind::MainImpure)
            .count(),
        1
    );

    let hub_state = state::derive(&Git::new(home.hub()), &|_| false).unwrap();
    assert_eq!(
        hub_state.main_impure,
        vec![("env".to_string(), vec!["?? leak.txt".to_string()])]
    );
}

#[test]
fn a_director_commit_on_an_impure_main_is_not_rejected() {
    let (repo, _home_dir, home, mut player) = setup();
    player.tick().unwrap();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!("{roadmap}\n[limits]\nmax_review_rounds = 3\n"),
    );
    let sha = commit(repo.path(), "more review rounds");
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(Git::new(home.hub()).rev_parse("refs/heads/main"), Some(sha));
}

#[test]
fn a_finish_failing_only_on_mains_impure_check_is_held_then_released() {
    held_then_released(false);
}

#[test]
fn a_released_finish_in_a_dirty_tree_is_left_to_the_worker_to_rebase() {
    held_then_released(true);
}

/// A held finish, released once main is fixed: the player rebases it onto
/// the merge in a clean tree, and leaves a `dirty` one to the worker.
fn held_then_released(dirty: bool) {
    let (repo, _home_dir, home, mut player) = setup();
    let hub = Git::new(home.hub());
    plan_and_post(&repo, &mut player);

    // The finish is right, but `env` dirties the worktree here as on main.
    let worker = worker_git(&repo);
    write_file(&worker.dir, "src/feature.txt", "done\n");
    worker.run(&["add", "src/feature.txt"]).unwrap();
    worker.run(&["commit", "-q", "-m", "finish"]).unwrap();
    let finish = worker.rev_parse("HEAD").unwrap();

    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "held, not struck: {report}");
    assert!(report.imported.is_empty(), "{report}");
    assert_eq!(report.held.len(), 1, "{report}");
    assert_eq!(records::held_commit(&hub, "a"), Some(finish.clone()));
    let told = |p: &Player<NullSessions>| {
        p.sessions()
            .sent
            .iter()
            .filter(|(s, m)| s == "a" && m.contains("held"))
            .count()
    };
    assert_eq!(told(&player), 1);

    let report = player.tick().unwrap();
    assert!(
        report.held.is_empty() && report.rejected.is_empty(),
        "{report}"
    );
    assert_eq!(told(&player), 1, "the worker is told once");
    let hub_state = state::derive(&hub, &|_| false).unwrap();
    assert_eq!(hub_state.held, vec!["a".to_string()]);

    // The director fixes main: the player merges it into task/a, and the
    // stale held finish isn't treated as a diverged clone.
    if dirty {
        write_file(&worker.dir, "src/scratch.txt", "mine\n");
    }
    fix_main(&repo);
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert!(
        report
            .imported
            .iter()
            .any(|(r, _, t)| r == "refs/heads/task/a" && *t == Transition::SyncRevision),
        "{report}"
    );
    assert_eq!(records::held_commit(&hub, "a"), None);
    let released = player
        .sessions()
        .sent
        .iter()
        .find(|(_, m)| m.contains("main's checks are fixed"))
        .unwrap()
        .clone();
    assert_eq!(released.0, "a");
    let hub_tip = hub.rev_parse("refs/heads/task/a").unwrap();
    if dirty {
        // Left as it was: the worker rebases as told.
        assert!(released.1.contains("git pull --rebase origin task/a"), "{}", released.1);
        assert_eq!(worker.rev_parse("HEAD"), Some(finish.clone()));
        let report = player.tick().unwrap();
        assert!(report.rejected.is_empty(), "{report}");
        worker
            .run(&["pull", "-q", "--rebase", "origin", "task/a"])
            .unwrap();
    } else {
        // The player rebased it: the worker's finish, on the merge.
        let head = worker.rev_parse("HEAD").unwrap();
        assert!(
            released.1.contains(&format!("your tree is at {}", &head[..12])),
            "{}",
            released.1
        );
        assert_eq!(hub.parents(&format!("{head}~1")).len(), 2, "on the merge");
        assert_eq!(hub_tip, head, "imported as it stands");
        assert_eq!(
            worker.run(&["log", "-1", "--format=%an|%cn"]).unwrap(),
            "Test Worker|dashboard-player"
        );
        // The same tick imports it as the finish.
        assert!(
            report.imported.iter().any(|(_, _, t)| *t == Transition::Finish),
            "{report}"
        );
        return;
    }
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert!(
        report
            .imported
            .iter()
            .any(|(_, _, t)| *t == Transition::Finish),
        "{report}"
    );
}

#[test]
fn a_finish_failing_on_anything_else_is_still_struck() {
    let (repo, _home_dir, _home, mut player) = setup();
    plan_and_post(&repo, &mut player);

    // No src/feature.txt: the task check fails for real, alongside `env`.
    let worker = worker_git(&repo);
    write_file(&worker.dir, "other.txt", "not it\n");
    worker.run(&["add", "--sparse", "other.txt"]).unwrap();
    worker.run(&["commit", "-q", "-m", "finish"]).unwrap();

    let report = player.tick().unwrap();
    assert!(report.held.is_empty(), "{report}");
    assert_eq!(report.rejected.len(), 1, "{report}");
}
