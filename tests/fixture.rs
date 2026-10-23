// Fixture blocks through the player (spec §6.2, §6.6, §8.3): a view holds a
// fixture only for its users; a post changing a shared fixture keeps its
// users' checks passing; a branch that changed one runs every user's
// checks; the worker reads it pinned; a check's `files` never reach into
// one; a task agent writes the fixtures of its modules only.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{NullSessions, Player};
use tempfile::TempDir;

/// Modules `a` and `b`, a fixture `own` of `a` alone and `both` shared.
const MANIFEST: &str = "import Architect\nopen Dashboard\n\narchitecture where\n  max_modules = 2\n\n\
module a where\n  lang = rust\n  paths = [\"a\"]\n  test_cmd = \"for f in a/tests/*.sh; do [ -e $f ] || continue; sh $f || exit 1; done\"\n\n\
module b where\n  lang = rust\n  paths = [\"b\"]\n\n\
root app where\n  lang = rust\n  paths = [\"src\"]\n\n\
api ab : a → b where\n  paths = [\"api\"]\n\n\
fixture own where\n  users = [a]\n  paths = [\"fixtures/a\"]\n\n\
fixture both where\n  users = [a, b]\n  paths = [\"fixtures/ab\"]\n  max_bytes = 100\n";

/// `b_story` sees the shared fixture and never `a`'s own.
const SUITE: &str = "import Architecture\nopen Dashboard\n\n\
check unit where\n  cmd = \"true\"\n  covers = all\n\n\
check a_story where\n  cmd = \"test -f fixtures/a/stand.txt && grep -q ok fixtures/ab/h.txt\"\n  covers = [a]\n\n\
check b_story where\n  cmd = \"test ! -e fixtures/a && grep -q ok fixtures/ab/h.txt\"\n  covers = [b]\n";

