// Thin synchronous wrapper around the `git` CLI (spec.md's player shells out
// to git rather than linking a git library).
#![allow(dead_code)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// SHA-1 of the empty tree — every git repo has this object.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug, Clone)]
pub(crate) struct GitError {
    pub(crate) args: Vec<String>,
    pub(crate) code: Option<i32>,
    pub(crate) stderr: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "git {} failed (exit {:?}): {}", self.args.join(" "), self.code, self.stderr.trim())
    }
}

impl std::error::Error for GitError {}

pub(crate) struct Git {
    pub(crate) dir: PathBuf,
}

impl Git {
    pub(crate) fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn err(args: &[&str], code: Option<i32>, stderr: impl Into<String>) -> GitError {
        GitError { args: args.iter().map(|s| s.to_string()).collect(), code, stderr: stderr.into() }
    }

    /// Runs `git <args>` and returns stdout with the trailing newline trimmed.
    pub(crate) fn run(&self, args: &[&str]) -> Result<String, GitError> {
        let output = Command::new("git").args(args).current_dir(&self.dir).output().map_err(|e| Self::err(args, None, e.to_string()))?;
        if output.status.success() {
            let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
            if s.ends_with('\n') {
                s.pop();
            }
            Ok(s)
        } else {
            Err(Self::err(args, output.status.code(), String::from_utf8_lossy(&output.stderr)))
        }
    }

    /// Runs `git <args>`, returning only whether it exited 0.
    pub(crate) fn run_ok(&self, args: &[&str]) -> bool {
        Command::new("git").args(args).current_dir(&self.dir).output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// Runs `git <args>` feeding `input` on stdin.
    pub(crate) fn run_with_stdin(&self, args: &[&str], input: &str) -> Result<String, GitError> {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Self::err(args, None, e.to_string()))?;
        child.stdin.take().expect("piped stdin").write_all(input.as_bytes()).map_err(|e| Self::err(args, None, e.to_string()))?;
        let output = child.wait_with_output().map_err(|e| Self::err(args, None, e.to_string()))?;
        if output.status.success() {
            let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
            if s.ends_with('\n') {
                s.pop();
            }
            Ok(s)
        } else {
            Err(Self::err(args, output.status.code(), String::from_utf8_lossy(&output.stderr)))
        }
    }

    // ---- helpers ----

    pub(crate) fn rev_parse(&self, rev: &str) -> Option<String> {
        self.run(&["rev-parse", "--verify", "-q", rev]).ok().filter(|s| !s.is_empty())
    }

    pub(crate) fn is_ancestor(&self, a: &str, b: &str) -> bool {
        self.run_ok(&["merge-base", "--is-ancestor", a, b])
    }

    pub(crate) fn ref_exists(&self, name: &str) -> bool {
        self.run_ok(&["show-ref", "--verify", "--quiet", name])
    }

    pub(crate) fn update_ref(&self, name: &str, sha: &str) -> Result<(), GitError> {
        self.run(&["update-ref", name, sha]).map(|_| ())
    }

    pub(crate) fn delete_ref(&self, name: &str) -> Result<(), GitError> {
        self.run(&["update-ref", "-d", name]).map(|_| ())
    }

