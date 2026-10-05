// Integration tests for parallel tasks (spec.md §4) and how they land
// (spec §11): several tasks in flight at once; the player merging a `main`
// that other accepts moved into a task before landing it; a conflict that
// stops the accept; and the director's targeted resolution of it.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::events::{self, EventKind};
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::player::{NullSessions, Player, TickReport};
use dashboard::records::{self, Actor, Transition};
use dashboard::state::{self, Status, TaskState};
use tempfile::TempDir;

fn init_hub(repo: &DirectorRepo) -> (TempDir, Home) {
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init should succeed");
    (home_dir, Home { root: report.home })
}

fn open_player(home: &Home) -> Player<NullSessions> {
    Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("player open")
}

fn task(hub: &Git, slug: &str) -> TaskState {
    let hub_state = state::derive(hub, &|_| false).expect("derive should succeed");
    hub_state.tasks.into_iter().find(|t| t.slug == slug).unwrap_or_else(|| panic!("no task state for {slug:?}"))
}

fn task_status(hub: &Git, slug: &str) -> Status {
    task(hub, slug).status
}

fn transitions(report: &TickReport) -> Vec<Transition> {
    report.imported.iter().map(|i| i.2).collect()
}

/// Plans `slug` and posts it with the task check `cmd`, in its own
/// `tests.d/<slug>.toml`: parallel posts never touch the same suite file.
fn plan_and_post(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str, cmd: &str) {
    director_commit_on_branch(repo.path(), &format!("task/{slug}"), &format!("tasks/{slug}/task.toml"), &format!("title = \"{slug}\"\nphase = \"p1\"\n"), &format!("plan: {slug}"));
    let report = player.tick().expect("plan tick");
    assert!(report.rejected.is_empty(), "{report}");

    write_file(repo.path(), &format!("tasks/{slug}/prompt.md"), "do it\n");
    write_file(repo.path(), &format!("tasks/{slug}/task.toml"), &format!("title = \"{slug}\"\nphase = \"p1\"\n\n[check]\nname = \"{slug}-check\"\ncmd = \"{cmd}\"\n"));
    write_file(repo.path(), &format!("tests.d/{slug}.toml"), &format!("[[check]]\nname = \"{slug}-check\"\ncmd = \"{cmd}\"\n"));
    commit(repo.path(), &format!("post: {slug}"));
    let report = player.tick().expect("post tick");
    assert!(report.rejected.is_empty(), "post: {slug} rejected: {report}");
    assert!(transitions(&report).contains(&Transition::Post), "{report}");
}

/// A subagent finish writing `files` (one commit each), the last recorded
/// as validated by hand.
fn finish(home: &Home, hub: &Git, slug: &str, files: &[(&str, &str)]) {
    let mut sha = String::new();
    for (file, content) in files {
        sha = subagent_commit(&home.hub(), slug, file, content, "finish work");
    }
    records::record_import(hub, &sha, Actor::Subagent, Transition::Finish, Some(slug)).expect("record finish");
    assert_eq!(task_status(hub, slug), Status::Tested);
}

fn accept(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) -> TickReport {
    director_pull_task_branch(repo.path(), slug);
    commit_empty(repo.path(), &format!("accept: {slug}"));
    player.tick().expect("accept tick")
}

/// `a` and `b` both posted at once and finished; `a` accepted first, so
/// `main` moved under `b`. With `conflict`, both also wrote `shared.txt`
/// differently, so `b` can't take `main` in cleanly.
fn a_accepted_beside_b(repo: &DirectorRepo, home: &Home, hub: &Git, player: &mut Player<NullSessions>, a_cmd: &str, conflict: bool) {
    plan_and_post(repo, player, "a", a_cmd);
    plan_and_post(repo, player, "b", "test -f b.txt");
    let shared = |who: &'static str| if conflict { vec![("shared.txt", who)] } else { Vec::new() };
    finish(home, hub, "a", &[shared("from a\n"), vec![("a.txt", "done\n")]].concat());
    finish(home, hub, "b", &[shared("from b\n"), vec![("b.txt", "done\n")]].concat());
    let report = accept(repo, player, "a");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(task_status(hub, "a"), Status::Accepted);
}

