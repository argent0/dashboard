// Shared fixtures for dashboard's integration tests: a throwaway director
// repo with a minimal roadmap.toml and Lean package (architecture/ with
// its Tests.lean suite), small helpers for writing
// files and committing (including marker commits), and a helper to invoke
// the built `dashboard` binary. Reused across tests/init.rs and later steps'
// test files.
#![allow(dead_code)]

use dashboard::git::Git;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

/// A throwaway director clone: `git init -b main` with a committer identity
/// and gpg signing disabled, and a minimal `roadmap.toml` (title + one
/// phase) plus the starter Lean package (no modules, a root `app` over
/// `src/`; `Tests.lean` with one
/// deterministic check, `unit`, `cmd = "true"`, `covers = all`) committed
/// on `main`.
pub struct DirectorRepo {
    pub dir: TempDir,
}

impl DirectorRepo {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn git(&self) -> Git {
        Git::new(self.path())
    }

    /// The canonicalized toplevel — matches what `init::init` stores as
    /// `dashboard.director` and what `Player`/`paths::worker_clone`
    /// resolve a worker's clone directory against.
    pub fn director_path(&self) -> std::path::PathBuf {
        std::fs::canonicalize(self.path()).expect("canonicalize director repo path")
    }
}

/// Appended to every fixture `roadmap.toml`: these tests drive the
/// director's own posts and send-backs, so task agents (spec §17) are off
/// unless a test turns them on.
pub const NO_TASK_AGENTS: &str = "\n[player]\nruns = \"parallel\"\ntask_agent_cmd = \"\"\n";

pub fn director_repo() -> DirectorRepo {
    let dir = TempDir::new().expect("tempdir");
    init_git_identity(dir.path());

    write_file(
        dir.path(),
        "roadmap.toml",
        &format!(
            "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n{NO_TASK_AGENTS}"
        ),
    );
    write_starter(dir.path(), &suite_lean("[[check]]\nname = \"unit\"\ncmd = \"true\"\n"));
    write_file(dir.path(), "architecture/Architecture.lean", STARTER_MANIFEST);
    write_file(dir.path(), "src/.keep", "");
    commit(dir.path(), "initial roadmap and test suite");

    DirectorRepo { dir }
}

/// `git init -b main` plus a committer identity and gpg signing disabled, so
/// commits made in tests don't depend on the host's global git config.
pub fn init_git_identity(dir: &Path) {
    let git = Git::new(dir);
    git.run(&["init", "-q", "-b", "main"]).expect("git init");
    git.run(&["config", "user.name", "Test User"])
        .expect("git config user.name");
    git.run(&["config", "user.email", "test@example.com"])
        .expect("git config user.email");
    git.run(&["config", "commit.gpgsign", "false"])
        .expect("git config commit.gpgsign");
}

/// Writes `content` to `<repo>/<rel>`, creating parent directories as needed.
pub fn write_file(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dirs");
    }
    std::fs::write(path, content).expect("write file");
}

/// `git add -A && git commit -m <msg>`, returning the new commit's SHA.
pub fn commit(repo: &Path, msg: &str) -> String {
    let git = Git::new(repo);
    git.run(&["add", "-A"]).expect("git add");
    git.run(&["commit", "-q", "-m", msg]).expect("git commit");
    git.rev_parse("HEAD").expect("HEAD after commit")
}

/// `git commit --allow-empty -m <msg>` — marker commits (block/accept/retire).
pub fn commit_empty(repo: &Path, msg: &str) -> String {
    let git = Git::new(repo);
    git.run(&["commit", "-q", "--allow-empty", "-m", msg])
        .expect("git commit --allow-empty");
    git.rev_parse("HEAD").expect("HEAD after commit")
}

/// A fresh directory under a TempDir suitable as a not-yet-existing `--home`
/// argument (`dashboard init` must create it).
pub fn temp_home() -> TempDir {
    TempDir::new().expect("tempdir")
}

