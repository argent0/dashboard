// Thin synchronous wrapper around the `git` CLI (spec.md's player shells out
// to git rather than linking a git library).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// SHA-1 of the empty tree — every git repo has this object.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug, Clone)]
pub struct GitError {
    pub args: Vec<String>,
    pub code: Option<i32>,
    pub stderr: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "git {} failed (exit {:?}): {}",
            self.args.join(" "),
            self.code,
            self.stderr.trim()
        )
    }
}

impl std::error::Error for GitError {}

pub struct Git {
    pub dir: PathBuf,
}

impl Git {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn err(args: &[&str], code: Option<i32>, stderr: impl Into<String>) -> GitError {
        GitError {
            args: args.iter().map(|s| s.to_string()).collect(),
            code,
            stderr: stderr.into(),
        }
    }

    /// Runs `git <args>` and returns stdout with the trailing newline trimmed.
    pub fn run(&self, args: &[&str]) -> Result<String, GitError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .output()
            .map_err(|e| Self::err(args, None, e.to_string()))?;
        if output.status.success() {
            let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
            if s.ends_with('\n') {
                s.pop();
            }
            Ok(s)
        } else {
            Err(Self::err(
                args,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr),
            ))
        }
    }

    /// Runs `git <args>`, returning only whether it exited 0.
    pub fn run_ok(&self, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Runs `git <args>` feeding `input` on stdin.
    pub fn run_with_stdin(&self, args: &[&str], input: &str) -> Result<String, GitError> {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Self::err(args, None, e.to_string()))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(input.as_bytes())
            .map_err(|e| Self::err(args, None, e.to_string()))?;
        let output = child
            .wait_with_output()
            .map_err(|e| Self::err(args, None, e.to_string()))?;
        if output.status.success() {
            let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
            if s.ends_with('\n') {
                s.pop();
            }
            Ok(s)
        } else {
            Err(Self::err(
                args,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr),
            ))
        }
    }

    // ---- helpers ----

    pub fn rev_parse(&self, rev: &str) -> Option<String> {
        self.run(&["rev-parse", "--verify", "-q", rev])
            .ok()
            .filter(|s| !s.is_empty())
    }

    pub fn is_ancestor(&self, a: &str, b: &str) -> bool {
        self.run_ok(&["merge-base", "--is-ancestor", a, b])
    }

    pub fn ref_exists(&self, name: &str) -> bool {
        self.run_ok(&["show-ref", "--verify", "--quiet", name])
    }

    pub fn update_ref(&self, name: &str, sha: &str) -> Result<(), GitError> {
        self.run(&["update-ref", name, sha]).map(|_| ())
    }

    /// Compare-and-swap ref update: `name` is set to `new` only if it
    /// currently points at `old` (an empty `old` means "must not exist yet",
    /// used for branch creation). Fails atomically otherwise — the player
    /// relies on this so a concurrent write never gets silently clobbered.
    pub fn update_ref_cas(&self, name: &str, new: &str, old: &str) -> Result<(), GitError> {
        self.run(&["update-ref", name, new, old]).map(|_| ())
    }

    pub fn delete_ref(&self, name: &str) -> Result<(), GitError> {
        self.run(&["update-ref", "-d", name]).map(|_| ())
    }

    /// Compare-and-delete: removes `name` only if it currently points at
    /// `old` (spec.md §9's retire transition: "delete the branch with an
    /// old-value check").
    pub fn delete_ref_cas(&self, name: &str, old: &str) -> Result<(), GitError> {
        self.run(&["update-ref", "-d", name, old]).map(|_| ())
    }

    /// Moves `from` to `to` in one `update-ref --stdin` transaction: `to`
    /// must not exist yet and `from` must still point at `sha`, else
    /// neither changes.
    pub fn move_ref_cas(&self, from: &str, to: &str, sha: &str) -> Result<(), GitError> {
        self.run_with_stdin(
            &["update-ref", "--stdin"],
            &format!("create {to} {sha}\ndelete {from} {sha}\n"),
        )
        .map(|_| ())
    }

    /// Returns `(refname, sha)` pairs for every ref matching `pattern`.
    pub fn for_each_ref(&self, pattern: &str) -> Vec<(String, String)> {
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

    pub fn show_file(&self, rev: &str, path: &str) -> Option<String> {
        self.run(&["show", &format!("{rev}:{path}")]).ok()
    }

    pub fn blob_hash(&self, rev: &str, path: &str) -> Option<String> {
        self.run(&["rev-parse", "--verify", "-q", &format!("{rev}:{path}")])
            .ok()
            .filter(|s| !s.is_empty())
    }

    /// Recursive, name-only listing of files under `path_prefix` at `rev`.
    pub fn ls_tree_files(&self, rev: &str, path_prefix: &str) -> Vec<String> {
        match self.run(&["ls-tree", "-r", "--name-only", rev, "--", path_prefix]) {
            Ok(out) => out
                .lines()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// `(status, path)` pairs from `git diff --name-status --no-renames a b`.
    pub fn diff_names(&self, a: &str, b: &str) -> Vec<(char, String)> {
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

    pub fn rev_list(&self, range_args: &[&str]) -> Vec<String> {
        let mut args = vec!["rev-list"];
        args.extend_from_slice(range_args);
        match self.run(&args) {
            Ok(out) => out
                .lines()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn parents(&self, sha: &str) -> Vec<String> {
        match self.run(&["rev-list", "--parents", "-n", "1", sha]) {
            Ok(out) => out
                .split_whitespace()
                .skip(1)
                .map(|s| s.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn commit_subject(&self, sha: &str) -> Option<String> {
        self.run(&["log", "-1", "--format=%s", sha]).ok()
    }

    pub fn commit_body(&self, sha: &str) -> Option<String> {
        self.run(&["log", "-1", "--format=%b", sha]).ok()
    }

    /// True when `sha`'s tree equals its first parent's tree; a root commit
    /// (no parents) counts as empty only if its own tree is the empty tree.
    pub fn is_empty_commit(&self, sha: &str) -> bool {
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

    /// The objects `notes_ref` has a note on (empty if the ref doesn't exist).
    pub fn notes_list(&self, notes_ref: &str) -> Vec<String> {
        self.run(&["notes", "--ref", notes_ref, "list"])
            .map(|out| {
                out.lines()
                    .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn notes_show(&self, notes_ref: &str, sha: &str) -> Option<String> {
        self.run(&["notes", "--ref", notes_ref, "show", sha]).ok()
    }

    /// Force-overwrites the note on `sha` under `notes_ref` with `content`.
    pub fn notes_add(&self, notes_ref: &str, sha: &str, content: &str) -> Result<(), GitError> {
        self.notes_modify(notes_ref, sha, |_| content.to_string())
    }

    /// Sets the note on `sha` under `notes_ref` to `f(its current text)`,
    /// safe against another process writing the same notes ref: `git notes`
    /// moves the ref without an old-value check, so the note is written on a
    /// private copy of the ref and swapped in with one (`update_ref_cas`),
    /// re-reading and retrying if the ref moved meanwhile. The player and
    /// `dashboard relaunch` both write `refs/notes/dashboard-escalations`.
    pub fn notes_modify(
        &self,
        notes_ref: &str,
        sha: &str,
        mut f: impl FnMut(Option<String>) -> String,
    ) -> Result<(), GitError> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let scratch = format!("refs/notes/scratch/{}-{seq}", std::process::id());
        let mut last = None;
        for _ in 0..20 {
            let old = self.rev_parse(notes_ref);
            match &old {
                Some(old) => self.update_ref(&scratch, old)?,
                None => {
                    let _ = self.delete_ref(&scratch);
                }
            }
            let content = f(self.notes_show(&scratch, sha));
            let written = self
                .run_with_stdin(
                    &["notes", "--ref", &scratch, "add", "-f", "-F", "-", sha],
                    &content,
                )
                .and_then(|_| {
                    let new = self.rev_parse(&scratch).unwrap_or_default();
                    self.update_ref_cas(notes_ref, &new, old.as_deref().unwrap_or(""))
                });
            let _ = self.delete_ref(&scratch);
            match written {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("at least one attempt"))
    }

    /// Runs `git <args>` with extra environment variables set (e.g. explicit
    /// author/committer identity for the player's own commits, spec.md §11).
    pub fn run_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String, GitError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .envs(envs.iter().copied())
            .output()
            .map_err(|e| Self::err(args, None, e.to_string()))?;
        if output.status.success() {
            let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
            if s.ends_with('\n') {
                s.pop();
            }
            Ok(s)
        } else {
            Err(Self::err(
                args,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr),
            ))
        }
    }

    /// `git merge-tree --write-tree <ours> <theirs>`, run without a worktree
    /// (spec.md §11's unblock merge). Returns the merged tree's sha on a
    /// clean merge, or `None` on conflicts (exit code 1) — the caller skips
    /// those and reports them rather than treating them as a hard error.
    pub fn merge_tree_write(&self, ours: &str, theirs: &str) -> Result<Option<String>, GitError> {
        match self.run(&["merge-tree", "--write-tree", ours, theirs]) {
            Ok(out) => Ok(Some(out.lines().next().unwrap_or("").to_string())),
            Err(e) if e.code == Some(1) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `git merge-tree --write-tree --name-only` that also succeeds on
    /// conflicts: the merged tree (conflict markers written into conflicted
    /// files) plus the conflicted paths, empty on a clean merge. A director's
    /// conflict resolution (spec §11) may differ from this tree only on those paths.
    pub fn merge_tree_conflicts(
        &self,
        ours: &str,
        theirs: &str,
    ) -> Result<(String, Vec<String>), GitError> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            ours,
            theirs,
        ];
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .output()
            .map_err(|e| Self::err(&args, None, e.to_string()))?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(Self::err(
                &args,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr),
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let tree = lines.next().unwrap_or("").to_string();
        let conflicts = lines
            .take_while(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();
        Ok((tree, conflicts))
    }

    /// `git commit-tree <tree> -p <parent> ... -m <message>`, with the
    /// player's own author/committer identity forced via env vars so it
    /// never depends on the user's global git config (spec.md §11).
    pub fn commit_tree(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
    ) -> Result<String, GitError> {
        let mut args: Vec<&str> = vec!["commit-tree", tree];
        for p in parents {
            args.push("-p");
            args.push(p);
        }
        args.push("-m");
        args.push(message);
        self.run_with_env(
            &args,
            &[
                ("GIT_AUTHOR_NAME", "dashboard-player"),
                ("GIT_AUTHOR_EMAIL", "dashboard-player@localhost"),
                ("GIT_COMMITTER_NAME", "dashboard-player"),
                ("GIT_COMMITTER_EMAIL", "dashboard-player@localhost"),
            ],
        )
    }

    pub fn config_get(&self, key: &str) -> Option<String> {
        self.run(&["config", "--get", key])
            .ok()
            .filter(|s| !s.is_empty())
    }

    pub fn config_set(&self, key: &str, val: &str) -> Result<(), GitError> {
        self.run(&["config", key, val]).map(|_| ())
    }

    /// Moves the local `refs/heads/<branch>` to `new`, a commit on top of
    /// `tip` (the hub's), for the director command `dashboard <cmd>`. The
    /// local branch may be absent or behind the hub, never ahead: commits
    /// the player has yet to import come first. A checked-out branch moves
    /// with its working tree by a fast-forward, on a clean tree.
    pub fn advance_branch(&self, branch: &str, tip: &str, new: &str, cmd: &str) -> Result<(), String> {
        let full = format!("refs/heads/{branch}");
        let local = self.rev_parse(&full);
        if let Some(local) = &local {
            if !self.is_ancestor(local, tip) {
                return Err(format!(
                    "{branch} has commits the hub's {branch} lacks: let the player import them (or `git reset --hard hub/{branch}` after a rejection) and run `dashboard {cmd}` again"
                ));
            }
        }
        let head = self.run(&["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
        if head == full {
            let dirty = self
                .run(&["status", "--porcelain", "--untracked-files=no"])
                .map_err(|e| e.to_string())?;
            if !dirty.is_empty() {
                return Err(format!("{branch} is checked out with uncommitted changes: commit or stash them first"));
            }
            self.run(&["merge", "--ff-only", "--quiet", new]).map_err(|e| e.to_string())?;
            return Ok(());
        }
        let reflog = format!("dashboard {cmd}");
        let mut args = vec!["update-ref", "-m", reflog.as_str(), full.as_str(), new];
        if let Some(local) = &local {
            args.push(local);
        }
        self.run(&args).map(|_| ()).map_err(|e| e.to_string())
    }

    /// `git sparse-checkout set --no-cone`: this worktree's patterns become
    /// `patterns` (spec §21). Git writes them to the worktree's own config,
    /// so sibling worktrees keep theirs. A modified or untracked file whose
    /// path leaves the patterns stays on disk.
    pub fn sparse_set(&self, patterns: &[String]) -> Result<(), GitError> {
        self.run_with_stdin(&["sparse-checkout", "set", "--no-cone", "--stdin"], &(patterns.join("\n") + "\n"))
            .map(|_| ())
    }

    /// `git sparse-checkout add`: widens this worktree's patterns.
    pub fn sparse_add(&self, patterns: &[String]) -> Result<(), GitError> {
        self.run_with_stdin(&["sparse-checkout", "add", "--stdin"], &(patterns.join("\n") + "\n"))
            .map(|_| ())
    }

    /// This worktree's sparse patterns, one per entry.
    pub fn sparse_list(&self) -> Vec<String> {
        self.run(&["sparse-checkout", "list"])
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Whether this worktree is a sparse checkout.
    pub fn is_sparse(&self) -> bool {
        self.config_get("core.sparseCheckout").as_deref() == Some("true")
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
        git.run(&["config", "user.email", "test@example.com"])
            .unwrap();
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
        assert_eq!(
            names,
            vec![('A', "b.txt".to_string()), ('D', "a.txt".to_string())]
        );
    }

    #[test]
    fn is_empty_commit_detects_marker_commits() {
        let (dir, git) = init_repo();
        let first = commit_file(&dir, &git, "a.txt", "1\n", "first");
        assert!(!git.is_empty_commit(&first));

        git.run(&["commit", "-q", "--allow-empty", "-m", "accept: marker"])
            .unwrap();
        let marker = git.rev_parse("HEAD").unwrap();
        assert!(git.is_empty_commit(&marker));
    }

    #[test]
    fn is_empty_commit_root_commit() {
        let dir = TempDir::new().unwrap();
        let git = Git::new(dir.path());
        git.run(&["init", "-q", "-b", "main"]).unwrap();
        git.run(&["config", "user.name", "Test User"]).unwrap();
        git.run(&["config", "user.email", "test@example.com"])
            .unwrap();
        git.run(&["commit", "-q", "--allow-empty", "-m", "root"])
            .unwrap();
        let root = git.rev_parse("HEAD").unwrap();
        assert!(git.is_empty_commit(&root));
    }

    #[test]
    fn notes_round_trip() {
        let (dir, git) = init_repo();
        let sha = commit_file(&dir, &git, "a.txt", "1\n", "first");
        assert!(git.notes_show("refs/notes/dashboard", &sha).is_none());
        git.notes_add("refs/notes/dashboard", &sha, "hello note")
            .unwrap();
        assert_eq!(
            git.notes_show("refs/notes/dashboard", &sha).as_deref(),
            Some("hello note")
        );
        // Force-overwrite.
        git.notes_add("refs/notes/dashboard", &sha, "updated note")
            .unwrap();
        assert_eq!(
            git.notes_show("refs/notes/dashboard", &sha).as_deref(),
            Some("updated note")
        );
    }

    #[test]
    fn move_ref_cas_moves_only_from_the_expected_value() {
        let (dir, git) = init_repo();
        let a = commit_file(&dir, &git, "a.txt", "1\n", "first");
        let b = commit_file(&dir, &git, "a.txt", "2\n", "second");
        git.update_ref("refs/x/from", &a).unwrap();
        assert!(git.move_ref_cas("refs/x/from", "refs/x/to", &b).is_err());
        assert!(!git.ref_exists("refs/x/to"), "nothing moved");
        git.update_ref("refs/x/taken", &a).unwrap();
        assert!(git.move_ref_cas("refs/x/from", "refs/x/taken", &a).is_err());
        assert_eq!(git.rev_parse("refs/x/from").as_deref(), Some(a.as_str()));
        git.move_ref_cas("refs/x/from", "refs/x/to", &a).unwrap();
        assert!(!git.ref_exists("refs/x/from"));
        assert_eq!(git.rev_parse("refs/x/to").as_deref(), Some(a.as_str()));
    }

    #[test]
    fn notes_modify_never_loses_a_concurrent_write() {
        let (dir, git) = init_repo();
        let sha = commit_file(&dir, &git, "a.txt", "1\n", "first");
        let path = dir.path().to_path_buf();
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let path = path.clone();
                let sha = sha.clone();
                std::thread::spawn(move || {
                    let git = Git::new(&path);
                    for i in 0..5 {
                        git.notes_modify("refs/notes/dashboard", &sha, |old| {
                            format!("{}{w}.{i};", old.unwrap_or_default())
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let text = git.notes_show("refs/notes/dashboard", &sha).unwrap();
        assert_eq!(text.matches(';').count(), 20, "{text}");
        assert!(git.for_each_ref("refs/notes/scratch/*").is_empty());
    }

    #[test]
    fn config_get_and_set() {
        let (_dir, git) = init_repo();
        assert!(git.config_get("dashboard.home").is_none());
        git.config_set("dashboard.home", "/tmp/somewhere").unwrap();
        assert_eq!(
            git.config_get("dashboard.home").as_deref(),
            Some("/tmp/somewhere")
        );
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