fn hub_with_task(modules: &str) -> (DirectorRepo, TempDir, Home, Player<NullSessions>) {
    let repo = director_repo();
    write_lean_arch(repo.path(), MANIFEST, &[("ab", LEAN_AB_IDL)], &[("Tests", SUITE)]);
    for (f, t) in [("a/lib.rs", ""), ("b/lib.rs", ""), ("api/.keep", ""), ("fixtures/a/stand.txt", "x\n"), ("fixtures/ab/h.txt", "ok\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "modules a and b with their fixtures");
    let home_dir = temp_home();
    let report = init::init(repo.path(), Some("proj"), Some(home_dir.path().join("home"))).expect("init");
    let home = Home { root: report.home };
    let mut player = Player::open(Home { root: home.root.clone() }, NullSessions::default()).expect("open");
    let plan = format!("title = \"s\"\nphase = \"p1\"\nmodules = [{modules}]\n");
    director_commit_on_branch(repo.path(), "task/s", "tasks/s/task.toml", &plan, "plan: s");
    player.tick().expect("plan tick");
    (repo, home_dir, home, player)
}

fn post(repo: &DirectorRepo, extra: &[(&str, &str)], check_files: &str) {
    write_file(repo.path(), "tasks/s/prompt.md", "make a/feature.txt\n");
    let check = format!(
        "import Architecture\nopen Dashboard\n\ncheck s_check where\n  cmd = \"test -f a/feature.txt\"\n  covers = [a]\n  files = {check_files}\n"
    );
    write_file(repo.path(), &tests_file("s"), &check);
    for (f, t) in extra {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "post: s");
}

#[test]
fn a_post_breaking_a_shared_fixture_is_rejected_and_its_users_join_the_run_set() {
    let (repo, _home_dir, home, mut player) = hub_with_task("\"a\"");
    let hub = Git::new(home.hub());
    // The post breaks `both`, which `b_story` reads: refused.
    post(&repo, &[("fixtures/ab/h.txt", "bad\n")], "[]");
    let report = player.tick().expect("post tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Post);
    assert!(report.rejected[0].message.contains("the fixture change breaks a_story, b_story (checks of its users a, b)"), "{}", report.rejected[0].message);
    // Extending it instead keeps every user passing, beside a new test
    // that fails until the worker's code (module a's own check runs it):
    // the fixture change is judged alone, so this posts and launches.
    write_file(repo.path(), "fixtures/ab/h.txt", "ok\nmore\n");
    write_file(repo.path(), "a/tests/new.sh", "test -f a/feature.txt\n");
    write_file(
        repo.path(),
        &tests_file("s"),
        "import Architecture\nopen Dashboard\n\ncheck s_check where\n  cmd = \"sh a/tests/new.sh\"\n  covers = [a]\n  files = [\"a/tests/new.sh\"]\n",
    );
    commit(repo.path(), "post: s, the fixture extended");
    let report = player.tick().expect("post tick");
    assert_eq!(report.launched, ["s"], "{report}");
    // The branch changed `both`, so `b` joins the run set of a task on `a`.
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();
    let runner = dashboard::checks::Runner::for_cli(&home);
    let set = runner.run_set_for(&tip, &["a".to_string()], None).expect("run set");
    let names: Vec<&str> = set.checks.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"b_story") && names.contains(&"a_story"), "{names:?}");
    assert!(set.why.contains("and b (users of a fixture it changed)"), "{}", set.why);
    // The worker reads the fixture pinned, in its tree.
    let clone = paths::worker_clone(&repo.director_path(), "s");
    assert!(clone.join("fixtures/ab/h.txt").is_file() && clone.join("fixtures/a/stand.txt").is_file());
    let pinned = dashboard::records::pinned_files(&hub, "s").expect("pins");
    assert!(pinned.contains_key("fixtures/ab/h.txt") && pinned.contains_key("fixtures/a/stand.txt"), "{pinned:?}");
    let git = Git::new(&clone);
    for args in [&["config", "user.name", "w"][..], &["config", "user.email", "w@x"], &["config", "commit.gpgsign", "false"]] {
        git.run(args).unwrap();
    }
    write_file(&clone, "a/feature.txt", "x\n");
    write_file(&clone, "fixtures/ab/h.txt", "ok\nworker\n");
    commit(&clone, "finish: s");
    let report = player.tick().expect("finish tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    assert_eq!(report.rejected[0].reason, Reason::Pins, "{}", report.rejected[0].message);
}

#[test]
fn a_check_declaring_files_inside_a_fixture_is_refused() {
    let (repo, _home_dir, _home, mut player) = hub_with_task("\"a\"");
    post(&repo, &[("fixtures/ab/extra.txt", "x\n")], "[\"fixtures/ab/extra.txt\"]");
    let report = player.tick().expect("post tick");
    assert_eq!(report.rejected.len(), 1, "{report}");
    let msg = &report.rejected[0].message;
    assert!(msg.contains("files \"fixtures/ab/extra.txt\" overlaps fixture both"), "{msg}");
}

#[test]
fn a_task_agent_writes_the_fixtures_of_its_modules_only() {
    let (_repo, _home_dir, home, _player) = hub_with_task("\"b\"");
    let hub = Git::new(home.hub());
    let tip = hub.rev_parse("refs/heads/task/s").unwrap();
    let runner = dashboard::checks::Runner::for_cli(&home);
    let scope = dashboard::records::agent_scope_at(&hub, "s", &tip, Some(runner.exporter()));
    assert!(scope.iter().any(|p| p == "fixtures/ab"), "{scope:?}");
    assert!(!scope.iter().any(|p| p == "fixtures/a"), "{scope:?}");
    // The worker of a task on `b` reads `both`, and `a`'s own stays absent.
    let w = dashboard::records::workspace_at(&hub, "s", &tip, Some(runner.exporter())).expect("workspace");
    let ro: Vec<&str> = w.readonly.iter().map(|(p, _)| p.as_str()).collect();
    assert!(ro.contains(&"fixtures/ab") && !ro.contains(&"fixtures/a"), "{ro:?}");
}
