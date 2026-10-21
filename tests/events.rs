// Integration tests for step 7 (spec.md's "the director must not poll"):
// event emission from `Player::tick` (src/player.rs's `emit_events`) and the
// `dashboard wait` CLI (src/main.rs's `run_wait`) that reads them. Unit
// coverage for the pure event-log mechanics (append/read/cursor/heartbeat)
// lives in src/events.rs; this file drives the whole pipeline: a real
// director repo, a real hub via `dashboard::init::init`, `Player::tick` with
// `NullSessions`, and — for `dashboard wait` — the built binary.

mod common;

use common::*;
use dashboard::events::{self, EventKind};
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{self, NullSessions, Player, Sessions};
use dashboard::records::Transition;
use dashboard::state::{self, Status};
use std::path::Path;
use tempfile::TempDir;

// --------------------------------------------------------------------------
// Fixture helpers (same shape as tests/player_worker.rs)
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

/// Posts `slug` (already planned and `ready`) with a task check that fails
/// until `feature-<slug>.txt` shows up (slug-specific, since an accepted
/// task's files land on `main` and a later task's branch is cut from it —
/// two tasks sharing one filename would make the second's check pass before
/// any work happened), and ticks (which also launches the worker, per step
/// 6).
fn post(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) {
    let feature = format!("feature-{slug}.txt");
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
            "title = \"{slug}\"\nphase = \"p1\"\nmodules = [\"app\"]\n\n[check]\nname = \"{slug}-check\"\ncmd = \"test -f {feature}\"\n"
        ),
    );
    write_file(
        repo.path(),
        &*tests_file(&slug),
        &*task_suite_lean(&format!("[[check]]\nname = \"{slug}-check\"\ncmd = \"test -f {feature}\"\n")),
    );
    commit(repo.path(), &format!("post: {slug}"));
    let report = player.tick().expect("post tick should succeed");
    assert_eq!(
        report.imported.len(),
        1,
        "post: {slug} not imported: {report}"
    );
}

fn ensure_identity(dir: &Path) {
    let git = Git::new(dir);
    git.run(&["config", "user.name", "Test Worker"])
        .expect("set user.name");
    git.run(&["config", "user.email", "worker@example.com"])
        .expect("set user.email");
    git.run(&["config", "commit.gpgsign", "false"])
        .expect("disable gpgsign");
}

fn commit_in_clone(repo: &DirectorRepo, slug: &str, rel: &str, content: &str, msg: &str) -> String {
    let dir = paths::worker_clone(&repo.director_path(), slug);
    ensure_identity(&dir);
    write_file(&dir, rel, content);
    commit(&dir, msg)
}

/// Fast-forwards the director's local `main` onto the hub's (an acceptance
/// merge only ever happens in the bare hub — the director must pull it
/// before planning a new branch from `main`, or the new branch's diff will
/// include everything main gained since the director's local copy went
/// stale).
fn director_pull_main(repo: &Path) {
    checkout_branch(repo, "main");
    let git = Git::new(repo);
    git.run(&["fetch", "-q", "hub", "main"])
        .expect("fetch main from hub");
    git.run(&["merge", "-q", "--ff-only", "FETCH_HEAD"])
        .expect("fast-forward local main onto hub's main");
}

fn block_in_clone(repo: &DirectorRepo, slug: &str, msg: &str) -> String {
    let dir = paths::worker_clone(&repo.director_path(), slug);
    ensure_identity(&dir);
    commit_empty(&dir, msg)
}

fn events_for(home: &Home, slug: &str, kind: EventKind) -> Vec<events::Event> {
    events::read_all(home)
        .into_iter()
        .filter(|e| e.kind == kind && e.slug.as_deref() == Some(slug))
        .collect()
}

// --------------------------------------------------------------------------
// Full flow: plan rejection, finish -> Tested, accept -> Accepted, an
// unblocked dependent -> Ready, a block -> Escalated, then Retired.
// --------------------------------------------------------------------------