/// Runs the built `dashboard` binary with `args` in `cwd`.
pub fn run_dashboard(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dashboard"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to run dashboard binary")
}

/// Checks out `branch` in the director repo, creating it from `main` on
/// first use. Leaves the repo checked out on `branch`.
pub fn checkout_branch(repo: &Path, branch: &str) {
    let git = Git::new(repo);
    if git.ref_exists(&format!("refs/heads/{branch}")) {
        git.run(&["checkout", "-q", branch])
            .expect("checkout branch");
    } else {
        git.run(&["checkout", "-q", "-b", branch, "main"])
            .expect("checkout new branch");
    }
}

/// Checks out `branch` (creating it from `main` on first use), writes `rel`,
/// and commits — the common shape of a director plan/replan/post commit used
/// throughout tests/player.rs. Returns the new commit's sha.
pub fn director_commit_on_branch(
    repo: &Path,
    branch: &str,
    rel: &str,
    content: &str,
    msg: &str,
) -> String {
    checkout_branch(repo, branch);
    write_file(repo, rel, content);
    commit(repo, msg)
}

/// Pushes `main` from a director clone already linked to its hub (via
/// `dashboard::init::init`) up to that hub. Used by tests that need commits
/// beyond the one `director_repo()` seeds, e.g. to exercise a check runner
/// against several commits.
pub fn push_main_to_hub(repo: &Path) {
    let git = Git::new(repo);
    git.run(&["push", "-q", "hub", "main"])
        .expect("push main to hub");
}

/// Fetches `task/<slug>` from the `hub` remote into the director repo and
/// fast-forwards the local branch onto it, checking it out. This is what a
/// real director does after the player validates a worker's finish claim
/// (or after any other player-side move, like an unblock merge), so the
/// director's next commit on the branch (accept/retire/send-back) builds on
/// top of it. Requires the repo to have been through `dashboard::init::init`
/// (which sets up the `hub` remote).
pub fn director_pull_task_branch(repo: &Path, slug: &str) {
    checkout_branch(repo, &format!("task/{slug}"));
    let git = Git::new(repo);
    git.run(&["fetch", "-q", "hub", &format!("task/{slug}")])
        .expect("fetch task branch from hub");
    git.run(&["merge", "-q", "--ff-only", "FETCH_HEAD"])
        .expect("fast-forward onto hub's task branch");
}

