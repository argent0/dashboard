// The task workspace's git ground (spec §21): a worker's worktree is a
// non-cone sparse checkout of the task branch, and `queue::snapshot` of
// such a tree keeps every absent file at its `HEAD` content while staging
// only paths inside the patterns.

mod common;

use common::*;
use dashboard::git::Git;
use dashboard::queue;
use std::path::PathBuf;
use tempfile::TempDir;

/// A repo with `a/lib.rs`, `b/lib.rs` and `stories/s.sh` on `main`, and a
/// worktree of it on `task/s` whose sparse patterns are `patterns`, built
/// sparse from the start (`--no-checkout`, `sparse-checkout set`,
/// `read-tree -mu HEAD`). Returns the repo, the worktree's parent and the
/// worktree.
fn sparse_fixture(patterns: &[&str]) -> (TempDir, TempDir, PathBuf) {
    let repo = TempDir::new().unwrap();
    init_git_identity(repo.path());
    for (f, t) in [("a/lib.rs", "a\n"), ("b/lib.rs", "b\n"), ("stories/s.sh", "true\n")] {
        write_file(repo.path(), f, t);
    }
    commit(repo.path(), "a, b and a story");
    let parent = TempDir::new().unwrap();
    let wt = parent.path().join("wt");
    let git = Git::new(repo.path());
    git.run(&["worktree", "add", "--quiet", "--no-checkout", "-B", "task/s", wt.to_str().unwrap(), "main"])
        .expect("worktree add");
    let w = Git::new(&wt);
    w.run_with_stdin(&["sparse-checkout", "set", "--no-cone", "--stdin"], &(patterns.join("\n") + "\n"))
        .expect("sparse-checkout set");
    w.run(&["read-tree", "-mu", "HEAD"]).expect("read-tree");
    (repo, parent, wt)
}

#[test]
fn a_sparse_worktree_holds_only_its_patterns_and_stays_its_own() {
    let (repo, _parent, wt) = sparse_fixture(&["/a", "/stories"]);
    assert!(wt.join("a/lib.rs").exists() && wt.join("stories/s.sh").exists());
    assert!(!wt.join("b").exists());
    assert_eq!(Git::new(&wt).run(&["show", "HEAD:b/lib.rs"]).unwrap(), "b");
    // The setting is the worktree's own: the main checkout stays full.
    assert_eq!(Git::new(repo.path()).config_get("extensions.worktreeConfig").as_deref(), Some("true"));
    assert!(Git::new(repo.path()).config_get("core.sparseCheckout").is_none_or(|v| v != "true"));
    assert!(repo.path().join("b/lib.rs").exists());
}

#[test]
fn snapshot_keeps_absent_files_and_skips_undeclared_ones() {
    let (_repo, _parent, wt) = sparse_fixture(&["/a", "/stories"]);
    let w = Git::new(&wt);
    write_file(&wt, "a/new.rs", "new\n");
    write_file(&wt, "tests2/new.sh", "true\n");
    let head_b = w.blob_hash("HEAD", "b/lib.rs").unwrap();
    let snap = queue::snapshot(&w).expect("snapshot");
    assert_eq!(w.blob_hash(&snap, "b/lib.rs").as_deref(), Some(head_b.as_str()));
    assert!(w.blob_hash(&snap, "a/new.rs").is_some());
    assert!(w.blob_hash(&snap, "tests2/new.sh").is_none());
    // Opening the path lets the next snapshot carry it.
    w.run_with_stdin(&["sparse-checkout", "add", "--stdin"], "/tests2/new.sh\n").expect("sparse-checkout add");
    let snap = queue::snapshot(&w).expect("snapshot");
    assert!(w.blob_hash(&snap, "tests2/new.sh").is_some());
    assert_eq!(w.blob_hash(&snap, "b/lib.rs").as_deref(), Some(head_b.as_str()));
}

#[test]
fn a_narrower_pattern_set_leaves_dirty_and_untracked_files() {
    let (_repo, _parent, wt) = sparse_fixture(&["/a", "/stories"]);
    let w = Git::new(&wt);
    write_file(&wt, "stories/s.sh", "changed\n");
    write_file(&wt, "stories/wip.sh", "wip\n");
    w.run_with_stdin(&["sparse-checkout", "set", "--no-cone", "--stdin"], "/a\n").expect("narrow");
    assert_eq!(std::fs::read_to_string(wt.join("stories/s.sh")).unwrap(), "changed\n");
    assert_eq!(std::fs::read_to_string(wt.join("stories/wip.sh")).unwrap(), "wip\n");
}
