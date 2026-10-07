// Integration tests for the check jail (src/sandbox.rs, roadmap.toml's
// `[sandbox]`) and the worktree-leak check every check run gets
// (src/checks.rs): what a jailed check can and can't touch, that a dirty
// worktree fails a check and can't stand in for a failing one, and the
// preflight `dashboard init` and the player run before trusting the jail.
// They need a working `bwrap`, like the rest of the suite (the default
// fixture roadmap has the jail on).

mod common;

use common::*;
use dashboard::checks::Runner;
use dashboard::config::{Check, Reason, SandboxConfig};
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::player::{NullSessions, Player};
use dashboard::sandbox::Sandbox;
use std::path::Path;
use tempfile::TempDir;

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

fn check(name: &str, cmd: &str) -> Check {
    Check {
        name: name.to_string(),
        cmd: cmd.to_string(),
        runs: 1,
        min_pass: 1,
        retry_failed: 0,
        retry_on: vec![],
    }
}

fn jail(writable: &[&Path]) -> Sandbox {
    Sandbox::from_config(&SandboxConfig {
        writable: writable.iter().map(|p| p.display().to_string()).collect(),
        ..Default::default()
    })
    .unwrap()
}

fn jailed_runner(home: &Home, writable: &[&Path]) -> Runner {
    let mut runner = Runner::new(home);
    runner.set_sandbox(jail(writable));
    runner
}

#[test]
fn a_jailed_check_cannot_write_outside_its_worktree() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = jailed_runner(&home, &[]);
    let commit = repo.git().rev_parse("HEAD").unwrap();
    let outside = TempDir::new().unwrap();
    let target = outside.path().join("escaped");

    let r = runner.run_check(
        &commit,
        &check("escape", &format!("echo x > {}", target.display())),
    );
    assert!(!r.pass, "{r:?}");
    assert!(!target.exists(), "the write must not reach the host");

    // Unjailed, the same command gets out: the jail is what stopped it.
    let r = Runner::new(&home).run_check(
        &commit,
        &check("escape", &format!("echo x > {}", target.display())),
    );
    assert!(r.pass, "{r:?}");
    assert!(target.exists());
}

#[test]
fn a_jailed_check_gets_a_private_tmp_and_no_network() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = jailed_runner(&home, &[]);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let name = format!("dashboard-sandbox-test-{}", std::process::id());
    let r = runner.run_check(
        &commit,
        &check(
            "tmp",
            &format!("echo x > /tmp/{name} && test \"$TMPDIR\" = /tmp"),
        ),
    );
    assert!(r.pass, "{r:?}");
    assert!(!Path::new("/tmp").join(&name).exists());

    let r = runner.run_check(
        &commit,
        &check(
            "net",
            "! tail -n +3 /proc/net/dev | grep -v '^ *lo:' | grep -q .",
        ),
    );
    assert!(r.pass, "only loopback inside the jail: {r:?}");
}

#[test]
fn writable_paths_are_writable_from_the_jail() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let cache = TempDir::new().unwrap();
    let runner = jailed_runner(&home, &[cache.path()]);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let r = runner.run_check(
        &commit,
        &check("cache", &format!("echo x > {}/hit", cache.path().display())),
    );
    assert!(r.pass, "{r:?}");
    assert!(cache.path().join("hit").exists());
}

#[test]
fn a_result_cached_under_one_jail_is_not_reused_under_another() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let commit = repo.git().rev_parse("HEAD").unwrap();
    let log = TempDir::new().unwrap();
    let c = check(
        "logged",
        &format!("echo run >> {}/log", log.path().display()),
    );

    Runner::new(&home).run_check(&commit, &c);
    let jailed = jailed_runner(&home, &[log.path()]);
    let r = jailed.run_check(&commit, &c);
    assert_eq!(r.isolation, jailed.sandbox().fingerprint());
    jailed.run_check(&commit, &c);

    let runs = std::fs::read_to_string(log.path().join("log")).unwrap();
    assert_eq!(runs.lines().count(), 2, "one run per policy, then cached");
}