#[test]
fn player_tick_emits_the_full_set_of_director_relevant_events() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    // ---- DirectorRejected: an invalid slug (spec §9's plan row) ----
    director_commit_on_branch(
        repo.path(),
        "task/Bad_Slug",
        "tasks/Bad_Slug/task.toml",
        "title = \"Bad\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
        "plan: Bad_Slug",
    );
    player.tick().expect("tick should succeed");
    let rejected = events_for(&home, "Bad_Slug", EventKind::DirectorRejected);
    assert_eq!(rejected.len(), 1, "{:?}", events::read_all(&home));
    assert!(
        rejected[0].message.contains("plan"),
        "{}",
        rejected[0].message
    );

    // ---- Tested: dep's finish claim passes the suite ----
    plan(
        &repo,
        &mut player,
        "dep",
        "title = \"Dep\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    plan(
        &repo,
        &mut player,
        "depender",
        "title = \"Depender\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"dep\"]\n",
    );
    post(&repo, &mut player, "dep");
    commit_in_clone(&repo, "dep", "feature-dep.txt", "done\n", "finish work");
    player.tick().expect("finish tick should succeed");
    assert_eq!(task_status(&hub, "dep"), Status::Tested);
    let tested = events_for(&home, "dep", EventKind::Tested);
    assert_eq!(tested.len(), 1, "{:?}", events::read_all(&home));
    assert!(
        tested[0].message.contains("awaiting director review"),
        "{}",
        tested[0].message
    );

    // ---- Accepted (dep) + Ready (depender, unblocked in the same tick) ----
    director_pull_task_branch(repo.path(), "dep");
    commit_empty(repo.path(), "accept: looks good");
    player.tick().expect("accept tick should succeed");
    assert_eq!(task_status(&hub, "dep"), Status::Accepted);
    assert_eq!(task_status(&hub, "depender"), Status::Ready);
    assert_eq!(
        events_for(&home, "dep", EventKind::Accepted).len(),
        1,
        "{:?}",
        events::read_all(&home)
    );
    assert_eq!(
        events_for(&home, "depender", EventKind::Ready).len(),
        1,
        "{:?}",
        events::read_all(&home)
    );

    // ---- Escalated: a block ----
    // `main` advanced in the hub (the accept merge above); the director must
    // pull it before planning a new branch from it.
    director_pull_main(repo.path());
    plan(
        &repo,
        &mut player,
        "blk",
        "title = \"Blocked\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "blk");
    block_in_clone(
        &repo,
        "blk",
        "block: cannot be done\n\nThe API this needs doesn't exist.",
    );
    player.tick().expect("block tick should succeed");
    assert_eq!(task_status(&hub, "blk"), Status::Escalated);
    let escalated = events_for(&home, "blk", EventKind::Escalated);
    assert_eq!(escalated.len(), 1, "{:?}", events::read_all(&home));
    assert!(
        escalated[0].message.contains("doesn't exist"),
        "{}",
        escalated[0].message
    );

    // ---- Retired ----
    // The director's local task/blk branch is stale after the block (which
    // happened in the worker clone, not the director's); pull it first so
    // the retire marker builds on the hub's actual tip.
    checkout_branch(repo.path(), "task/blk");
    let git = repo.git();
    git.run(&["fetch", "-q", "hub", "task/blk"])
        .expect("fetch task branch from hub");
    git.run(&["reset", "-q", "--hard", "FETCH_HEAD"])
        .expect("reset onto hub's task/blk tip");
    commit_empty(
        repo.path(),
        "retire: didn't pan out\n\nThe API this needs doesn't exist and won't soon.",
    );
    let report = player.tick().expect("retire tick should succeed");
    assert_eq!(
        report
            .imported
            .iter()
            .filter(|(_, _, t)| *t == Transition::Retire)
            .count(),
        1,
        "{report}"
    );
    assert_eq!(task_status(&hub, "blk"), Status::Retired);
    let retired = events_for(&home, "blk", EventKind::Retired);
    assert_eq!(retired.len(), 1, "{:?}", events::read_all(&home));
    // The post-mortem is the marker commit's *body* (spec §5): the subject
    // line ("retire: ...") is the transition marker itself, not the message.
    assert_eq!(
        retired[0].message,
        "The API this needs doesn't exist and won't soon."
    );
}

