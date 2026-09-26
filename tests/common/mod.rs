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
