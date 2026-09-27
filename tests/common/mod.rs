// Shared fixtures for dashboard's integration tests: a throwaway director
// repo with a minimal roadmap.toml/tests.toml, small helpers for writing
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
/// phase) plus `tests.toml` (one deterministic check, `cmd = "true"`)
/// committed on `main`.
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
    /// `dashboard.director` and what `Player`/`paths::subagent_clone`
    /// resolve a subagent's clone directory against.
    pub fn director_path(&self) -> std::path::PathBuf {
        std::fs::canonicalize(self.path()).expect("canonicalize director repo path")
    }
}

pub fn director_repo() -> DirectorRepo {
    let dir = TempDir::new().expect("tempdir");
    init_git_identity(dir.path());

    write_file(dir.path(), "roadmap.toml", "title = \"Test Project\"\n\n[[phases]]\nid = \"p1\"\ntitle = \"Phase One\"\n");
    write_file(dir.path(), "tests.toml", "[[check]]\nname = \"unit\"\ncmd = \"true\"\n");
    commit(dir.path(), "initial roadmap and test suite");

    DirectorRepo { dir }
}

/// `git init -b main` plus a committer identity and gpg signing disabled, so
/// commits made in tests don't depend on the host's global git config.
pub fn init_git_identity(dir: &Path) {
    let git = Git::new(dir);
    git.run(&["init", "-q", "-b", "main"]).expect("git init");
    git.run(&["config", "user.name", "Test User"]).expect("git config user.name");
    git.run(&["config", "user.email", "test@example.com"]).expect("git config user.email");
    git.run(&["config", "commit.gpgsign", "false"]).expect("git config commit.gpgsign");
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
    git.run(&["commit", "-q", "--allow-empty", "-m", msg]).expect("git commit --allow-empty");
    git.rev_parse("HEAD").expect("HEAD after commit")
}

/// A fresh directory under a TempDir suitable as a not-yet-existing `--home`
/// argument (`dashboard init` must create it).
pub fn temp_home() -> TempDir {
    TempDir::new().expect("tempdir")
}

/// Runs the built `dashboard` binary with `args` in `cwd`.
pub fn run_dashboard(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dashboard")).args(args).current_dir(cwd).output().expect("failed to run dashboard binary")
}

/// Checks out `branch` in the director repo, creating it from `main` on
/// first use. Leaves the repo checked out on `branch`.
pub fn checkout_branch(repo: &Path, branch: &str) {
    let git = Git::new(repo);
    if git.ref_exists(&format!("refs/heads/{branch}")) {
        git.run(&["checkout", "-q", branch]).expect("checkout branch");
    } else {
        git.run(&["checkout", "-q", "-b", branch, "main"]).expect("checkout new branch");
    }
}

/// Checks out `branch` (creating it from `main` on first use), writes `rel`,
/// and commits — the common shape of a director plan/replan/post commit used
/// throughout tests/player.rs. Returns the new commit's sha.
pub fn director_commit_on_branch(repo: &Path, branch: &str, rel: &str, content: &str, msg: &str) -> String {
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
    git.run(&["push", "-q", "hub", "main"]).expect("push main to hub");
}

/// Fetches `task/<slug>` from the `hub` remote into the director repo and
/// fast-forwards the local branch onto it, checking it out. This is what a
/// real director does after the player validates a subagent's finish claim
/// (or after any other player-side move, like an unblock merge), so the
/// director's next commit on the branch (accept/retire/send-back) builds on
/// top of it. Requires the repo to have been through `dashboard::init::init`
/// (which sets up the `hub` remote).
pub fn director_pull_task_branch(repo: &Path, slug: &str) {
    checkout_branch(repo, &format!("task/{slug}"));
    let git = Git::new(repo);
    git.run(&["fetch", "-q", "hub", &format!("task/{slug}")]).expect("fetch task branch from hub");
    git.run(&["merge", "-q", "--ff-only", "FETCH_HEAD"]).expect("fast-forward onto hub's task branch");
}

/// Clones `hub_path` into a fresh temp dir, checks out `task/<slug>`, and
/// sets a committer identity — the shared setup behind `subagent_commit` and
/// `subagent_empty_commit` below.
fn subagent_clone(hub_path: &Path, slug: &str) -> TempDir {
    let clone_dir = TempDir::new().expect("tempdir");
    let hub_str = hub_path.to_str().expect("hub path is valid UTF-8");
    let output = Command::new("git").args(["clone", "-q", hub_str, "."]).current_dir(clone_dir.path()).output().expect("failed to run git clone");
    assert!(output.status.success(), "git clone {hub_str} failed: {}", String::from_utf8_lossy(&output.stderr));
    let git = Git::new(clone_dir.path());
    git.run(&["checkout", "-q", &format!("task/{slug}")]).expect("checkout task branch in subagent clone");
    git.run(&["config", "user.name", "Test Subagent"]).expect("git config user.name");
    git.run(&["config", "user.email", "subagent@example.com"]).expect("git config user.email");
    git.run(&["config", "commit.gpgsign", "false"]).expect("git config commit.gpgsign");
    clone_dir
}

/// Simulates the subagent side of a task, which step 6 (real subagent
/// clones, launch, tmux) hasn't implemented yet: clones the hub, checks out
/// `task/<slug>`, writes `rel`, commits, and pushes straight back to the
/// hub's `refs/heads/task/<slug>` — a fast-forward, since the clone was made
/// from that exact tip. This is the closest plumbing-level stand-in for what
/// a real subagent clone will do in step 6. Returns the new commit's sha;
/// the caller still records provenance itself, e.g. via
/// `records::record_import(hub, &sha, Actor::Subagent, Transition::Finish, Some(slug))`,
/// since that validation is the player's job and isn't exercised here.
pub fn subagent_commit(hub_path: &Path, slug: &str, rel: &str, content: &str, msg: &str) -> String {
    let clone_dir = subagent_clone(hub_path, slug);
    write_file(clone_dir.path(), rel, content);
    let sha = commit(clone_dir.path(), msg);
    Git::new(clone_dir.path()).run(&["push", "-q", "origin", &format!("HEAD:refs/heads/task/{slug}")]).expect("push subagent commit to hub");
    sha
}

/// Same as `subagent_commit`, for an empty marker commit — the subagent's
/// only other legitimate commit shape (spec §7's `block:`).
pub fn subagent_empty_commit(hub_path: &Path, slug: &str, msg: &str) -> String {
    let clone_dir = subagent_clone(hub_path, slug);
    let sha = commit_empty(clone_dir.path(), msg);
    Git::new(clone_dir.path()).run(&["push", "-q", "origin", &format!("HEAD:refs/heads/task/{slug}")]).expect("push subagent marker commit to hub");
    sha
}