// --------------------------------------------------------------------------
// No re-emission across a player restart (state file persisted)
// --------------------------------------------------------------------------

#[test]
fn no_reemission_after_reconstructing_the_player() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(&repo, &mut player, "x", "title = \"X\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    post(&repo, &mut player, "x");
    commit_in_clone(&repo, "x", "feature-x.txt", "done\n", "finish work");
    player.tick().expect("finish tick should succeed");
    assert_eq!(task_status(&hub, "x"), Status::Tested);
    assert_eq!(events_for(&home, "x", EventKind::Tested).len(), 1);

    // Drop the player, reopen fresh against the same (persisted) home, tick
    // again with no hub changes: the status hasn't changed, so nothing new
    // should be emitted.
    drop(player);
    let mut player = open_player(&home);
    let count_before = events::read_all(&home).len();
    player
        .tick()
        .expect("tick after reconstruction should succeed");
    let count_after = events::read_all(&home).len();
    assert_eq!(
        count_after, count_before,
        "reconstructing the player must not re-emit unchanged statuses"
    );
    assert_eq!(
        events_for(&home, "x", EventKind::Tested).len(),
        1,
        "still exactly the one Tested event"
    );
}

// --------------------------------------------------------------------------
// A first run with no state file stays silent about pre-existing statuses
// --------------------------------------------------------------------------

#[test]
fn first_run_with_no_state_file_emits_nothing_for_preexisting_statuses() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "already",
        "title = \"Already\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "already");
    commit_in_clone(
        &repo,
        "already",
        "feature-already.txt",
        "done\n",
        "finish work",
    );
    player.tick().expect("finish tick should succeed");
    assert_eq!(task_status(&hub, "already"), Status::Tested);
    assert_eq!(events_for(&home, "already", EventKind::Tested).len(), 1);

    // Simulate losing the persisted player state (e.g. a fresh install of
    // the player pointed at an existing hub) — the state file is gone, so
    // the next Player::open sees no prior `last` at all.
    std::fs::remove_file(home.root.join("player-state.json")).expect("remove player-state.json");
    drop(player);
    let mut fresh = open_player(&home);

    let count_before = events::read_all(&home).len();
    fresh
        .tick()
        .expect("first-run tick after losing state should succeed");
    let count_after = events::read_all(&home).len();
    assert_eq!(
        count_after, count_before,
        "the first run after losing state must not re-emit already-Tested as new"
    );
    assert_eq!(
        events_for(&home, "already", EventKind::Tested).len(),
        1,
        "still exactly the original Tested event"
    );
}

// --------------------------------------------------------------------------
// SessionDied
// --------------------------------------------------------------------------

#[test]
fn session_dying_while_in_progress_emits_session_died() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan(
        &repo,
        &mut player,
        "die",
        "title = \"Die\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "die");
    assert!(
        player.sessions().alive("die"),
        "post should have launched a worker session"
    );

    // Simulate the session dying on its own — not via `kill` (accept/retire)
    // — by removing it from `NullSessions::alive` directly, exactly as that
    // struct's own doc comment describes.
    player.sessions_mut().alive.remove("die");

    player.tick().expect("tick should succeed");
    let died = events_for(&home, "die", EventKind::SessionDied);
    assert_eq!(died.len(), 1, "{:?}", events::read_all(&home));
}

#[test]
fn killing_a_session_via_accept_does_not_emit_session_died() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan(&repo, &mut player, "ok", "title = \"Ok\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    post(&repo, &mut player, "ok");
    commit_in_clone(&repo, "ok", "feature-ok.txt", "done\n", "finish work");
    player.tick().expect("finish tick should succeed");

    director_pull_task_branch(repo.path(), "ok");
    commit_empty(repo.path(), "accept: looks good");
    player.tick().expect("accept tick should succeed");
    assert_eq!(task_status(&hub, "ok"), Status::Accepted);
    assert!(
        events_for(&home, "ok", EventKind::SessionDied).is_empty(),
        "an accept-time kill isn't a session death"
    );
}