    /// Returns `(refname, sha)` pairs for every ref matching `pattern`.
    pub(crate) fn for_each_ref(&self, pattern: &str) -> Vec<(String, String)> {
        match self.run(&["for-each-ref", "--format=%(objectname) %(refname)", pattern]) {
            Ok(out) => out
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, ' ');
                    let sha = parts.next()?.to_string();
                    let refname = parts.next()?.to_string();
                    Some((refname, sha))
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub(crate) fn show_file(&self, rev: &str, path: &str) -> Option<String> {
        self.run(&["show", &format!("{rev}:{path}")]).ok()
    }

    pub(crate) fn blob_hash(&self, rev: &str, path: &str) -> Option<String> {
        self.run(&["rev-parse", "--verify", "-q", &format!("{rev}:{path}")]).ok().filter(|s| !s.is_empty())
    }

    /// Recursive, name-only listing of files under `path_prefix` at `rev`.
    pub(crate) fn ls_tree_files(&self, rev: &str, path_prefix: &str) -> Vec<String> {
        match self.run(&["ls-tree", "-r", "--name-only", rev, "--", path_prefix]) {
            Ok(out) => out.lines().filter(|s| !s.is_empty()).map(|s| s.to_string()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// `(status, path)` pairs from `git diff --name-status --no-renames a b`.
    pub(crate) fn diff_names(&self, a: &str, b: &str) -> Vec<(char, String)> {
        match self.run(&["diff", "--name-status", "--no-renames", a, b]) {
            Ok(out) => out
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, '\t');
                    let status = parts.next()?.chars().next()?;
                    let path = parts.next()?.to_string();
                    Some((status, path))
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub(crate) fn rev_list(&self, range_args: &[&str]) -> Vec<String> {
        let mut args = vec!["rev-list"];
        args.extend_from_slice(range_args);
        match self.run(&args) {
            Ok(out) => out.lines().filter(|s| !s.is_empty()).map(|s| s.to_string()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub(crate) fn parents(&self, sha: &str) -> Vec<String> {
        match self.run(&["rev-list", "--parents", "-n", "1", sha]) {
            Ok(out) => out.split_whitespace().skip(1).map(|s| s.to_string()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub(crate) fn commit_subject(&self, sha: &str) -> Option<String> {
        self.run(&["log", "-1", "--format=%s", sha]).ok()
    }

    pub(crate) fn commit_body(&self, sha: &str) -> Option<String> {
        self.run(&["log", "-1", "--format=%b", sha]).ok()
    }

    /// True when `sha`'s tree equals its first parent's tree; a root commit
    /// (no parents) counts as empty only if its own tree is the empty tree.
    pub(crate) fn is_empty_commit(&self, sha: &str) -> bool {
        let tree = match self.run(&["rev-parse", &format!("{sha}^{{tree}}")]) {
            Ok(t) => t,
            Err(_) => return false,
        };
        match self.parents(sha).first() {
            Some(parent) => match self.run(&["rev-parse", &format!("{parent}^{{tree}}")]) {
                Ok(ptree) => tree == ptree,
                Err(_) => false,
            },
            None => tree == EMPTY_TREE_SHA,
        }
    }

    pub(crate) fn notes_show(&self, notes_ref: &str, sha: &str) -> Option<String> {
        self.run(&["notes", "--ref", notes_ref, "show", sha]).ok()
    }

    /// Force-overwrites the note on `sha` under `notes_ref` with `content`.
    pub(crate) fn notes_add(&self, notes_ref: &str, sha: &str, content: &str) -> Result<(), GitError> {
        self.run_with_stdin(&["notes", "--ref", notes_ref, "add", "-f", "-F", "-", sha], content).map(|_| ())
    }

    pub(crate) fn config_get(&self, key: &str) -> Option<String> {
        self.run(&["config", "--get", key]).ok().filter(|s| !s.is_empty())
    }

    pub(crate) fn config_set(&self, key: &str, val: &str) -> Result<(), GitError> {
        self.run(&["config", key, val]).map(|_| ())
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn init_repo() -> (TempDir, Git) {
        let dir = TempDir::new().unwrap();
        let git = Git::new(dir.path());
        git.run(&["init", "-q", "-b", "main"]).unwrap();
        git.run(&["config", "user.name", "Test User"]).unwrap();
        git.run(&["config", "user.email", "test@example.com"]).unwrap();
        (dir, git)
    }

    fn commit_file(dir: &TempDir, git: &Git, name: &str, content: &str, msg: &str) -> String {
        fs::write(dir.path().join(name), content).unwrap();
        git.run(&["add", name]).unwrap();
        git.run(&["commit", "-q", "-m", msg]).unwrap();
        git.rev_parse("HEAD").unwrap()
    }

    #[test]
    fn init_and_rev_parse() {
        let (dir, git) = init_repo();
        assert!(git.rev_parse("HEAD").is_none());
        let sha = commit_file(&dir, &git, "a.txt", "hello\n", "first");
        assert_eq!(git.rev_parse("HEAD").as_deref(), Some(sha.as_str()));
        assert!(git.rev_parse("nonexistent-ref").is_none());
    }

    #[test]
    fn is_ancestor_true_and_false() {
        let (dir, git) = init_repo();
        let first = commit_file(&dir, &git, "a.txt", "1\n", "first");
        let second = commit_file(&dir, &git, "a.txt", "2\n", "second");
        assert!(git.is_ancestor(&first, &second));
        assert!(!git.is_ancestor(&second, &first));
    }

    #[test]
    fn diff_names_reports_changes() {
        let (dir, git) = init_repo();
        let first = commit_file(&dir, &git, "a.txt", "1\n", "first");
        fs::write(dir.path().join("b.txt"), "new\n").unwrap();
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        git.run(&["add", "-A"]).unwrap();
        git.run(&["commit", "-q", "-m", "second"]).unwrap();
        let second = git.rev_parse("HEAD").unwrap();
        let mut names = git.diff_names(&first, &second);
        names.sort();
        assert_eq!(names, vec![('A', "b.txt".to_string()), ('D', "a.txt".to_string())]);
    }

    #[test]
    fn is_empty_commit_detects_marker_commits() {
        let (dir, git) = init_repo();
        let first = commit_file(&dir, &git, "a.txt", "1\n", "first");
        assert!(!git.is_empty_commit(&first));

        git.run(&["commit", "-q", "--allow-empty", "-m", "accept: marker"]).unwrap();
        let marker = git.rev_parse("HEAD").unwrap();
        assert!(git.is_empty_commit(&marker));
    }

    #[test]
    fn is_empty_commit_root_commit() {
        let dir = TempDir::new().unwrap();
        let git = Git::new(dir.path());
        git.run(&["init", "-q", "-b", "main"]).unwrap();
        git.run(&["config", "user.name", "Test User"]).unwrap();
        git.run(&["config", "user.email", "test@example.com"]).unwrap();
        git.run(&["commit", "-q", "--allow-empty", "-m", "root"]).unwrap();
        let root = git.rev_parse("HEAD").unwrap();
        assert!(git.is_empty_commit(&root));
    }

    #[test]
    fn notes_round_trip() {
        let (dir, git) = init_repo();
        let sha = commit_file(&dir, &git, "a.txt", "1\n", "first");
        assert!(git.notes_show("refs/notes/dashboard", &sha).is_none());
        git.notes_add("refs/notes/dashboard", &sha, "hello note").unwrap();
        assert_eq!(git.notes_show("refs/notes/dashboard", &sha).as_deref(), Some("hello note"));
        // Force-overwrite.
        git.notes_add("refs/notes/dashboard", &sha, "updated note").unwrap();
        assert_eq!(git.notes_show("refs/notes/dashboard", &sha).as_deref(), Some("updated note"));
    }

    #[test]
    fn config_get_and_set() {
        let (_dir, git) = init_repo();
        assert!(git.config_get("dashboard.home").is_none());
        git.config_set("dashboard.home", "/tmp/somewhere").unwrap();
        assert_eq!(git.config_get("dashboard.home").as_deref(), Some("/tmp/somewhere"));
    }

    #[test]
    fn for_each_ref_and_update_ref() {
        let (dir, git) = init_repo();
        let sha = commit_file(&dir, &git, "a.txt", "1\n", "first");
        git.update_ref("refs/heads/task/foo", &sha).unwrap();
        let refs = git.for_each_ref("refs/heads/task/*");
        assert_eq!(refs, vec![("refs/heads/task/foo".to_string(), sha.clone())]);
        git.delete_ref("refs/heads/task/foo").unwrap();
        assert!(!git.ref_exists("refs/heads/task/foo"));
    }

    #[test]
    fn ls_tree_files_lists_recursively() {
        let (dir, git) = init_repo();
        fs::create_dir_all(dir.path().join("tasks/foo")).unwrap();
        fs::write(dir.path().join("tasks/foo/task.toml"), "title = \"x\"\n").unwrap();
        git.run(&["add", "-A"]).unwrap();
        git.run(&["commit", "-q", "-m", "add task"]).unwrap();
        let files = git.ls_tree_files("HEAD", "tasks/");
        assert_eq!(files, vec!["tasks/foo/task.toml".to_string()]);
    }

    #[test]
    fn blob_hash_matches_show_file() {
        let (dir, git) = init_repo();
        commit_file(&dir, &git, "a.txt", "hello\n", "first");
        let hash = git.blob_hash("HEAD", "a.txt").unwrap();
        assert!(!hash.is_empty());
        assert_eq!(git.show_file("HEAD", "a.txt").as_deref(), Some("hello"));
        assert!(git.blob_hash("HEAD", "missing.txt").is_none());
    }
}