#[test]
fn changing_a_tracked_file_is_a_leak_but_ignored_build_output_is_not() {
    let repo = director_repo();
    write_file(repo.path(), ".gitignore", "target/\n");
    commit(repo.path(), "ignore build output");
    let (_home_dir, home) = init_hub(&repo);
    let runner = jailed_runner(&home, &[]);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let r = runner.run_check(
        &commit,
        &check("build", "mkdir -p target && touch target/x"),
    );
    assert!(r.pass, "{r:?}");
    assert!(r.leaked.is_empty());

    let r = runner.run_check(&commit, &check("edits", "echo >> tests.toml"));
    assert!(!r.pass);
    assert_eq!(r.leaked, vec!["M tests.toml".to_string()]);
}

#[test]
fn a_leak_stops_the_runs_and_is_never_retried() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = jailed_runner(&home, &[]);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let c = Check {
        runs: 3,
        retry_failed: 2,
        ..check("leaky", "touch out.txt")
    };
    let r = runner.run_check(&commit, &c);
    assert!(!r.pass);
    assert_eq!(r.exits, vec![0], "stops at the first leaking run");
    assert_eq!(r.retries, 0);
}

#[test]
fn an_impure_task_check_cannot_stand_in_for_a_failing_one_at_post() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = Player::open(
        Home {
            root: home.root.clone(),
        },
        NullSessions::default(),
    )
    .unwrap();
    director_commit_on_branch(
        repo.path(),
        "task/a",
        "tasks/a/task.toml",
        "title = \"a\"\nphase = \"p1\"\n",
        "plan: a",
    );
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");

    // Fails (exit 1), but only after dirtying its worktree.
    let cmd = "touch scratch.txt; test -f feature.txt";
    write_file(repo.path(), "tasks/a/prompt.md", "do it\n");
    write_file(
        repo.path(),
        "tasks/a/task.toml",
        &format!("title = \"a\"\nphase = \"p1\"\n\n[check]\nname = \"a-check\"\ncmd = {cmd:?}\n"),
    );
    write_file(
        repo.path(),
        "tests.d/a.toml",
        &format!("[[check]]\nname = \"a-check\"\ncmd = {cmd:?}\n"),
    );
    commit(repo.path(), "post: a");
    let report = player.tick().unwrap();
    assert!(report.imported.is_empty(), "{report}");
    assert_eq!(report.rejected.len(), 1, "{report}");
    let rej = &report.rejected[0];
    assert_eq!(rej.reason, Reason::Post);
    assert!(rej.message.contains("impure"), "{}", rej.message);
    assert!(rej.message.contains("scratch.txt"), "{}", rej.message);
    assert!(hub.rev_parse("refs/heads/task/a").is_some());
}

#[test]
fn init_refuses_without_bwrap_unless_the_jail_is_off() {
    // A PATH with git and sh but no bwrap.
    let bin = TempDir::new().unwrap();
    for tool in ["git", "sh"] {
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .unwrap();
        let real = String::from_utf8(out.stdout).unwrap();
        std::os::unix::fs::symlink(real.trim(), bin.path().join(tool)).unwrap();
    }
    let init_without_bwrap = |repo: &DirectorRepo, home: &Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_dashboard"))
            .args(["--home", home.to_str().unwrap(), "init", "--name", "proj"])
            .current_dir(repo.path())
            .env("PATH", bin.path())
            .output()
            .unwrap()
    };

    let repo = director_repo();
    let home_dir = temp_home();
    let home = home_dir.path().join("home");
    let out = init_without_bwrap(&repo, &home);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("sandbox preflight failed"), "{stderr}");
    assert!(stderr.contains("install bubblewrap"), "{stderr}");
    assert!(
        !home.join("hub.git").exists(),
        "no hub on a failed preflight"
    );

    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!("{roadmap}\n[sandbox]\nenabled = false\n"),
    );
    commit(repo.path(), "no jail");
    let out = init_without_bwrap(&repo, &home);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("sandbox: off"), "{stdout}");
}

#[test]
fn a_bad_writable_path_is_rejected_at_init() {
    let repo = director_repo();
    let roadmap = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(
        repo.path(),
        "roadmap.toml",
        &format!("{roadmap}\n[sandbox]\nwritable = [\"relative/dir\"]\n"),
    );
    commit(repo.path(), "bad jail");
    let home_dir = temp_home();
    let err = init::init(
        repo.path(),
        Some("proj"),
        Some(home_dir.path().join("home")),
    )
    .unwrap_err();
    assert!(err.contains("writable"), "{err}");
}