#[test]
fn a_session_dead_before_any_tick_saw_it_is_reported_once_per_launch() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan(&repo, &mut player, "sb", "title = \"Sb\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    // The worker's command crashes at startup: dead by the end of the very
    // tick that launched it, never once seen alive.
    player.sessions_mut().stillborn.insert("sb".to_string());
    post(&repo, &mut player, "sb");
    assert_eq!(player.sessions().launched.len(), 1);
    assert_eq!(events_for(&home, "sb", EventKind::SessionDied).len(), 1);

    player.tick().expect("tick");
    assert_eq!(
        events_for(&home, "sb", EventKind::SessionDied).len(),
        1,
        "the same death isn't reported again"
    );
    drop(player);
    let mut player = open_player(&home);
    player.tick().expect("tick after a restart");
    assert_eq!(
        events_for(&home, "sb", EventKind::SessionDied).len(),
        1,
        "nor after a player restart"
    );

    // `dashboard relaunch` runs in its own process: its session dies again
    // before the long-running player ever sees it alive.
    open_player(&home).relaunch("sb").expect("relaunch");
    player.tick().expect("tick after the relaunch");
    assert_eq!(
        events_for(&home, "sb", EventKind::SessionDied).len(),
        2,
        "a relaunch that dies is a new death"
    );
    player.tick().expect("tick");
    assert_eq!(events_for(&home, "sb", EventKind::SessionDied).len(), 2);
}

/// Plays the worker's `Stop`/`StopFailure` hook: `line` is what it writes.
fn worker_stops(repo: &DirectorRepo, slug: &str, line: &str) -> std::path::PathBuf {
    let marker = player::stop_marker(&paths::worker_clone(&repo.director_path(), slug))
        .expect("stop marker path");
    std::fs::write(&marker, format!("{line}\n")).expect("write stop marker");
    marker
}

#[test]
fn a_worker_stopped_while_it_owes_a_finish_is_reported_idle_once_per_stop() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(
        &repo,
        &mut player,
        "idl",
        "title = \"Idl\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "idl");

    // The launch installed the hooks without dirtying the worktree.
    let clone = paths::worker_clone(&repo.director_path(), "idl");
    let settings = std::fs::read_to_string(clone.join(".claude/settings.local.json"))
        .expect("the launch writes the worktree's Claude Code settings");
    let marker = player::stop_marker(&clone).expect("stop marker path");
    for event in ["\"Stop\"", "\"StopFailure\"", "\"UserPromptSubmit\""] {
        assert!(settings.contains(event), "{settings}");
    }
    assert!(settings.contains(&*marker.to_string_lossy()), "{settings}");
    assert_eq!(
        Git::new(&clone).run(&["status", "--porcelain"]).unwrap(),
        "",
        "the settings file is invisible to git"
    );
    let exclude = Git::new(&clone)
        .run(&["rev-parse", "--git-path", "info/exclude"])
        .unwrap();
    let exclude = std::fs::read_to_string(clone.join(exclude)).unwrap();
    assert!(
        exclude.lines().any(|l| l == "/.claude/settings.local.json"),
        "not just a host's global excludes: {exclude}"
    );

    player.tick().expect("tick");
    assert!(events_for(&home, "idl", EventKind::SessionIdle).is_empty());

    // It ends its turn without a finish: reported once, not again on later
    // ticks or after a player restart.
    worker_stops(&repo, "idl", "stop 100 1");
    player.tick().expect("tick");
    let idle = events_for(&home, "idl", EventKind::SessionIdle);
    assert_eq!(idle.len(), 1, "{:?}", events::read_all(&home));
    assert!(
        idle[0].message.contains("dash-proj-idl"),
        "{}",
        idle[0].message
    );
    player.tick().expect("tick");
    drop(player);
    let mut player = open_player(&home);
    // Its session outlived the player.
    player.sessions_mut().alive.insert("idl".to_string());
    player.tick().expect("tick after a restart");
    assert_eq!(events_for(&home, "idl", EventKind::SessionIdle).len(), 1);

    // A relay (here, a failing finish's rejection) re-arms it: the agent
    // resumes, and its next stop is a new one.
    commit_in_clone(&repo, "idl", "other.txt", "x\n", "finish: not really");
    player.tick().expect("rejection tick");
    assert!(player.sessions().sent.iter().any(|(k, _)| k == "idl"));
    assert!(!marker.exists(), "a relay clears the stop");
    worker_stops(&repo, "idl", "error 101 2");
    player.tick().expect("tick");
    let idle = events_for(&home, "idl", EventKind::SessionIdle);
    assert_eq!(idle.len(), 2, "{:?}", events::read_all(&home));
    assert!(idle[1].message.contains("API error"), "{}", idle[1].message);

    // Stopping after a finish is waiting for review: nothing to report.
    commit_in_clone(&repo, "idl", "feature-idl.txt", "done\n", "finish work");
    player.tick().expect("finish tick");
    assert_eq!(task_status(&hub, "idl"), Status::Tested);
    worker_stops(&repo, "idl", "stop 102 3");
    player.tick().expect("tick");
    assert_eq!(events_for(&home, "idl", EventKind::SessionIdle).len(), 2);
}

