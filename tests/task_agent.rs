// Integration tests for task agents (spec.md §17): the player launches one
// per agent-managed task once it's ready, imports its post and send-backs
// from its own clone (`paths::agent_clone`, branch `agent/<slug>`) under the
// rules a task agent works by, asks it to review every tested tip, and turns
// the report it leaves into a note and an `agent_report` event. Every test
// drives `Player::tick` with `NullSessions` and plays the task agent and the
// worker by committing in the clones the player itself created.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::events::{self, EventKind};
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{NullSessions, Player, Sessions, TickReport};
use dashboard::records::{self, Actor, Transition};
use dashboard::state::{self, Status};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const PLAN: &str = "title = \"s\"\nphase = \"p1\"\nbrief = \"mind the widget cache\"\n";
const CHECK: &str = "\n[check]\nname = \"s-check\"\ncmd = \"test -f feature.txt\"\n";

// --------------------------------------------------------------------------
// Fixture helpers
// --------------------------------------------------------------------------

/// A director repo whose roadmap turns task agents on (`task_agent_cmd`),
/// with the worker's `agent_cmd` left at its default.
fn repo_with_task_agents() -> DirectorRepo {
    let repo = director_repo();
    write_file(
        repo.path(),
        "roadmap.toml",
        "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\ngoal = \"ship the widget\"\n\n[player]\ntask_agent_cmd = \"fake-task-agent\"\n",
    );
    commit(repo.path(), "turn task agents on");
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
    git.run(&["config", "user.name", "Test Agent"])
        .expect("set user.name");
    git.run(&["config", "user.email", "agent@example.com"])
        .expect("set user.email");
    git.run(&["config", "commit.gpgsign", "false"])
        .expect("disable gpgsign");
}

fn agent_dir(repo: &DirectorRepo) -> PathBuf {
    paths::agent_clone(&repo.director_path(), "s")
}

fn agent_sent(player: &Player<NullSessions>) -> Vec<String> {
    player
        .sessions()
        .sent
        .iter()
        .filter(|(k, _)| k == "s_agent")
        .map(|(_, m)| m.clone())
        .collect()
}

/// Plans `s` as the director and ticks: the task is ready at once (no
/// dependencies), so the same tick launches its task agent.
fn plan(repo: &DirectorRepo, player: &mut Player<NullSessions>) -> TickReport {
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    let report = player.tick().expect("plan tick should succeed");
    assert!(report.rejected.is_empty(), "{report}");
    report
}

/// Commits `files` in the task agent's clone, on `agent/s`.
fn agent_commit(repo: &DirectorRepo, files: &[(&str, &str)], msg: &str) -> String {
    let dir = agent_dir(repo);
    ensure_identity(&dir);
    for (rel, content) in files {
        write_file(&dir, rel, content);
    }
    commit(&dir, msg)
}

/// The task agent's post: the prompt, the `[check]` in task.toml and the
/// same check in its suite fragment.
fn agent_post(repo: &DirectorRepo) -> String {
    agent_commit(
        repo,
        &[
            (
                "tasks/s/prompt.md",
                "make feature.txt; tests you add must be pure\n",
            ),
            ("tasks/s/task.toml", &format!("{PLAN}{CHECK}")),
            (
                "tests.d/s.toml",
                "[[check]]\nname = \"s-check\"\ncmd = \"test -f feature.txt\"\n",
            ),
        ],
        "post: s",
    )
}

