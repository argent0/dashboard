// Integration tests for queued runs (`[player] runs = "queued"`): an agent's
// `dashboard test` snapshots its worktree, the player runs the suite on it
// one request per tick, and the agent reads the output. No strikes.

mod common;

use common::*;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::player::{NullSessions, Player};
use dashboard::queue;
use std::time::Duration;

fn queued_repo() -> DirectorRepo {
    let repo = director_repo();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(
        repo.path(),
        "roadmap.toml",
        &roadmap.replace("runs = \"parallel\"", "runs = \"queued\""),
    );
    write_file(
        repo.path(),
        TESTS_LEAN,
        &*suite_lean("[[check]]\nname = \"flag\"\ncmd = \"echo checking-the-flag; test -f ok.flag\"\n"),
    );
    commit(repo.path(), "queued runs");
    repo
}

#[test]
fn queued_run_reports_output_and_never_strikes() {
    let repo = queued_repo();
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home")))
        .expect("init");
    let home = Home { root: report.home };
    let mut player = Player::open(
        Home {
            root: home.root.clone(),
        },
        NullSessions::default(),
    )
    .expect("player open");
    player.tick().expect("first tick");

    // A worker-style worktree on task/x with one untracked file.
    let work = tempfile::TempDir::new().unwrap();
    let hub_path = home.hub();
    let dir = work.path().join("wt");
    Git::new(work.path())
        .run(&["clone", "-q", hub_path.to_str().unwrap(), dir.to_str().unwrap()])
        .unwrap();
    let wt = Git::new(&dir);
    wt.run(&["checkout", "-q", "-b", "task/x", "origin/main"]).unwrap();

    // Fails: the flag is missing.
    let snap = queue::snapshot(&wt).unwrap();
    let req = queue::enqueue(&home, &wt, "x", &snap, &[], 1).unwrap();
    assert!(queue::next(&home).is_some());
    player.tick().expect("serve tick");
    let (pass, text) = queue::wait(&home, &req.id, Some(Duration::from_secs(1))).expect("result");
    assert!(!pass, "{text}");
    assert!(text.contains("checking-the-flag"), "{text}");
    assert!(queue::next(&home).is_none());

    // Passes once an untracked file is there; the branch itself is untouched.
    let head = wt.rev_parse("HEAD").unwrap();
    write_file(&dir, "ok.flag", "1\n");
    let snap = queue::snapshot(&wt).unwrap();
    let req = queue::enqueue(&home, &wt, "x", &snap, &[], 2).unwrap();
    player.tick().expect("serve tick");
    let (pass, text) = queue::wait(&home, &req.id, Some(Duration::from_secs(1))).expect("result");
    assert!(pass, "{text}");
    assert_eq!(wt.rev_parse("HEAD").unwrap(), head);

    let hub = Git::new(&hub_path);
    assert!(hub.for_each_ref("refs/rejected/*").is_empty());
    assert!(hub.for_each_ref("refs/dashboard/queue/*").is_empty());
}