/// Shows `screen` as session `key`'s pane.
fn show(player: &mut Player<NullSessions>, key: &str, screen: &str) {
    player
        .sessions_mut()
        .panes
        .insert(key.to_string(), screen.to_string());
}

#[test]
fn a_worker_whose_screen_goes_stale_while_it_owes_a_finish_is_reported_idle_once() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(
        &repo,
        &mut player,
        "stl",
        "title = \"Stl\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "stl");
    let idle = || events_for(&home, "stl", EventKind::SessionIdle);

    // The default `idle_after` is 900 s: not before, once at it.
    show(&mut player, "stl", "* Working (3s)\n\n\n");
    player.set_clock(1000);
    player.tick().expect("tick");
    player.set_clock(1899);
    player.tick().expect("tick");
    assert!(idle().is_empty());
    player.set_clock(1900);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1, "{:?}", events::read_all(&home));
    assert!(
        idle()[0].message.contains("no new output for 15 min"),
        "{}",
        idle()[0].message
    );
    assert!(idle()[0].message.contains("dash-proj-stl"));
    player.set_clock(5000);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1);

    // A restart keeps both the report and the clock.
    drop(player);
    let mut player = open_player(&home);
    player.sessions_mut().alive.insert("stl".to_string());
    show(&mut player, "stl", "* Working (3s)");
    player.set_clock(6000);
    player.tick().expect("tick after a restart");
    assert_eq!(idle().len(), 1, "trailing blank lines don't count");

    // New output re-arms it, and the clock starts over.
    show(&mut player, "stl", "* Working (4s)");
    player.tick().expect("tick");
    player.set_clock(6899);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1);
    player.set_clock(6900);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 2);

    // So does a relay, even onto an identical screen.
    commit_in_clone(&repo, "stl", "other.txt", "x\n", "finish: not really");
    player.tick().expect("rejection tick");
    player.set_clock(7799);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 2);
    player.set_clock(7800);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 3);

    // Finished, it's waiting for review: a still screen is no news.
    commit_in_clone(&repo, "stl", "feature-stl.txt", "done\n", "finish work");
    player.tick().expect("finish tick");
    assert_eq!(task_status(&hub, "stl"), Status::Tested);
    player.set_clock(20_000);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 3, "{:?}", events::read_all(&home));
}