/// plan -> the task agent's post -> the worker's passing finish: `s` is
/// tested and its task agent has been asked to review it.
fn plan_post_finish(repo: &DirectorRepo, player: &mut Player<NullSessions>) {
    plan(repo, player);
    agent_post(repo);
    let report = player.tick().expect("post tick should succeed");
    assert_eq!(report.launched, vec!["s".to_string()], "{report}");
    let dir = paths::worker_clone(&repo.director_path(), "s");
    ensure_identity(&dir);
    write_file(&dir, "feature.txt", "done\n");
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

/// The report file the player reads from the task agent's git directory.
fn report_file(repo: &DirectorRepo) -> PathBuf {
    let out = Git::new(agent_dir(repo))
        .run(&["rev-parse", "--absolute-git-dir"])
        .expect("git dir");
    PathBuf::from(out).join("dashboard-report.md")
}

// --------------------------------------------------------------------------
// Launch
// --------------------------------------------------------------------------

#[test]
fn a_ready_task_launches_its_task_agent_with_the_post_step() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);

    let report = plan(&repo, &mut player);
    assert_eq!(report.agents_launched, vec!["s".to_string()], "{report}");
    let call = player
        .sessions()
        .launched
        .iter()
        .find(|c| c.slug == "s_agent")
        .expect("a launch for the task agent")
        .clone();
    assert_eq!(call.agent_cmd, "fake-task-agent");
    assert_eq!(call.clone_dir, agent_dir(&repo));
    assert_eq!(
        Git::new(&call.clone_dir)
            .run(&["branch", "--show-current"])
            .unwrap(),
        "agent/s"
    );

    let prompt = std::fs::read_to_string(&call.prompt_file).expect("prompt file");
    for needle in [
        "task agent for task `s`",
        "Never write a throwaway implementation",
        "must be pure",
        "mind the widget cache",
        "ship the widget",
        "Step: post",
        "verdict:",
    ] {
        assert!(
            prompt.contains(needle),
            "prompt lacks {needle:?}:\n{prompt}"
        );
    }

    // A second tick launches nothing more.
    let report = player.tick().expect("tick");
    assert!(report.agents_launched.is_empty(), "{report}");

    let kinds: Vec<EventKind> = events::read_all(&home).iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EventKind::AgentLaunched), "{kinds:?}");
    assert!(
        !kinds.contains(&EventKind::Ready),
        "a task agent's ready task needs nothing from the director: {kinds:?}"
    );
}

#[test]
fn spikes_and_disabled_task_agents_launch_none() {
    let repo = director_repo(); // task_agent_cmd = ""
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    let report = plan(&repo, &mut player);
    assert!(report.agents_launched.is_empty(), "{report}");

    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    director_commit_on_branch(
        repo.path(),
        "task/sp",
        "tasks/sp/task.toml",
        "title = \"sp\"\nkind = \"spike\"\nphase = \"p1\"\n",
        "plan: sp",
    );
    let report = player.tick().expect("tick");
    assert!(
        report.agents_launched.is_empty(),
        "a spike's question is the director's: {report}"
    );
}

// --------------------------------------------------------------------------
// The task agent's post
// --------------------------------------------------------------------------

#[test]
fn the_task_agents_post_is_imported_and_launches_the_worker() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(&repo, &mut player);

    let sha = agent_post(&repo);
    let report = player.tick().expect("post tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::Post);
    let note = records::import_note(&hub, &sha).expect("import note");
    assert_eq!(note.actor, Actor::TaskAgent);
    assert_eq!(report.launched, vec!["s".to_string()], "{report}");
    assert!(
        agent_sent(&player)
            .iter()
            .any(|m| m.contains("imported your post")),
        "{:?}",
        agent_sent(&player)
    );
}

#[test]
fn a_director_post_is_ownership_when_task_agents_are_on() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);

    checkout_branch(repo.path(), "task/s");
    write_file(repo.path(), "tasks/s/prompt.md", "do it\n");
    write_file(repo.path(), "tasks/s/task.toml", &format!("{PLAN}{CHECK}"));
    write_file(
        repo.path(),
        "tests.d/s.toml",
        "[[check]]\nname = \"s-check\"\ncmd = \"test -f feature.txt\"\n",
    );
    commit(repo.path(), "post: s");
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert_eq!(report.rejected[0].actor, Actor::Director);
    assert!(
        report.rejected[0].message.contains("task agent"),
        "{}",
        report.rejected[0].message
    );
}

