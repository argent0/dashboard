// Integration tests for `staged`: a director's `stage:` marker holds a
// `ready` task (no task agent is launched), `unstage:` releases it. The
// status survives `main` moving under the branch (the player's
// `merge_unblock` merge becomes the tip) and a replan.

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

const PLAN: &str = "title = \"s\"\nphase = \"p1\"\n";
const ROADMAP: &str = "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\ngoal = \"ship the widget\"\n\n[player]\ntask_agent_cmd = \"fake-task-agent\"\n";

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

/// Plans `s` and stages it before the player ever sees it `ready`, so no
/// task agent is launched.
fn plan_and_stage(repo: &DirectorRepo, player: &mut Player<NullSessions>) {
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    commit_empty(repo.path(), "stage: not yet");
    let report = tick(player);
    assert!(report.rejected.is_empty(), "{report}");
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
fn a_staged_task_launches_no_task_agent_until_unstaged() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    plan_and_stage(&repo, &mut player);
    assert_eq!(status(&hub, "s"), Status::Staged);
    for _ in 0..3 {
        let report = tick(&mut player);
        assert!(report.agents_launched.is_empty(), "{report}");
    }
    assert!(player.sessions().launched.is_empty());

    commit_empty(repo.path(), "unstage: go");
    let report = tick(&mut player);
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status(&hub, "s"), Status::Ready);
    assert_eq!(report.agents_launched, vec!["s".to_string()], "{report}");
}

#[test]
fn staged_survives_main_moving_and_a_replan() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    plan_and_stage(&repo, &mut player);
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

    director_commit_on_branch(
        repo.path(),
        "task/s",
        "tasks/s/task.toml",
        &format!("{PLAN}brief = \"new\"\n"),
        "replan: s",
    );
    let report = tick(&mut player);
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status(&hub, "s"), Status::Staged);
    assert!(player.sessions().launched.is_empty());

    commit_empty(repo.path(), "unstage: go");
    let report = tick(&mut player);
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status(&hub, "s"), Status::Ready);
    move_main(&repo, &mut player, "Retitled thrice");
    assert_eq!(status(&hub, "s"), Status::Ready);
}

#[test]
fn stage_and_unstage_are_refused_outside_their_status() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());

    // `unstage:` on a ready task.
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    tick(&mut player);
    assert_eq!(status(&hub, "s"), Status::Ready);
    commit_empty(repo.path(), "unstage: nothing to release");
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(status(&hub, "s"), Status::Ready);
}

#[test]
fn staging_twice_a_pending_task_and_a_spike_are_refused() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());

    plan_and_stage(&repo, &mut player);
    commit_empty(repo.path(), "stage: again");
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(status(&hub, "s"), Status::Staged);

    // A pending task (an unaccepted dependency).
    director_commit_on_branch(
        repo.path(),
        "task/p",
        "tasks/p/task.toml",
        "title = \"p\"\nphase = \"p1\"\ndepends_on = [\"s\"]\n",
        "plan: p",
    );
    commit_empty(repo.path(), "stage: too early");
    let report = tick(&mut player);
    assert_eq!(status(&hub, "p"), Status::Pending);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);

    // A spike.
    director_commit_on_branch(
        repo.path(),
        "task/sp",
        "tasks/sp/task.toml",
        "title = \"sp\"\nphase = \"p1\"\nkind = \"spike\"\n",
        "plan: sp",
    );
    tick(&mut player);
    assert_eq!(status(&hub, "sp"), Status::Ready);
    commit_empty(repo.path(), "stage: hold the spike");
    let report = tick(&mut player);
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Plan);
    assert_eq!(status(&hub, "sp"), Status::Ready);
}

#[test]
fn a_task_agents_stage_is_ownership() {
    let (repo, _h, home, mut player) = setup();
    let hub = Git::new(home.hub());
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    tick(&mut player);
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
    assert_eq!(status(&hub, "s"), Status::Ready);
}