/// The director's resolution of `b`'s conflict: merges `main_commit` into
/// its branch and resolves shared.txt by keeping both lines, plus any
/// `extra` edits. Doesn't tick.
fn resolve_b(repo: &DirectorRepo, main_commit: &str, extra: &[(&str, &str)]) -> String {
    director_pull_task_branch(repo.path(), "b");
    let git = Git::new(repo.path());
    git.run(&["fetch", "-q", "hub"]).expect("fetch hub");
    assert!(git.run(&["merge", "-q", "--no-edit", main_commit]).is_err(), "shared.txt should conflict");
    write_file(repo.path(), "shared.txt", "from a\nfrom b\n");
    for (path, content) in extra {
        write_file(repo.path(), path, content);
    }
    commit(repo.path(), "resolve: merge main into task/b")
}

fn events_of(home: &Home, kind: EventKind) -> Vec<events::Event> {
    events::read_all(home).into_iter().filter(|e| e.kind == kind).collect()
}

#[test]
fn two_tasks_can_be_in_flight_at_once() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "a", "test -f a.txt");
    plan_and_post(&repo, &mut player, "b", "test -f b.txt");
    assert_eq!(player.sessions().launched.iter().map(|l| l.slug.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
}

#[test]
fn the_player_merges_main_in_and_lands_the_accept() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    a_accepted_beside_b(&repo, &home, &hub, &mut player, "test -f a.txt", false);

    let report = accept(&repo, &mut player, "b");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(transitions(&report), vec![Transition::Accept, Transition::Sync, Transition::MergeAccept]);
    assert_eq!(task_status(&hub, "b"), Status::Accepted);
    // Both posts' checks reach main without either touching the other's file.
    assert!(hub.show_file("refs/heads/main", "tests.d/a.toml").unwrap().contains("a-check"));
    assert!(hub.show_file("refs/heads/main", "tests.d/b.toml").unwrap().contains("b-check"));
}

#[test]
fn a_merge_that_fails_the_suite_goes_back_to_the_subagent() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    // a's check, now on main, is one b's finished work breaks.
    a_accepted_beside_b(&repo, &home, &hub, &mut player, "test -f a.txt && test ! -f b.txt", false);
    let main_before = hub.rev_parse("refs/heads/main");

    let report = accept(&repo, &mut player, "b");
    assert!(report.rejected.is_empty(), "no strike: the merge, not the director, broke it: {report}");
    assert_eq!(transitions(&report), vec![Transition::Accept, Transition::SyncRevision]);
    assert_eq!(hub.rev_parse("refs/heads/main"), main_before);

    let b = task(&hub, "b");
    assert_eq!(b.status, Status::InRevision);
    assert_eq!(b.review_round, 0, "a merge spends no review round");
    assert_eq!(hub.rev_parse("refs/dashboard/pins/b").as_deref(), Some(b.tip.as_str()), "re-pinned at the merge");
    let relayed = player.sessions().sent.iter().find(|(s, _)| s == "b").map(|(_, m)| m.clone()).expect("relayed to b");
    assert!(relayed.contains("main was merged") && relayed.contains("a-check"), "{relayed}");
    assert_eq!(events_of(&home, EventKind::MergeFailed).len(), 1);
}

#[test]
fn a_conflict_stops_the_accept_and_a_targeted_resolution_lands_it() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    a_accepted_beside_b(&repo, &home, &hub, &mut player, "test -f a.txt", true);
    let main_tip = hub.rev_parse("refs/heads/main").unwrap();

    let report = accept(&repo, &mut player, "b");
    assert!(report.rejected.is_empty(), "a conflict is no strike: {report}");
    assert_eq!(transitions(&report), vec![Transition::Accept]);
    let b = task(&hub, "b");
    assert_eq!(b.status, Status::Conflicted);
    let conflict = b.conflict.expect("conflict recorded");
    assert_eq!(conflict.main, main_tip);
    assert_eq!(conflict.paths, vec!["shared.txt".to_string()]);
    let ev = events_of(&home, EventKind::Conflict);
    assert_eq!(ev.len(), 1);
    assert!(ev[0].message.contains(&main_tip) && ev[0].message.contains("shared.txt"), "{}", ev[0].message);

    resolve_b(&repo, &main_tip, &[]);
    let report = player.tick().expect("resolve tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(transitions(&report), vec![Transition::Resolve, Transition::MergeAccept]);
    assert_eq!(task_status(&hub, "b"), Status::Accepted);
    assert!(hub.commit_body("refs/heads/main").unwrap_or_default().contains("Task: b"));
}

