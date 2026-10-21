// Integration tests for `dashboard init` (src/init.rs), exercised both
// through the library function directly and through the built binary.
// Every test passes an explicit --home inside a TempDir so nothing touches
// the real XDG data dir.

mod common;

use common::*;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::prompts;
use std::path::PathBuf;
use tempfile::TempDir;

/// A not-yet-existing `home` path under its own TempDir (init must create it).
fn fresh_home(home_dir: &TempDir) -> PathBuf {
    home_dir.path().join("home")
}

// --------------------------------------------------------------------------
// Happy path
// --------------------------------------------------------------------------

#[test]
fn happy_path_via_library() {
    let repo = director_repo();
    let home_dir = temp_home();
    let home = fresh_home(&home_dir);

    let report =
        init::init(repo.path(), Some("myproj"), Some(home.clone())).expect("init should succeed");
    assert_eq!(report.project, "myproj");

    let home_canon = std::fs::canonicalize(&home).unwrap();
    assert_eq!(report.home, home_canon);
    assert_eq!(
        report.hub,
        Home {
            root: home_canon.clone()
        }
        .hub()
    );

    // Hub is bare.
    let hub = Git::new(&report.hub);
    assert_eq!(hub.config_get("core.bare").as_deref(), Some("true"));

    // Hub main == director main.
    let director_main = repo.git().rev_parse("refs/heads/main").unwrap();
    let hub_main = hub.rev_parse("refs/heads/main").unwrap();
    assert_eq!(director_main, hub_main);

    // Config keys set both ways.
    let director_toplevel = std::fs::canonicalize(repo.path()).unwrap();
    assert_eq!(
        hub.config_get("dashboard.director").as_deref(),
        Some(director_toplevel.to_str().unwrap())
    );
    assert_eq!(
        hub.config_get("dashboard.project").as_deref(),
        Some("myproj")
    );
    assert_eq!(
        repo.git().config_get("dashboard.home").as_deref(),
        Some(home_canon.to_str().unwrap())
    );

    // `hub` remote present and pointing at the hub, and hub/main fetched.
    let remote_url = repo.git().run(&["remote", "get-url", "hub"]).unwrap();
    assert_eq!(std::fs::canonicalize(&remote_url).unwrap(), report.hub);
    assert!(repo.git().ref_exists("refs/remotes/hub/main"));

    // worker clones nest under the director's own tree, not under home,
    // and events.jsonl exists under home.
    assert_eq!(report.director, director_toplevel);
    assert!(paths::workers_root(&report.director).is_dir());
    assert!(Home { root: home_canon }.events().is_file());

    // the director repo's .gitignore now excludes it.
    let gitignore = repo
        .git()
        .show_file("refs/heads/main", ".gitignore")
        .expect(".gitignore should exist on main");
    assert!(
        gitignore.lines().any(|l| l.trim() == "/.dashboard/"),
        "gitignore missing /.dashboard/: {gitignore:?}"
    );
}

#[test]
fn commits_the_starter_prompts_and_keeps_an_existing_one() {
    let repo = director_repo();
    let home_dir = temp_home();
    let report =
        init::init(repo.path(), None, Some(fresh_home(&home_dir))).expect("init should succeed");
    let hub = Git::new(&report.hub);
    assert_eq!(
        hub.show_file("refs/heads/main", "prompts.kdl")
            .unwrap()
            .trim_end(),
        prompts::STARTER.trim_end()
    );

    let repo = director_repo();
    let custom = prompts::STARTER.replace("max-bytes 6144", "max-bytes 7000");
    write_file(repo.path(), "prompts.kdl", &custom);
    commit(repo.path(), "own prompts");
    let home_dir = temp_home();
    let report =
        init::init(repo.path(), None, Some(fresh_home(&home_dir))).expect("init should succeed");
    assert_eq!(
        Git::new(&report.hub)
            .show_file("refs/heads/main", "prompts.kdl")
            .unwrap()
            .trim_end(),
        custom.trim_end()
    );
}

#[test]
fn invalid_prompts_kdl_errors() {
    let repo = director_repo();
    write_file(repo.path(), "prompts.kdl", "max-bytes 100\n");
    commit(repo.path(), "broken prompts");
    let home_dir = temp_home();
    let err = init::init(repo.path(), None, Some(fresh_home(&home_dir))).unwrap_err();
    assert!(
        err.contains("prompts.kdl: missing template(s)"),
        "unexpected error: {err}"
    );
}

