// Integration tests for `dashboard draft speedup|flaky` (src/draft.rs,
// spec §15, §15.1): the director's plan of a test-speedup or a
// flaky-test-fix, drafted from what the player recorded on the hub (main's
// over-budget suite run, the flake ledger) and committed as a plan the
// player imports; and its refusals.

mod common;

use common::*;
use dashboard::checks::Runner;
use dashboard::draft;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::Home;
use dashboard::player::{NullSessions, Player};
use dashboard::records::Transition;
use dashboard::state::{self, Status};
use tempfile::TempDir;

/// A director repo whose suite is `tests` (fixture TOML), with `roadmap`
/// appended to roadmap.toml and the extra `files` committed.
fn repo_with(roadmap: &str, tests: &str, files: &[(&str, &str)]) -> DirectorRepo {
    let repo = director_repo();
    let text = std::fs::read_to_string(repo.path().join("roadmap.toml")).unwrap();
    write_file(repo.path(), "roadmap.toml", &format!("{text}\n{roadmap}\n"));
    write_file(repo.path(), TESTS_LEAN, &suite_lean(tests));
    for (rel, text) in files {
        write_file(repo.path(), rel, text);
    }
    commit(repo.path(), "the suite");
    repo
}

fn init_hub(repo: &DirectorRepo, home_dir: &TempDir) -> Home {
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home")))
        .expect("init should succeed");
    Home { root: report.home }
}

fn open_player(home: &Home) -> Player<NullSessions> {
    Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("player open")
}

/// The player imports `slug`'s drafted plan and the task is ready.
fn assert_plan_imported(player: &mut Player<NullSessions>, hub: &Git, slug: &str, out: &draft::Outcome) {
    let report = player.tick().expect("plan tick");
    assert!(report.rejected.is_empty(), "{report}");
    assert!(
        report.imported.iter().any(|(r, c, t)| r == &format!("refs/heads/task/{slug}") && c == &out.commit && *t == Transition::Plan),
        "{report}"
    );
    let st = state::derive(hub, &|_| false).unwrap();
    let task = st.tasks.iter().find(|t| t.slug == slug).expect("the drafted task");
    assert_eq!(task.status, Status::Ready);
}

const OVER: &str = "test_paths = [\"tests/\"]\nlist_cmd = \"cat tests/inventory.txt\"\n\n\
[[check]]\nname = \"slow\"\ncmd = \"sleep 3\"\n\n[[check]]\nname = \"other\"\ncmd = \"true\"\n";

fn speedup(repo: &DirectorRepo, home: &Home, slug: Option<&str>) -> Result<draft::Outcome, String> {
    let runner = Runner::for_cli(home);
    draft::speedup(&Git::new(home.hub()), &repo.git(), slug, None, runner.exporter())
}

#[test]
fn a_speedup_is_drafted_from_mains_recorded_over_budget_run() {
    let repo = repo_with("[budget]\ncheck_timeout = 1", OVER, &[("tests/inventory.txt", "one\n")]);
    let home_dir = temp_home();
    let home = init_hub(&repo, &home_dir);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    assert_eq!(player.tick().unwrap().main_over_budget.len(), 1);

    let out = speedup(&repo, &home, None).expect("draft speedup");
    assert_eq!(out.draft.slug, "test-speedup");
    assert_eq!(out.draft.phase, "p1");
    assert!(out.draft.todo.is_empty(), "{:?}", out.draft.todo);
    let toml = &out.draft.toml;
    assert!(toml.contains("kind = \"test-speedup\"") && toml.contains("[[slow]]\ncheck = \"slow\""), "{toml}");
    assert!(!toml.contains("\"other\"") && !toml.contains("modules") && !toml.contains("[check]"), "{toml}");
    assert!(toml.contains("killed at check_timeout 1s"), "{toml}");
    // The director's branch, off the hub's main, holding only the plan.
    let tip = repo.git().rev_parse("refs/heads/task/test-speedup");
    assert_eq!(tip.as_deref(), Some(out.commit.as_str()));
    assert_eq!(repo.git().run(&["symbolic-ref", "--short", "HEAD"]).unwrap(), "main");
    assert_plan_imported(&mut player, &hub, "test-speedup", &out);

    // A second draft takes the next free slug; a taken one is refused.
    let err = speedup(&repo, &home, Some("test-speedup")).unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    let out = speedup(&repo, &home, None).expect("second draft");
    assert_eq!(out.draft.slug, "test-speedup-2");
}

#[test]
fn a_speedup_draft_is_refused_within_budget_or_without_list_cmd() {
    let repo = repo_with("", OVER, &[]);
    let home_dir = temp_home();
    let home = init_hub(&repo, &home_dir);
    open_player(&home).tick().unwrap();
    let err = speedup(&repo, &home, None).unwrap_err();
    assert!(err.contains("nothing to draft") && err.contains("no over-budget run"), "{err}");

    let no_list = OVER.replace("list_cmd = \"cat tests/inventory.txt\"\n", "");
    let repo = repo_with("[budget]\ncheck_timeout = 1", &no_list, &[]);
    let home_dir = temp_home();
    let home = init_hub(&repo, &home_dir);
    open_player(&home).tick().unwrap();
    let err = speedup(&repo, &home, None).unwrap_err();
    assert!(err.contains("list_cmd"), "{err}");
    assert!(repo.git().rev_parse("refs/heads/task/test-speedup").is_none(), "a refusal commits nothing");
}

