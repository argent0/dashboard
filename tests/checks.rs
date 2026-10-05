// Integration tests for src/checks.rs: the check runner (worktree
// management, thresholds, caching as git notes) and the suite/task-check
// wrappers. Every test builds a fresh director repo, initializes a hub for
// it in a TempDir via `dashboard::init::init`, and drives `Runner` against
// that hub.

mod common;

use common::*;
use dashboard::checks::{self, Runner};
use dashboard::config::Check;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use tempfile::TempDir;

/// Initializes a hub for `repo` inside its own TempDir and returns the
/// `Home` pointing at it, plus the guard keeping that TempDir alive.
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

fn check(name: &str, cmd: &str, runs: u32, min_pass: u32) -> Check {
    Check {
        name: name.to_string(),
        cmd: cmd.to_string(),
        runs,
        min_pass,
        retry_failed: 0,
        retry_on: vec![],
    }
}

// --------------------------------------------------------------------------
// Deterministic pass / fail
// --------------------------------------------------------------------------

#[test]
fn deterministic_pass_and_fail() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let ok = check("ok", "true", 1, 1);
    let result = runner.run_check(&commit, &ok);
    assert!(result.pass);
    assert_eq!(result.exits, vec![0]);
    assert!(
        result.output_tail.is_none(),
        "a fully-passing check should have no output_tail"
    );

    let bad = check("bad", "false", 1, 1);
    let result = runner.run_check(&commit, &bad);
    assert!(!result.pass);
    assert_eq!(result.exits, vec![1]);
    assert!(result.output_tail.is_some());
}

// --------------------------------------------------------------------------
// Threshold checks
// --------------------------------------------------------------------------

#[test]
fn threshold_passes_at_min_pass_2_fails_at_min_pass_3() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let cmd =
        "n=$(cat count 2>/dev/null || echo 0); n=$((n+1)); echo $n > count; [ $((n % 2)) -eq 0 ]";

    let lenient = check("flaky", cmd, 4, 2);
    let result = runner.run_check(&commit, &lenient);
    assert!(
        result.pass,
        "min_pass=2 over 4 runs (2 evens) should pass: {result:?}"
    );

    // A different threshold is a different check as far as the cache is
    // concerned, so this re-runs from a freshly-cleaned worktree (fresh
    // counter file) rather than reusing the cached verdict above.
    let strict = check("flaky", cmd, 4, 3);
    let result = runner.run_check(&commit, &strict);
    assert!(
        !result.pass,
        "min_pass=3 over 4 runs (2 evens) should fail: {result:?}"
    );
}

// --------------------------------------------------------------------------
// Cache behavior
// --------------------------------------------------------------------------

#[test]
fn cache_is_hit_on_second_call() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let log_dir = TempDir::new().unwrap();
    let log_path = log_dir.path().join("log.txt");
    let cmd = format!("echo run >> {}", log_path.display());
    let c = check("logged", &cmd, 1, 1);

    let first = runner.run_check(&commit, &c);
    assert!(first.pass);
    let second = runner.run_check(&commit, &c);
    assert_eq!(
        second, first,
        "a cache hit must return exactly the cached result"
    );

    let lines = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(
        lines.lines().count(),
        1,
        "the cache hit must not run the command again"
    );
}

#[test]
fn changed_threshold_misses_the_cache() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let log_dir = TempDir::new().unwrap();
    let log_path = log_dir.path().join("log.txt");
    let cmd = format!("echo run >> {}", log_path.display());

    let once = check("logged", &cmd, 1, 1);
    runner.run_check(&commit, &once);

    let twice = check("logged", &cmd, 2, 1);
    runner.run_check(&commit, &twice);

    let lines = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(
        lines.lines().count(),
        2,
        "a changed runs/min_pass must not reuse the cached result"
    );
}

#[test]
fn notes_merge_keeps_other_checks() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    runner.run_check(&commit, &check("unit", "true", 1, 1));
    runner.run_check(&commit, &check("lint", "true", 1, 1));

    let hub = Git::new(home.hub());
    let mut cached = checks::cached(&hub, &commit);
    cached.sort_by(|a, b| a.name.cmp(&b.name));
    let names: Vec<&str> = cached.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["lint", "unit"],
        "writing the second check's note must not drop the first"
    );
}

// --------------------------------------------------------------------------
// Suite
// --------------------------------------------------------------------------

#[test]
fn suite_with_missing_tests_toml_fails_with_error() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);

    // The initial commit (seeded by director_repo()) has tests.toml; commit
    // a removal on top and check *that* commit specifically.
    std::fs::remove_file(repo.path().join("tests.toml")).unwrap();
    let commit = commit(repo.path(), "drop tests.toml");
    push_main_to_hub(repo.path());

    let result = runner.run_suite(&commit);
    assert!(!result.pass);
    assert!(result.error.is_some());
    assert!(result.checks.is_empty());
    assert!(
        result.summary().contains("tests.toml"),
        "unexpected summary: {}",
        result.summary()
    );
}

#[test]
fn suite_passes_when_every_check_passes() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    // director_repo()'s tests.toml has a single `cmd = "true"` check.
    let result = runner.run_suite(&commit);
    assert!(result.pass, "unexpected: {}", result.summary());
    assert_eq!(result.checks.len(), 1);
    assert!(result.error.is_none());
}

// --------------------------------------------------------------------------
// Task check
// --------------------------------------------------------------------------