/// Clones `hub_path` into a fresh temp dir, checks out `task/<slug>`, and
/// sets a committer identity — the shared setup behind `worker_commit` and
/// `worker_empty_commit` below.
fn worker_clone(hub_path: &Path, slug: &str) -> TempDir {
    let clone_dir = TempDir::new().expect("tempdir");
    let hub_str = hub_path.to_str().expect("hub path is valid UTF-8");
    let output = Command::new("git")
        .args(["clone", "-q", hub_str, "."])
        .current_dir(clone_dir.path())
        .output()
        .expect("failed to run git clone");
    assert!(
        output.status.success(),
        "git clone {hub_str} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let git = Git::new(clone_dir.path());
    git.run(&["checkout", "-q", &format!("task/{slug}")])
        .expect("checkout task branch in worker clone");
    git.run(&["config", "user.name", "Test Worker"])
        .expect("git config user.name");
    git.run(&["config", "user.email", "worker@example.com"])
        .expect("git config user.email");
    git.run(&["config", "commit.gpgsign", "false"])
        .expect("git config commit.gpgsign");
    clone_dir
}

/// Simulates the worker side of a task, which step 6 (real worker
/// clones, launch, tmux) hasn't implemented yet: clones the hub, checks out
/// `task/<slug>`, writes `rel`, commits, and pushes straight back to the
/// hub's `refs/heads/task/<slug>` — a fast-forward, since the clone was made
/// from that exact tip. This is the closest plumbing-level stand-in for what
/// a real worker clone will do in step 6. Returns the new commit's sha;
/// the caller still records provenance itself, e.g. via
/// `records::record_import(hub, &sha, Actor::Worker, Transition::Finish, Some(slug))`,
/// since that validation is the player's job and isn't exercised here.
pub fn worker_commit(hub_path: &Path, slug: &str, rel: &str, content: &str, msg: &str) -> String {
    let clone_dir = worker_clone(hub_path, slug);
    write_file(clone_dir.path(), rel, content);
    let sha = commit(clone_dir.path(), msg);
    Git::new(clone_dir.path())
        .run(&[
            "push",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/task/{slug}"),
        ])
        .expect("push worker commit to hub");
    sha
}

/// Same as `worker_commit`, for an empty marker commit — the worker's
/// only other legitimate commit shape (spec §7's `block:`).
pub fn worker_empty_commit(hub_path: &Path, slug: &str, msg: &str) -> String {
    let clone_dir = worker_clone(hub_path, slug);
    let sha = commit_empty(clone_dir.path(), msg);
    Git::new(clone_dir.path())
        .run(&[
            "push",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/task/{slug}"),
        ])
        .expect("push worker marker commit to hub");
    sha
}

/// A minimal Lean architecture: two Rust modules `a` and `b` and one API
/// `ab` between them (`api/`), with `a`'s AST budget set.
pub const LEAN_AB_MANIFEST: &str = "import Dashboard\nopen Dashboard\n\narchitecture where\n  max_modules = 2\n\n\
module a where\n  lang = rust\n  paths = [\"a\"]\n  max_ast = 100\n\n\
module b where\n  lang = rust\n  paths = [\"b\"]\n\n\
api ab : a → b where\n  paths = [\"api\"]\n";

/// `ab`'s IDL: one bounded string T and two constructors.
pub const LEAN_AB_IDL: &str = "import Architecture\nopen Dashboard\n\nidl ab where\n  type Id : string 16\n  constructor X : Id\n  constructor Y\n";

/// Writes the Lean package `architecture/` under `repo`: the vendored
/// dashboard library, `Architecture.lean`, one `Apis/<name>.lean` per API
/// and one `<stem>.lean` per suite file (`("Tests", …)`, `("Tests/x", …)`,
/// `("Tests/x.worker", …)`); nothing is committed. Tests using it need
/// `lake` on the host.
pub fn write_lean_arch(repo: &Path, manifest: &str, apis: &[(&str, &str)], tests: &[(&str, &str)]) {
    dashboard::lean::write_vendored(repo).expect("vendor the dashboard library");
    write_file(repo, "architecture/Architecture.lean", manifest);
    for (name, idl) in apis {
        write_file(repo, &format!("architecture/Apis/{name}.lean"), idl);
    }
    for (stem, text) in tests {
        write_file(repo, &format!("architecture/{stem}.lean"), text);
    }
}

/// A suite for the `a`/`b` architecture: `ab_story` covering both with its
/// own `stories` files, `arch` covering all, and `test_paths = ["stories"]`.
pub const LEAN_AB_TESTS: &str = "import Architecture\nopen Dashboard\n\n\
suite where\n  test_paths = [\"stories\"]\n\n\
check ab_story where\n  cmd = \"true\"\n  covers = [a, b]\n  files = [\"stories\"]\n\n\
check arch where\n  cmd = \"true\"\n  covers = all\n";

/// One `check` block: `covers` is `all` or a Lean list like `[a, b]`.
pub fn lean_check(name: &str, cmd: &str, covers: &str) -> String {
    format!("check {} where\n  cmd = {cmd:?}\n  covers = {covers}\n", dashboard::lean::ident(name))
}

/// A per-task suite file (`Tests/<slug>.lean` or `.worker.lean`) holding one
/// check.
pub fn lean_task_tests(name: &str, cmd: &str, covers: &str) -> String {
    format!("import Architecture\nopen Dashboard\n\n{}", lean_check(name, cmd, covers))
}

/// `director_repo()`'s manifest: no modules, one root `app` over `src/`,
/// so every fixture task can name `modules = ["app"]`.
pub const STARTER_MANIFEST: &str =
    "import Dashboard\nopen Dashboard\n\narchitecture where\n  max_modules = 0\n\nroot app where\n  lang = rust\n  paths = [\"src\"]\n";

/// The root `app` over `src/`, to append to another manifest.
pub const APP_ROOT: &str = "\nroot app where\n  lang = rust\n  paths = [\"src\"]\n";

/// `architecture/Tests.lean`.
pub const TESTS_LEAN: &str = "architecture/Tests.lean";

/// `architecture/Tests/<slug>.lean`, a task's post.
pub fn tests_file(slug: &str) -> String {
    format!("architecture/Tests/{slug}.lean")
}

/// The starter package of `dashboard arch gen` (no modules) with `tests`
/// as `Tests.lean`.
pub fn write_starter(repo: &Path, tests: &str) {
    for (rel, text) in dashboard::lean::starter() {
        write_file(repo, &rel, &text);
    }
    dashboard::lean::write_vendored(repo).expect("vendor the dashboard library");
    write_file(repo, TESTS_LEAN, tests);
}

/// A Lean string literal.
pub fn lean_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

fn lean_strs(xs: &[String]) -> String {
    format!("[{}]", xs.iter().map(|x| lean_str(x)).collect::<Vec<_>>().join(", "))
}

/// The checks of a TOML suite text (`test_paths`, `list_cmd`, `[[check]]`
/// tables, as the fixtures wrote them before the suite moved to Lean) as
/// Lean declarations; a check without `covers` gets `default_covers`.
fn lean_checks(toml_text: &str, with_block: bool, default_covers: &str) -> String {
    let t: toml::Table = toml::from_str(toml_text).expect("fixture suite TOML");
    let mut out = String::from("import Architecture\nopen Dashboard\n\n");
    if with_block {
        let paths: Vec<String> = t
            .get("test_paths")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        out.push_str(&format!("suite where\n  test_paths = {}\n", lean_strs(&paths)));
        if let Some(l) = t.get("list_cmd").and_then(|v| v.as_str()) {
            out.push_str(&format!("  list_cmd = some {}\n", lean_str(l)));
        }
        out.push('\n');
    }
    for c in t.get("check").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let c = c.as_table().expect("check table");
        out.push_str(&format!("check {} where\n", dashboard::lean::ident(c["name"].as_str().unwrap())));
        for (k, v) in c {
            let val = match (k.as_str(), v) {
                ("name", _) => continue,
                ("covers", toml::Value::String(s)) if s == "all" => "all".to_string(),
                ("covers", toml::Value::Array(a)) => {
                    format!("[{}]", a.iter().map(|x| dashboard::lean::ident(x.as_str().unwrap())).collect::<Vec<_>>().join(", "))
                }
                (_, toml::Value::String(s)) => lean_str(s),
                (_, toml::Value::Integer(i)) => i.to_string(),
                (_, toml::Value::Array(a)) => {
                    lean_strs(&a.iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>())
                }
                (_, v) => v.to_string(),
            };
            out.push_str(&format!("  {k} = {val}\n"));
        }
        if !c.contains_key("covers") {
            out.push_str(&format!("  covers = {default_covers}\n"));
        }
        out.push('\n');
    }
    out
}

/// `Tests.lean` from a TOML suite text (its `[[check]]`s, `test_paths`
/// and `list_cmd`).
pub fn suite_lean(toml_text: &str) -> String {
    lean_checks(toml_text, true, "all")
}

/// A per-task suite file (`Tests/<slug>.lean`) from `[[check]]` TOML;
/// its checks cover the fixture tasks' module, `app`.
pub fn task_suite_lean(toml_text: &str) -> String {
    lean_checks(toml_text, false, "[app]")
}