/// A suite whose `unit` check fails its first run, printing `said`, and
/// passes its retry; the counter lives in `home_dir`, outside the worktree.
fn flaky_suite(home_dir: &TempDir, said: &str, files: &str, list: bool) -> String {
    let counter = home_dir.path().join("unit-count");
    let cmd = format!(
        "n=$(cat {c} 2>/dev/null || echo 0); echo $((n+1)) > {c}; if [ $n -lt 1 ]; then echo '{said}'; exit 1; fi",
        c = counter.display()
    );
    format!(
        "test_paths = [\"tests/\"]\n{}\n[[check]]\nname = \"unit\"\ncmd = {cmd:?}\nretry_failed = 1\n{files}\n",
        if list { "list_cmd = \"cat tests/inventory.txt\"\n" } else { "" }
    )
}

const SANDBOX: &str = "[sandbox]\nwritable = [";

fn flaky_repo(home_dir: &TempDir, said: &str, files: &str, list: bool) -> DirectorRepo {
    repo_with(
        &format!("{SANDBOX}{:?}]", home_dir.path().display().to_string()),
        &flaky_suite(home_dir, said, files, list),
        &[
            ("tests/inventory.txt", "keep\nm::flaky\n"),
            ("tests/unit.sh", "run m::flaky\n"),
            ("tests/other.sh", "run keep\n"),
        ],
    )
}

fn flaky(repo: &DirectorRepo, home: &Home, check: &str) -> Result<draft::Outcome, String> {
    let runner = Runner::for_cli(home);
    draft::flaky(&Git::new(home.hub()), &repo.git(), check, None, None, &runner)
}

#[test]
fn a_flaky_test_fix_is_drafted_from_a_recorded_flaky_pass() {
    let home_dir = temp_home();
    let repo = flaky_repo(&home_dir, "test m::flaky ... FAILED", "files = [\"tests/unit.sh\", \"tests/other.sh\"]", true);
    let home = init_hub(&repo, &home_dir);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    player.verify_main().expect("main's suite");
    player.tick().unwrap();
    assert_eq!(state::derive(&hub, &|_| false).unwrap().flakes.len(), 1);

    let out = flaky(&repo, &home, "unit").expect("draft flaky");
    assert_eq!(out.draft.slug, "fix-flaky-unit");
    assert!(out.draft.todo.is_empty(), "{:?}", out.draft.todo);
    let toml = &out.draft.toml;
    assert!(toml.contains("kind = \"flaky-test-fix\""), "{toml}");
    assert!(toml.contains("fix_paths = [\"tests/unit.sh\"]"), "only the file naming the test: {toml}");
    assert!(toml.contains("old = \"m::flaky\"\nnew = \"flaky_stable\""), "{toml}");
    assert!(toml.contains("runs = 20\nmin_pass = 1\nmax_pass = 19"), "{toml}");
    assert!(toml.contains("name = \"unit-stable\"") && toml.contains("min_pass = 20"), "{toml}");
    assert!(toml.contains("Helpers first, keep assertions as strong"), "{toml}");
    assert!(toml.contains("FAILED"), "the failing output is in the brief: {toml}");
    assert!(!toml.contains("TODO"), "{toml}");
    assert_plan_imported(&mut player, &hub, "fix-flaky-unit", &out);

    // A check with no flaky pass recorded is refused.
    let err = flaky(&repo, &home, "keep").unwrap_err();
    assert!(err.contains("nothing to draft") && err.contains("recorded: unit"), "{err}");
}

#[test]
fn a_flaky_draft_marks_what_it_cannot_derive() {
    let home_dir = temp_home();
    let repo = flaky_repo(&home_dir, "boom", "", true);
    let home = init_hub(&repo, &home_dir);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    player.verify_main().expect("main's suite");
    player.tick().unwrap();

    let out = flaky(&repo, &home, "unit").expect("draft flaky");
    assert_eq!(out.draft.todo.len(), 2, "{:?}", out.draft.todo);
    assert!(out.draft.todo[0].starts_with("[[replace]]") && out.draft.todo[1].starts_with("fix_paths"));
    assert!(out.draft.toml.matches("# TODO(director)").count() == 2, "{}", out.draft.toml);
    // Placeholders still make a valid plan, for the director to replan.
    assert_plan_imported(&mut player, &hub, "fix-flaky-unit", &out);
}

#[test]
fn a_flaky_draft_needs_list_cmd() {
    let home_dir = temp_home();
    let repo = flaky_repo(&home_dir, "test m::flaky ... FAILED", "", false);
    let home = init_hub(&repo, &home_dir);
    let mut player = open_player(&home);
    player.verify_main().expect("main's suite");
    let err = flaky(&repo, &home, "unit").unwrap_err();
    assert!(err.contains("list_cmd") && err.contains("flaky-test-fix"), "{err}");
}