#[test]
fn happy_path_via_binary() {
    let repo = director_repo();
    let home_dir = temp_home();
    let home = fresh_home(&home_dir);

    let output = run_dashboard(
        &[
            "init",
            "--name",
            "binproj",
            "--home",
            home.to_str().unwrap(),
        ],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("binproj"), "unexpected stdout: {stdout}");

    let home_canon = std::fs::canonicalize(&home).unwrap();
    let hub = Git::new(&Home { root: home_canon }.hub());
    assert!(hub.ref_exists("refs/heads/main"));
}

// --------------------------------------------------------------------------
// task/* branches are not imported
// --------------------------------------------------------------------------

#[test]
fn task_branches_not_imported() {
    let repo = director_repo();
    repo.git().run(&["branch", "task/foo"]).unwrap();

    let home_dir = temp_home();
    let report =
        init::init(repo.path(), None, Some(fresh_home(&home_dir))).expect("init should succeed");

    let hub = Git::new(&report.hub);
    assert!(!hub.ref_exists("refs/heads/task/foo"));
}

// --------------------------------------------------------------------------
// Missing / invalid config on main
// --------------------------------------------------------------------------

#[test]
fn missing_roadmap_toml_errors() {
    let dir = TempDir::new().unwrap();
    init_git_identity(dir.path());
    write_file(
        dir.path(),
        TESTS_LEAN,
        &*suite_lean("[[check]]\nname = \"unit\"\ncmd = \"true\"\n"),
    );
    commit(dir.path(), "no roadmap.toml here");

    let home_dir = temp_home();
    let err = init::init(dir.path(), None, Some(fresh_home(&home_dir))).unwrap_err();
    assert!(err.contains("roadmap.toml"), "unexpected error: {err}");
}

#[test]
fn invalid_check_threshold_errors() {
    let dir = TempDir::new().unwrap();
    init_git_identity(dir.path());
    write_file(dir.path(), "roadmap.toml", "title = \"X\"\n");
    write_starter(dir.path(), &suite_lean("[[check]]\nname = \"bad\"\ncmd = \"true\"\nruns = 3\nmin_pass = 4\n"));
    commit(dir.path(), "bad threshold");

    let home_dir = temp_home();
    let err = init::init(dir.path(), None, Some(fresh_home(&home_dir))).unwrap_err();
    assert!(err.contains("min_pass"), "unexpected error: {err}");
}

// --------------------------------------------------------------------------
// Re-running init
// --------------------------------------------------------------------------

#[test]
fn rerunning_init_errors_without_touching_existing_hub() {
    let repo = director_repo();
    let home_dir = temp_home();
    let home = fresh_home(&home_dir);

    let report = init::init(repo.path(), Some("proj"), Some(home.clone()))
        .expect("first init should succeed");
    let hub = Git::new(&report.hub);
    let main_before = hub.rev_parse("refs/heads/main").unwrap();

    let err = init::init(repo.path(), Some("proj"), Some(home.clone())).unwrap_err();
    assert!(err.contains("already exists"), "unexpected error: {err}");

    let main_after = hub.rev_parse("refs/heads/main").unwrap();
    assert_eq!(
        main_before, main_after,
        "re-running init must not touch the existing hub"
    );
}

#[test]
fn rerunning_init_via_binary_errors() {
    let repo = director_repo();
    let home_dir = temp_home();
    let home = fresh_home(&home_dir);

    let first = run_dashboard(&["init", "--home", home.to_str().unwrap()], repo.path());
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second = run_dashboard(&["init", "--home", home.to_str().unwrap()], repo.path());
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("already exists"),
        "unexpected stderr: {stderr}"
    );
}

// --------------------------------------------------------------------------
// Running from a subdirectory
// --------------------------------------------------------------------------

#[test]
fn works_from_subdirectory_of_the_clone() {
    let repo = director_repo();
    let subdir = repo.path().join("nested").join("dir");
    std::fs::create_dir_all(&subdir).unwrap();

    let home_dir = temp_home();
    let report = init::init(&subdir, None, Some(fresh_home(&home_dir)))
        .expect("init should succeed from a subdirectory");

    let hub = Git::new(&report.hub);
    assert!(hub.ref_exists("refs/heads/main"));
}