#[test]
fn a_task_agent_never_writes_product_code_or_the_plan_or_a_marker() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(&repo, &mut player);
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();

    // Product code next to the post.
    agent_post(&repo);
    agent_commit(
        &repo,
        &[("src/feature.rs", "fn main() {}\n")],
        "a throwaway implementation",
    );
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Ownership);
    assert_eq!(report.rejected[0].actor, Actor::TaskAgent);
    assert!(
        report.rejected[0].message.contains("src/feature.rs"),
        "{}",
        report.rejected[0].message
    );
    assert!(
        agent_sent(&player)
            .iter()
            .any(|m| m.contains("rejected your commit")),
        "rejections are relayed to the task agent"
    );

    // The plan: depends_on, phase and the rest are the director's.
    let dir = agent_dir(&repo);
    Git::new(&dir)
        .run(&["reset", "-q", "--hard", &tip])
        .unwrap();
    agent_commit(
        &repo,
        &[
            ("tasks/s/prompt.md", "p\n"),
            (
                "tasks/s/task.toml",
                &format!(
                    "title = \"s, renamed\"\nphase = \"p1\"\nbrief = \"mind the widget cache\"\n{CHECK}"
                ),
            ),
            (
                "tests.d/s.toml",
                "[[check]]\nname = \"s-check\"\ncmd = \"test -f feature.txt\"\n",
            ),
        ],
        "post with a new title",
    );
    let report = player.tick().expect("tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert!(
        report.rejected[0].message.contains("only set [check]"),
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

#[test]
fn a_hub_move_under_the_agents_commit_asks_for_a_rebase_without_a_strike() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan(&repo, &mut player);

    // The task agent posts on the planned tip while the director replans.
    agent_post(&repo);
    director_commit_on_branch(
        repo.path(),
        "task/s",
        "tasks/s/task.toml",
        &format!("{PLAN}milestone = true\n"),
        "replan: s",
    );
    let report = player.tick().expect("tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(report.imported.len(), 1, "only the replan: {report}");
    assert_eq!(report.imported[0].2, Transition::Replan);
    let asked: Vec<String> = agent_sent(&player)
        .into_iter()
        .filter(|m| m.contains("git rebase origin/task/s"))
        .collect();
    assert_eq!(asked.len(), 1, "{asked:?}");

    // Said once, not every tick.
    player.tick().expect("tick");
    assert_eq!(
        agent_sent(&player)
            .iter()
            .filter(|m| m.contains("git rebase origin/task/s"))
            .count(),
        1
    );

    // The agent rebases its post onto the replan; it still keeps the plan.
    let dir = agent_dir(&repo);
    Git::new(&dir).run(&["fetch", "-q", "origin"]).unwrap();
    Git::new(&dir)
        .run(&["reset", "-q", "--hard", "origin/task/s"])
        .unwrap();
    agent_commit(
        &repo,
        &[
            ("tasks/s/prompt.md", "make feature.txt\n"),
            (
                "tasks/s/task.toml",
                &format!("{PLAN}milestone = true\n{CHECK}"),
            ),
            (
                "tests.d/s.toml",
                "[[check]]\nname = \"s-check\"\ncmd = \"test -f feature.txt\"\n",
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
fn a_tested_task_is_handed_to_its_task_agent_for_review_once() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    let reviews: Vec<String> = agent_sent(&player)
        .into_iter()
        .filter(|m| m.starts_with("Step: review round 1"))
        .collect();
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    assert!(reviews[0].contains("reviews/r1.md"), "{}", reviews[0]);
    player.tick().expect("tick");
    assert_eq!(
        agent_sent(&player)
            .iter()
            .filter(|m| m.starts_with("Step: review round 1"))
            .count(),
        1,
        "asked once per tested tip"
    );

    let kinds: Vec<EventKind> = events::read_all(&home).iter().map(|e| e.kind).collect();
    assert!(
        !kinds.contains(&EventKind::Tested),
        "the review is the task agent's: {kinds:?}"
    );
}

#[test]
fn the_task_agents_send_back_is_imported_and_relayed_to_the_worker() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    let dir = agent_dir(&repo);
    Git::new(&dir).run(&["fetch", "-q", "origin"]).unwrap();
    Git::new(&dir)
        .run(&["merge", "-q", "--ff-only", "origin/task/s"])
        .unwrap();
    let sha = agent_commit(
        &repo,
        &[
            ("tasks/s/reviews/r1.md", "also feature2.txt\n"),
            (
                "tests.d/s.toml",
                "[[check]]\nname = \"s-check\"\ncmd = \"test -f feature.txt && test -f feature2.txt\"\n",
            ),
        ],
        "send back: s r1",
    );
    let report = player.tick().expect("tick");
    assert_eq!(report.imported.len(), 1, "{report}");
    assert_eq!(report.imported[0].2, Transition::SendBack);
    assert_eq!(
        records::import_note(&hub, &sha).unwrap().actor,
        Actor::TaskAgent
    );
    assert_eq!(task(&hub, "s").status, Status::InRevision);
    assert!(
        player
            .sessions()
            .sent
            .iter()
            .any(|(k, m)| k == "s" && m.contains("also feature2.txt")),
        "the worker gets the review"
    );
}

#[test]
fn a_report_becomes_a_note_and_an_agent_report_event() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_post_finish(&repo, &mut player);

    let file = report_file(&repo);
    std::fs::write(&file, "slug: s\nstep: review r1\ncommit: none\ncheck: feature.txt exists\nverdict: accept\nwhy: it does it.\nconcerns: none\n").unwrap();
    let report = player.tick().expect("tick");
    assert_eq!(report.agent_reports.len(), 1, "{report}");
    assert_eq!(report.agent_reports[0].1.verdict, "accept");
    assert!(!file.exists(), "the player consumes the report");

    let t = task(&hub, "s");
    assert_eq!(
        t.agent.and_then(|a| a.report).map(|r| r.verdict).as_deref(),
        Some("accept")
    );
    let ev = events::read_all(&home)
        .into_iter()
        .find(|e| e.kind == EventKind::AgentReport)
        .expect("an agent_report event");
    assert!(ev.message.contains("verdict: accept"), "{}", ev.message);

    // The director accepts on it; the task agent's session ends with the task.
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
    assert!(player.sessions().killed.contains(&"s_agent".to_string()));
    assert!(
        !agent_dir(&repo).exists(),
        "the task agent's worktree is removed"
    );
}

// --------------------------------------------------------------------------
// ask / relaunch / death
// --------------------------------------------------------------------------

#[test]
fn ask_relaunch_and_death_of_a_task_agent() {
    let repo = repo_with_task_agents();
    let (_home_dir, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan(&repo, &mut player);

    player.ask("s", "why that check?").expect("ask");
    assert!(
        agent_sent(&player)
            .iter()
            .any(|m| m.contains("Question from the director: why that check?"))
    );
    assert!(
        player.relaunch_agent("s").is_err(),
        "alive: nothing to relaunch"
    );

    player.sessions_mut().alive.remove("s_agent");
    player.tick().expect("tick");
    let died = events::read_all(&home)
        .into_iter()
        .filter(|e| e.kind == EventKind::AgentDied)
        .count();
    assert_eq!(died, 1);
    assert!(
        player.ask("s", "still there?").is_err(),
        "dead: ask must say so"
    );

    player.relaunch_agent("s").expect("relaunch");
    let call = player
        .sessions()
        .launched
        .iter()
        .rfind(|c| c.slug == "s_agent")
        .unwrap()
        .clone();
    let prompt = std::fs::read_to_string(&call.prompt_file).unwrap();
    assert!(
        prompt.contains("You were relaunched") && prompt.contains("Step: post"),
        "{prompt}"
    );
    assert!(player.sessions().alive("s_agent"));
}

// --------------------------------------------------------------------------
// Contests (spec §18)
// --------------------------------------------------------------------------

/// plan -> the task agent's post -> the worker contests the task check.
fn plan_post_contest(repo: &DirectorRepo, player: &mut Player<NullSessions>) -> TickReport {
    plan(repo, player);
    agent_post(repo);
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
            "feature.txt can't exist in a pure test",
        ])
        .unwrap();
    player.tick().expect("contest tick")
}

#[test]
fn a_contest_goes_to_the_task_agent_and_its_reply_back_to_the_worker() {
    let repo = repo_with_task_agents();
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
    let asked: Vec<String> = agent_sent(&player)
        .into_iter()
        .filter(|m| m.starts_with("Step: contest c1"))
        .collect();
    assert_eq!(asked.len(), 1, "{:?}", agent_sent(&player));
    assert!(asked[0].contains("pure test"), "{}", asked[0]);
    assert!(
        !events::read_all(&home)
            .iter()
            .any(|e| e.kind == EventKind::Contested),
        "the task agent's to answer"
    );

    let dir = agent_dir(&repo);
    Git::new(&dir).run(&["fetch", "-q", "origin"]).unwrap();
    Git::new(&dir)
        .run(&["merge", "-q", "--ff-only", "origin/task/s"])
        .unwrap();
    let sha = agent_commit(
        &repo,
        &[(
            "tasks/s/contests/c1.md",
            "fair: check feature.txt is created by the build\n",
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
        Actor::TaskAgent
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
}

#[test]
fn without_task_agents_the_director_replies_and_a_contest_past_the_limit_escalates() {
    let repo = director_repo(); // task agents off
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!(
            "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\n\n[limits]\nmax_contest_rounds = 1\n{NO_TASK_AGENTS}"
        ),
    );
    commit(repo.path(), "one contest only");
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", PLAN, "plan: s");
    player.tick().expect("plan tick");
    checkout_branch(repo.path(), "task/s");
    write_file(repo.path(), "tasks/s/prompt.md", "make feature.txt\n");
    write_file(repo.path(), "tasks/s/task.toml", &format!("{PLAN}{CHECK}"));
    write_file(
        repo.path(),
        "tests.d/s.toml",
        "[[check]]\nname = \"s-check\"\ncmd = \"test -f feature.txt\"\n",
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
        "tests.d/s.toml",
        "[[check]]\nname = \"s-check\"\ncmd = \"test -f feature.txt\"\nruns = 1\n",
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
        "the test stands: create feature.txt\n",
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