#[test]
fn a_resolution_must_merge_the_recorded_main_commit() {
    let repo = director_repo();
    write_file(repo.path(), "roadmap.toml", &format!("title = \"Test Project\"\ndocs_paths = [\"docs\"]\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n{NO_TASK_AGENTS}"));
    commit(repo.path(), "declare docs_paths");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    a_accepted_beside_b(&repo, &home, &hub, &mut player, "test -f a.txt", true);
    let conflicted_with = hub.rev_parse("refs/heads/main").unwrap();
    accept(&repo, &mut player, "b");
    assert_eq!(task_status(&hub, "b"), Status::Conflicted);

    // main moves again after the conflict was recorded.
    let git = Git::new(repo.path());
    checkout_branch(repo.path(), "main");
    git.run(&["fetch", "-q", "hub"]).unwrap();
    git.run(&["reset", "-q", "--hard", "hub/main"]).unwrap();
    write_file(repo.path(), "docs/notes.md", "a lesson\n");
    commit(repo.path(), "docs: a lesson");
    assert!(player.tick().expect("docs tick").rejected.is_empty());
    let newer_main = hub.rev_parse("refs/heads/main").unwrap();

    // Merging the newer main isn't resolving the recorded conflict.
    resolve_b(&repo, &newer_main, &[]);
    let report = player.tick().expect("resolve tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(report.rejected[0].message.contains(&conflicted_with), "{}", report.rejected[0].message);

    // The targeted one is; the player then merges the newer main itself.
    git.run(&["reset", "-q", "--hard", "hub/task/b"]).unwrap();
    resolve_b(&repo, &conflicted_with, &[]);
    let report = player.tick().expect("resolve tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(transitions(&report), vec![Transition::Resolve, Transition::Sync, Transition::MergeAccept]);
    assert_eq!(task_status(&hub, "b"), Status::Accepted);
    assert_eq!(hub.show_file("refs/heads/main", "docs/notes.md").as_deref(), Some("a lesson"));
}

#[test]
fn a_resolution_may_not_change_paths_git_merged_cleanly() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    a_accepted_beside_b(&repo, &home, &hub, &mut player, "test -f a.txt", true);
    let main_tip = hub.rev_parse("refs/heads/main").unwrap();
    accept(&repo, &mut player, "b");
    let tip_before = hub.rev_parse("refs/heads/task/b");

    resolve_b(&repo, &main_tip, &[("a.txt", "the director rewriting a's code\n")]);
    let report = player.tick().expect("resolve tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert!(report.rejected[0].message.contains("a.txt"), "{}", report.rejected[0].message);
    assert_eq!(hub.rev_parse("refs/heads/task/b"), tip_before);
    assert_eq!(task_status(&hub, "b"), Status::Conflicted);
}

#[test]
fn a_director_merge_without_a_recorded_conflict_is_history() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    a_accepted_beside_b(&repo, &home, &hub, &mut player, "test -f a.txt", true);

    // b is tested and behind main, but merging main in is the player's job.
    director_pull_task_branch(repo.path(), "b");
    let git = Git::new(repo.path());
    git.run(&["fetch", "-q", "hub"]).unwrap();
    assert!(git.run(&["merge", "-q", "--no-edit", "hub/main"]).is_err());
    write_file(repo.path(), "shared.txt", "from a\nfrom b\n");
    commit(repo.path(), "unasked merge");

    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::History);
    assert!(report.rejected[0].message.contains("player merges main itself"), "{}", report.rejected[0].message);
    assert_eq!(task_status(&hub, "b"), Status::Tested);
}