#[test]
fn run_task_check_reads_task_toml() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);

    write_file(
        repo.path(),
        "tasks/foo/task.toml",
        "title = \"Foo\"\n\n[check]\nname = \"foo-check\"\ncmd = \"true\"\n",
    );
    let sha = commit(repo.path(), "plan: foo");
    push_main_to_hub(repo.path());

    let result = runner
        .run_task_check(&sha, "foo")
        .expect("task check should be found and run");
    assert_eq!(result.name, "foo-check");
    assert!(result.pass);
}

#[test]
fn run_task_check_errors_when_task_toml_is_missing() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let err = runner.run_task_check(&commit, "nonexistent").unwrap_err();
    assert!(err.contains("task.toml"), "unexpected error: {err}");
}

// --------------------------------------------------------------------------
// Worktree management
// --------------------------------------------------------------------------

#[test]
fn checking_two_different_commits_checks_each_out_correctly() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);

    let first = repo.git().rev_parse("HEAD").unwrap();

    write_file(repo.path(), "marker.txt", "present\n");
    let second = commit(repo.path(), "add marker.txt");
    push_main_to_hub(repo.path());

    let presence = check("presence", "test -f marker.txt", 1, 1);
    // First commit predates marker.txt: the worktree is created fresh here.
    let result_first = runner.run_check(&first, &presence);
    assert!(
        !result_first.pass,
        "marker.txt should not exist at the first commit"
    );

    // Second commit has it: this reuses (checks out + cleans) the same
    // worktree that was just prepared for `first`.
    let result_second = runner.run_check(&second, &presence);
    assert!(
        result_second.pass,
        "marker.txt should exist at the second commit"
    );
}

#[test]
fn untracked_files_from_a_previous_run_are_cleaned() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let leaves_stray_file = check("stray", "touch stray.txt", 1, 1);
    let result = runner.run_check(&commit, &leaves_stray_file);
    assert!(result.pass);

    // Re-preparing the (now-existing) worktree for the same commit must
    // `git clean -ffd` away stray.txt before the next check runs.
    let checks_absence = check("no-stray", "test ! -f stray.txt", 1, 1);
    let result = runner.run_check(&commit, &checks_absence);
    assert!(
        result.pass,
        "stray.txt from the previous run should have been cleaned"
    );
}

// --------------------------------------------------------------------------
// output_tail
// --------------------------------------------------------------------------

#[test]
fn output_tail_is_captured_on_failure() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let failing = check("boom", "echo failmsg 1>&2; exit 1", 1, 1);
    let result = runner.run_check(&commit, &failing);
    assert!(!result.pass);
    let tail = result
        .output_tail
        .expect("a failing check must capture an output tail");
    assert!(tail.contains("failmsg"), "unexpected tail: {tail}");
}

// --------------------------------------------------------------------------
// retry_failed / retry_on (feature request 1a, 1b)
// --------------------------------------------------------------------------

/// A command that fails (printing `msg`) until it has been run `fails` times
/// in total, counting in a file under `dir`.
fn fails_first(dir: &std::path::Path, fails: u32, msg: &str) -> String {
    let f = dir.join("n");
    format!(
        "n=$(cat {f} 2>/dev/null || echo 0); echo $((n+1)) > {f}; if [ $n -lt {fails} ]; then echo '{msg}'; exit 1; fi",
        f = f.display()
    )
}

fn retrying(name: &str, cmd: &str, retry_failed: u32, retry_on: &[&str]) -> Check {
    Check {
        retry_failed,
        retry_on: retry_on.iter().map(|s| s.to_string()).collect(),
        ..check(name, cmd, 1, 1)
    }
}

#[test]
fn a_failing_check_passes_on_retry_and_says_so() {
    let repo = director_repo();
    let (home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let c = retrying(
        "flaky",
        &fails_first(home_dir.path(), 1, "Text file busy"),
        1,
        &[],
    );
    let result = runner.run_check(&commit, &c);
    assert!(result.pass && result.flaky(), "{result:?}");
    assert_eq!(result.retries, 1);
    assert!(result.retry_outputs[0].contains("Text file busy"));
    // Cached: no third run.
    assert_eq!(runner.run_check(&commit, &c).retries, 1);
}

#[test]
fn a_check_failing_every_attempt_fails_once_after_n_retries() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let result = runner.run_check(&commit, &retrying("bad", "false", 2, &[]));
    assert!(!result.pass && !result.flaky());
    assert_eq!(result.retries, 2);
    assert_eq!(result.retry_outputs.len(), 2);
}

#[test]
fn retry_on_only_retries_matching_failures() {
    let repo = director_repo();
    let (home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    let real = retrying(
        "real",
        &fails_first(home_dir.path(), 1, "assertion failed"),
        3,
        &["Text file busy", "os error 26"],
    );
    let result = runner.run_check(&commit, &real);
    assert!(!result.pass);
    assert_eq!(
        result.retries, 0,
        "an unlisted failure is a strike immediately"
    );

    let other = TempDir::new().unwrap();
    let env = retrying(
        "env",
        &fails_first(other.path(), 1, "oops: os error 26"),
        3,
        &["Text file busy", "os error 26"],
    );
    assert!(runner.run_check(&commit, &env).flaky());
}

#[test]
fn changing_the_retry_policy_invalidates_the_cached_verdict() {
    let repo = director_repo();
    let (_home_dir, home) = init_hub(&repo);
    let runner = Runner::new(&home);
    let commit = repo.git().rev_parse("HEAD").unwrap();

    assert_eq!(
        runner
            .run_check(&commit, &retrying("c", "false", 0, &[]))
            .retries,
        0
    );
    assert_eq!(
        runner
            .run_check(&commit, &retrying("c", "false", 1, &[]))
            .retries,
        1
    );
}