#[test]
fn a_stale_screen_and_a_stop_are_one_idle_report() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(
        &repo,
        &mut player,
        "one",
        "title = \"One\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "one");
    let idle = || events_for(&home, "one", EventKind::SessionIdle);

    // A stop is reported by the hook; its screen going stale isn't news.
    show(&mut player, "one", "> ");
    player.set_clock(1000);
    worker_stops(&repo, "one", "stop 1000 1");
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1);
    assert!(idle()[0].message.contains("ended its turn"));
    player.set_clock(5000);
    player.tick().expect("tick");
    // Not even if an idle clock redraws it while it sits there.
    show(&mut player, "one", "> 12:00");
    player.tick().expect("tick");
    player.set_clock(9000);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1, "{:?}", events::read_all(&home));

    // The other way round: stale first (a hang), then a stop on that same
    // screen. Reported once.
    let clone = paths::worker_clone(&repo.director_path(), "one");
    std::fs::remove_file(player::stop_marker(&clone).unwrap()).unwrap();
    show(&mut player, "one", "* Working");
    player.tick().expect("tick");
    player.set_clock(9900);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 2);
    assert!(idle()[1].message.contains("no new output"));
    worker_stops(&repo, "one", "stop 9950 2");
    player.set_clock(9950);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 2, "{:?}", events::read_all(&home));
}

#[test]
fn idle_after_zero_turns_stale_screen_detection_off() {
    let repo = director_repo();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!("{roadmap}idle_after = 0\n"),
    );
    commit(repo.path(), "no stale-screen detection");
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(
        &repo,
        &mut player,
        "off",
        "title = \"Off\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "off");

    show(&mut player, "off", "* Working");
    player.set_clock(1000);
    player.tick().expect("tick");
    player.set_clock(100_000);
    player.tick().expect("tick");
    assert!(events_for(&home, "off", EventKind::SessionIdle).is_empty());
}

#[test]
fn the_stop_hook_merges_into_existing_settings() {
    let repo = director_repo();
    // A project that tracks its own local settings, with a hook of its own.
    write_file(
        repo.path(),
        ".claude/settings.local.json",
        r#"{"model": "opus", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "true"}]}]}}"#,
    );
    // Forced: a host's global excludes may ignore it (Claude Code's do).
    repo.git()
        .run(&["add", "-f", ".claude/settings.local.json"])
        .expect("git add -f");
    commit(repo.path(), "project settings");
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player, "mg", "title = \"Mg\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    post(&repo, &mut player, "mg");

    let clone = paths::worker_clone(&repo.director_path(), "mg");
    let text = std::fs::read_to_string(clone.join(".claude/settings.local.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).expect("still JSON");
    assert_eq!(json["model"], "opus");
    let stop = json["hooks"]["Stop"].as_array().expect("Stop hooks");
    assert_eq!(stop.len(), 2, "theirs kept, ours added: {text}");
    assert_eq!(stop[0]["hooks"][0]["command"], "true");
    assert_eq!(
        Git::new(&clone).run(&["status", "--porcelain"]).unwrap(),
        "",
        "a tracked settings file is skipped, not shown as modified"
    );

    // A relaunch replaces our hooks instead of adding a second copy.
    player.sessions_mut().alive.remove("mg");
    player.relaunch("mg").expect("relaunch");
    let text = std::fs::read_to_string(clone.join(".claude/settings.local.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["hooks"]["Stop"].as_array().unwrap().len(), 2, "{text}");
}

#[test]
fn a_player_state_in_the_old_format_is_migrated() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player, "a", "title = \"A\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    post(&repo, &mut player, "a");
    plan(&repo, &mut player, "b", "title = \"B\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    post(&repo, &mut player, "b");
    drop(player);

    // Written by a player from before launch ids: `a` last seen alive, `b`
    // already dead (and so already reported, or never seen alive).
    std::fs::remove_dir_all(home.launches()).expect("remove launch ids");
    std::fs::write(
        home.player_state(),
        r#"{"a":["in_progress",true],"b":["posted",false]}"#,
    )
    .expect("write old player state");
    let before = events::read_all(&home).len();
    let mut player = open_player(&home);
    player.tick().expect("tick");
    assert_eq!(events_for(&home, "a", EventKind::SessionDied).len(), 1);
    assert!(events_for(&home, "b", EventKind::SessionDied).is_empty());
    let kinds: Vec<EventKind> = events::read_all(&home)[before..]
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![EventKind::SessionDied],
        "statuses carry over: nothing else is re-emitted"
    );
}

// --------------------------------------------------------------------------
// `dashboard wait` (the CLI): no director/hub needed — it only reads/writes
// plain files under `home.root`.
// --------------------------------------------------------------------------

fn plain_home() -> (TempDir, Home) {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path().to_path_buf();
    (dir, Home { root })
}

#[test]
fn wait_timeout_exits_2_with_no_events() {
    let (dir, _home) = plain_home();
    let home_str = dir.path().to_str().expect("valid utf-8 path");
    let output = run_dashboard(&["--home", home_str, "wait", "--timeout", "1"], dir.path());
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("no events within 1 s"));
}

#[test]
fn wait_prints_pending_events_then_a_second_wait_times_out_after_the_cursor_advances() {
    let (dir, home) = plain_home();
    events::append(&home, EventKind::Tested, Some("x"), "finish accepted").expect("append");
    events::append(&home, EventKind::Accepted, Some("x"), "merged").expect("append");

    let home_str = dir.path().to_str().expect("valid utf-8 path");
    let output = run_dashboard(&["--home", home_str, "wait"], dir.path());
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("#1 tested"), "{stdout}");
    assert!(stdout.contains("#2 accepted"), "{stdout}");
    assert_eq!(events::read_cursor(&home), 2);

    let output2 = run_dashboard(&["--home", home_str, "wait", "--timeout", "1"], dir.path());
    assert_eq!(
        output2.status.code(),
        Some(2),
        "cursor should have advanced past both events"
    );
}

#[test]
fn wait_all_prints_history_without_moving_the_cursor() {
    let (dir, home) = plain_home();
    events::append(&home, EventKind::Tested, Some("x"), "finish accepted").expect("append");

    let home_str = dir.path().to_str().expect("valid utf-8 path");
    let output = run_dashboard(&["--home", home_str, "wait", "--all"], dir.path());
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("#1 tested"));
    assert_eq!(
        events::read_cursor(&home),
        0,
        "--all must not move the cursor"
    );

    // A normal wait afterwards should still see event #1.
    let output2 = run_dashboard(&["--home", home_str, "wait", "--timeout", "1"], dir.path());
    assert!(
        output2.status.success(),
        "the event should still be unread after --all"
    );
}

