// Integration tests for `staged`: a `ready` task never starts on its own; a
// director's `stage:` marker releases it (status `staged`, its task agent is
// launched). The status survives `main` moving under the branch (the
// player's `merge_unblock` merge becomes the tip). `unstage:`
// puts it back while no task agent exists yet.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::player::{NullSessions, Player, TickReport};
use dashboard::records::{self, Actor, Transition};
use dashboard::state::{self, Status};
use tempfile::TempDir;

const PLAN: &str = "title = \"s\"\nphase = \"p1\"\nmodules = [\"app\"]\n";
const ROADMAP: &str = "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\ngoal = \"ship the widget\"\n\n[player]\nruns = \"parallel\"\ntask_agent_cmd = \"fake-task-agent\"\n";

fn setup() -> (DirectorRepo, TempDir, Home, Player<NullSessions>) {
    let repo = director_repo();
    write_file(repo.path(), "roadmap.toml", ROADMAP);
    commit(repo.path(), "turn task agents on");
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
    .expect("player open");
    (repo, home_dir, home, player)
}

fn status(hub: &Git, slug: &str) -> Status {
    state::derive(hub, &|_| false)
        .expect("derive")
        .tasks
        .into_iter()
        .find(|t| t.slug == slug)
        .unwrap_or_else(|| panic!("no task {slug}"))
        .status
}

fn tick(player: &mut Player<NullSessions>) -> TickReport {
    player.tick().expect("tick")
}

/// Plans `s` (no stage): ready, nothing launched.
fn plan_only(repo: &DirectorRepo, player: &mut Player<NullSessions>) {
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    let report = tick(player);
    assert!(report.rejected.is_empty(), "{report}");
}

/// Stages `s` as the director and ticks.
fn stage(repo: &DirectorRepo, player: &mut Player<NullSessions>) -> TickReport {
    commit_empty(repo.path(), "stage: go");
    let report = tick(player);
    assert!(report.rejected.is_empty(), "{report}");
    report
}

/// Moves `main` with a roadmap change, ticks (the player merges it into the
/// pending branches) and brings the director's branch up to the hub's.
fn move_main(repo: &DirectorRepo, player: &mut Player<NullSessions>, title: &str) {
    let roadmap = ROADMAP.replace("Test Project", title);
    director_commit_on_branch(repo.path(), "main", "roadmap.toml", &roadmap, "retitle");
    push_main_to_hub(repo.path());
    let report = tick(player);
    assert!(report.rejected.is_empty(), "{report}");
    director_pull_task_branch(repo.path(), "s");
}

#[test]
fn a_ready_task_never_starts_until_staged() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    tick(&mut player); // the first tick is a baseline: no events
    plan_only(&repo, &mut player);
    assert_eq!(status(&hub, "s"), Status::Ready);
    for _ in 0..3 {
        let report = tick(&mut player);
        assert!(report.agents_launched.is_empty(), "{report}");
    }
    assert!(player.sessions().launched.is_empty());
    let kinds: Vec<_> = dashboard::events::read_all(&home).iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&dashboard::events::EventKind::Ready), "{kinds:?}");

    let report = stage(&repo, &mut player);
    assert_eq!(status(&hub, "s"), Status::Staged);
    assert_eq!(report.agents_launched, vec!["s".to_string()], "{report}");
}

#[test]
fn staged_survives_main_moving() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    plan_only(&repo, &mut player);
    stage(&repo, &mut player);
    director_pull_task_branch(repo.path(), "s");

    move_main(&repo, &mut player, "Retitled once");
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();
    assert_eq!(
        records::import_note(&hub, &tip).map(|n| n.transition),
        Some(Transition::MergeUnblock),
        "the staged branch still receives main"
    );
    assert_eq!(status(&hub, "s"), Status::Staged);
    move_main(&repo, &mut player, "Retitled twice");
    assert_eq!(status(&hub, "s"), Status::Staged);
}

#[test]
fn stage_and_unstage_are_refused_outside_their_status() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());

    // `unstage:` on a ready task.
    plan_only(&repo, &mut player);
    commit_empty(repo.path(), "unstage: nothing to put back");
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(status(&hub, "s"), Status::Ready);

}

#[test]
fn staging_twice_or_unstaging_after_the_agent_launched_is_refused() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    plan_only(&repo, &mut player);
    stage(&repo, &mut player);
    commit_empty(repo.path(), "stage: again");
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(status(&hub, "s"), Status::Staged);

    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    plan_only(&repo, &mut player);
    stage(&repo, &mut player);
    commit_empty(repo.path(), "unstage: too late");
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert!(report.rejected[0].message.contains("already running"), "{report}");
    assert_eq!(status(&hub, "s"), Status::Staged);
}

#[test]
fn unstage_puts_a_task_back_while_no_agent_exists() {
    let repo = director_repo(); // task agents off: staging launches nothing
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home")))
        .expect("init");
    let home = Home { root: report.home };
    let hub = Git::new(home.hub());
    let mut player = Player::open(Home { root: home.root.clone() }, NullSessions::default()).unwrap();
    plan_only(&repo, &mut player);
    stage(&repo, &mut player);
    assert_eq!(status(&hub, "s"), Status::Staged);
    commit_empty(repo.path(), "unstage: not yet");
    let report = tick(&mut player);
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status(&hub, "s"), Status::Ready);
}