#[test]
fn wait_blocks_then_returns_once_an_event_is_appended_from_another_thread() {
    let (dir, home) = plain_home();
    let home_for_thread = Home {
        root: home.root.clone(),
    };
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1000));
        events::append(
            &home_for_thread,
            EventKind::Tested,
            Some("late"),
            "finish accepted",
        )
        .expect("append from thread");
    });

    let home_str = dir.path().to_str().expect("valid utf-8 path");
    let start = std::time::Instant::now();
    let output = run_dashboard(&["--home", home_str, "wait", "--timeout", "5"], dir.path());
    let elapsed = start.elapsed();
    handle.join().expect("appender thread should not panic");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(800),
        "wait returned too early: {elapsed:?}"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("late"));
}

// --------------------------------------------------------------------------
// The evidence in session-health events: screen tails, dead panes
// --------------------------------------------------------------------------

#[test]
fn idle_events_carry_the_screen_tail() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(
        &repo,
        &mut player,
        "scr",
        "title = \"Scr\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "scr");
    let idle = || events_for(&home, "scr", EventKind::SessionIdle);

    // The stop-hook path: the screen's last non-blank lines, trimmed.
    let screen: String = (1..=40).map(|i| format!("line {i}   \n\n")).collect();
    show(&mut player, "scr", &screen);
    player.set_clock(1000);
    player.tick().expect("tick");
    worker_stops(&repo, "scr", "stop 100 1");
    player.tick().expect("tick");
    assert_eq!(idle().len(), 1, "{:?}", events::read_all(&home));
    let message = &idle()[0].message;
    let tail = message.split_once("\nlast screen:\n").expect(message).1;
    let want: Vec<String> = (16..=40).map(|i| format!("  line {i}")).collect();
    assert_eq!(tail, format!("  …\n{}", want.join("\n")));

    // The stale-pane path, on a screen that changed since.
    commit_in_clone(&repo, "scr", "other.txt", "x\n", "finish: not really");
    player.tick().expect("rejection tick");
    show(
        &mut player,
        "scr",
        "Do you trust this folder?\n> Yes\n  No\n\n",
    );
    player.set_clock(2000);
    player.tick().expect("tick");
    player.set_clock(3000);
    player.tick().expect("tick");
    assert_eq!(idle().len(), 2, "{:?}", events::read_all(&home));
    let message = &idle()[1].message;
    assert!(message.contains("no new output"), "{message}");
    assert!(
        message.ends_with("\nlast screen:\n  Do you trust this folder?\n  > Yes\n    No"),
        "{message}"
    );
}

#[test]
fn an_idle_event_with_an_empty_screen_says_so() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(
        &repo,
        &mut player,
        "emp",
        "title = \"Emp\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "emp");
    player.tick().expect("tick");
    worker_stops(&repo, "emp", "stop 100 1");
    player.tick().expect("tick");
    let idle = events_for(&home, "emp", EventKind::SessionIdle);
    assert_eq!(idle.len(), 1, "{:?}", events::read_all(&home));
    assert!(
        idle[0].message.ends_with("\nno screen captured"),
        "{}",
        idle[0].message
    );
}

/// Kills session `key` the way a crash does: its pane lingers, dead, with
/// `screen` on it and exit `status`.
fn crash(player: &mut Player<NullSessions>, key: &str, screen: &str, status: i32) {
    let sessions = player.sessions_mut();
    sessions.alive.remove(key);
    sessions.lingering.insert(
        key.to_string(),
        player::Remains {
            screen: Some(screen.to_string()),
            status: Some(status),
            signal: None,
        },
    );
}

#[test]
fn a_died_event_carries_the_dead_panes_output_and_exit_status_then_reaps_it() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player, "cr", "title = \"Cr\"\nphase = \"p1\"\nmodules = [\"app\"]\n");
    post(&repo, &mut player, "cr");
    player.tick().expect("tick");

    crash(&mut player, "cr", "Error: invalid API key\n\n\n", 1);
    assert!(
        !player.sessions().alive("cr"),
        "a lingering dead pane is dead"
    );
    player.tick().expect("tick");
    let died = events_for(&home, "cr", EventKind::SessionDied);
    assert_eq!(died.len(), 1, "{:?}", events::read_all(&home));
    assert_eq!(
        died[0].message,
        "task/cr's worker session died (exit status 1)\nlast screen:\n  Error: invalid API key"
    );
    assert_eq!(player.sessions().reaped, vec!["cr".to_string()]);
    assert!(
        player.sessions().lingering.is_empty(),
        "reaped after reporting"
    );

    // A relaunch from another process (`dashboard relaunch`) before the
    // player reaped it: dead, so allowed, and the launch replaces it.
    let mut other = open_player(&home);
    crash(&mut other, "cr", "Segmentation fault\n", 139);
    other
        .relaunch("cr")
        .expect("a lingering dead pane can be relaunched");
    assert!(other.sessions().lingering.is_empty());
    assert!(other.sessions().alive("cr"));

    // A death with nothing left behind still says so.
    player.tick().expect("tick after the relaunch");
    assert_eq!(events_for(&home, "cr", EventKind::SessionDied).len(), 2);
    let died = events_for(&home, "cr", EventKind::SessionDied);
    assert_eq!(
        died[1].message,
        "task/cr's worker session died\nno screen captured"
    );
}

#[test]
fn a_dead_session_nobody_watches_is_still_reaped() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(
        &repo,
        &mut player,
        "fin",
        "title = \"Fin\"\nphase = \"p1\"\nmodules = [\"app\"]\n",
    );
    post(&repo, &mut player, "fin");
    commit_in_clone(&repo, "fin", "feature-fin.txt", "done\n", "finish work");
    player.tick().expect("finish tick");
    assert_eq!(task_status(&hub, "fin"), Status::Tested);

    // Waiting for review, it exits: not a death worth reporting, but its
    // pane doesn't linger either.
    crash(&mut player, "fin", "bye\n", 0);
    player.tick().expect("tick");
    assert!(events_for(&home, "fin", EventKind::SessionDied).is_empty());
    assert!(player.sessions().lingering.is_empty());
}