#[test]
fn staging_a_pending_task_and_a_spike_is_refused() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());

    plan_only(&repo, &mut player);
    director_commit_on_branch(
        repo.path(),
        "task/p",
        "tasks/p/task.toml",
        "title = \"p\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"s\"]\n",
        "plan: p",
    );
    commit_empty(repo.path(), "stage: too early");
    let report = tick(&mut player);
    assert_eq!(status(&hub, "p"), Status::Pending);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);

    director_commit_on_branch(
        repo.path(),
        "task/sp",
        "tasks/sp/task.toml",
        "title = \"sp\"\nphase = \"p1\"\nmodules = [\"app\"]\nkind = \"spike\"\n",
        "plan: sp",
    );
    tick(&mut player);
    assert_eq!(status(&hub, "sp"), Status::Ready);
    commit_empty(repo.path(), "stage: start the spike");
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(status(&hub, "sp"), Status::Ready);
}

#[test]
fn a_task_agents_stage_is_ownership() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    plan_only(&repo, &mut player);
    stage(&repo, &mut player);
    let dir = dashboard::paths::agent_clone(&repo.director_path(), "s");
    let git = Git::new(&dir);
    git.run(&["config", "user.name", "Test Agent"]).unwrap();
    git.run(&["config", "user.email", "agent@example.com"]).unwrap();
    git.run(&["config", "commit.gpgsign", "false"]).unwrap();
    commit_empty(&dir, "stage: sneaky");
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert_eq!(report.rejected[0].actor, Actor::TaskAgent);
    assert_eq!(status(&hub, "s"), Status::Staged);
}

// The old race: a dependent task used to get its agent in the accept's own
// tick. Now it becomes `ready` and waits for the director's `stage:`.
#[test]
fn a_dependent_task_waits_ready_after_its_dependency_is_accepted() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    director_commit_on_branch(repo.path(), "task/t", "tasks/t/task.toml",
        "title = \"t\"\nphase = \"p1\"\nmodules = [\"app\"]\ndepends_on = [\"s\"]\n", "plan: t");
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    tick(&mut player);
    stage(&repo, &mut player);

    let adir = dashboard::paths::agent_clone(&repo.director_path(), "s");
    let g = Git::new(&adir);
    g.run(&["config", "user.name", "a"]).unwrap();
    g.run(&["config", "user.email", "a@x"]).unwrap();
    g.run(&["config", "commit.gpgsign", "false"]).unwrap();
    write_file(&adir, "tasks/s/prompt.md", "p\n");
    write_file(&adir, &*tests_file("s"), &*task_suite_lean("[[check]]\nname = \"sc\"\ncmd = \"test -f src/f.txt\"\n"));
    commit(&adir, "post: s");
    let r = tick(&mut player);
    assert!(r.rejected.is_empty(), "{r}");
    let wdir = dashboard::paths::worker_clone(&repo.director_path(), "s");
    let g = Git::new(&wdir);
    g.run(&["config", "user.name", "w"]).unwrap();
    g.run(&["config", "user.email", "w@x"]).unwrap();
    g.run(&["config", "commit.gpgsign", "false"]).unwrap();
    write_file(&wdir, "src/f.txt", "x\n");
    commit(&wdir, "finish");
    let r = tick(&mut player);
    assert!(r.rejected.is_empty(), "{r}");
    assert_eq!(status(&hub, "s"), Status::Tested);

    director_pull_task_branch(repo.path(), "s");
    commit_empty(repo.path(), "accept: s");
    let r = tick(&mut player);
    assert_eq!(status(&hub, "s"), Status::Accepted, "{r}");
    assert_eq!(status(&hub, "t"), Status::Ready, "{r}");
    assert!(!r.agents_launched.contains(&"t".to_string()), "{r}");
    tick(&mut player);
    assert_eq!(status(&hub, "t"), Status::Ready);

    director_pull_task_branch(repo.path(), "t");
    commit_empty(repo.path(), "stage: go");
    let r = tick(&mut player);
    assert!(r.rejected.is_empty(), "{r}");
    assert_eq!(status(&hub, "t"), Status::Staged);
    assert!(r.agents_launched.contains(&"t".to_string()), "{r}");
}

// Once the task agent exists its prompt is written; a replan could only
// desync it (and used to send the task back to `pending` under it).
#[test]
fn a_replan_is_refused_once_the_task_agent_exists() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    director_commit_on_branch(repo.path(), "task/b", "tasks/b/task.toml", PLAN, "plan: b");
    plan_only(&repo, &mut player);
    stage(&repo, &mut player);
    director_pull_task_branch(repo.path(), "s");
    assert!(records::agent_launched(&hub, "s"));

    director_commit_on_branch(
        repo.path(),
        "task/s",
        "tasks/s/task.toml",
        &format!("{PLAN}depends_on = [\"b\"]\n"),
        "replan: s",
    );
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(status(&hub, "s"), Status::Staged);
}
