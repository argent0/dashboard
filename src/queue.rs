// The test queue (`[player] runs = "queued"`): agents don't run the suite
// themselves, they ask the player with `dashboard test`, which snapshots the
// worktree, files a request in the home's `queue/` and waits for the result
// file the player writes once it has run the suite (one request per tick, so
// runs never overlap). A queued run is never judged: no strike, no event, and
// nothing in the task's history.

use crate::checks::{CheckResult, SuiteResult};
use crate::git::Git;
use crate::paths::Home;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub slug: String,
    /// The snapshot commit to run the suite on.
    pub commit: String,
    /// The repository holding `refs/dashboard/test/<id>`, to fetch from.
    pub repo: PathBuf,
}

impl Request {
    pub fn ref_name(&self) -> String {
        format!("refs/dashboard/test/{}", self.id)
    }
}

fn dir(home: &Home) -> PathBuf {
    home.queue()
}

fn request_path(home: &Home, id: &str) -> PathBuf {
    dir(home).join(format!("{id}.req"))
}

/// The file the player writes the result to: `PASS` or `FAIL` on the first
/// line, then the report.
pub fn result_path(home: &Home, id: &str) -> PathBuf {
    dir(home).join(format!("{id}.result"))
}

/// The task a worktree belongs to, from its branch (`task/<slug>` for a
/// worker, `agent/<slug>` for a task agent).
pub fn slug_of_branch(branch: &str) -> Option<&str> {
    branch
        .strip_prefix("task/")
        .or_else(|| branch.strip_prefix("agent/"))
        .filter(|s| !s.is_empty())
}

/// A commit holding the worktree's current content (tracked and untracked,
/// not ignored) on top of `HEAD`, built in a private index so the branch,
/// the index and the working tree are untouched. Deterministic: the same
/// content gives the same commit, so the check cache can answer a re-run.
pub fn snapshot(worktree: &Git) -> Result<String, String> {
    let git_dir = worktree
        .run(&["rev-parse", "--absolute-git-dir"])
        .map_err(|e| e.to_string())?;
    let index = Path::new(&git_dir).join("dashboard-test-index");
    let index_str = index.to_str().ok_or("non-UTF-8 git dir")?;
    let _ = std::fs::remove_file(&index);
    let env = [("GIT_INDEX_FILE", index_str)];
    let built = (|| {
        worktree.run_with_env(&["read-tree", "HEAD"], &env)?;
        // In a sparse tree (spec §21) an untracked file outside the patterns
        // is skipped and makes `add -A` exit 1 after staging the rest: the
        // snapshot is the tree as the worker's next commit would hold it.
        if let Err(e) = worktree.run_with_env(&["-c", "advice.updateSparsePath=false", "add", "-A"], &env) {
            if !e.stderr.contains("outside of your sparse-checkout definition") {
                return Err(e);
            }
        }
        let tree = worktree.run_with_env(&["write-tree"], &env)?;
        worktree.run_with_env(
            &[
                "commit-tree",
                &tree,
                "-p",
                "HEAD",
                "-m",
                "dashboard test snapshot",
            ],
            &[
                ("GIT_AUTHOR_NAME", "dashboard"),
                ("GIT_AUTHOR_EMAIL", "dashboard@localhost"),
                ("GIT_COMMITTER_NAME", "dashboard"),
                ("GIT_COMMITTER_EMAIL", "dashboard@localhost"),
                ("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z"),
                ("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z"),
            ],
        )
    })();
    let _ = std::fs::remove_file(&index);
    built.map_err(|e| format!("snapshotting the worktree: {e}"))
}

/// Files a request for `commit` (reachable from `refs/dashboard/test/<id>`
/// in `worktree`'s repository). Written atomically; returns it.
pub fn enqueue(
    home: &Home,
    worktree: &Git,
    slug: &str,
    commit: &str,
    now_millis: u128,
) -> Result<Request, String> {
    let common = worktree
        .run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map_err(|e| e.to_string())?;
    let req = Request {
        id: format!("{now_millis:016}-{slug}"),
        slug: slug.to_string(),
        commit: commit.to_string(),
        repo: PathBuf::from(common),
    };
    worktree
        .update_ref(&req.ref_name(), commit)
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir(home)).map_err(|e| format!("{}: {e}", dir(home).display()))?;
    write_atomic(
        &request_path(home, &req.id),
        &serde_json::to_string(&req).map_err(|e| e.to_string())?,
    )?;
    Ok(req)
}

/// The oldest request still waiting for a result, if any.
pub fn next(home: &Home) -> Option<Request> {
    let mut ids: Vec<String> = std::fs::read_dir(dir(home))
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".req").map(str::to_string))
        .collect();
    ids.sort();
    ids.into_iter().find_map(|id| {
        if result_path(home, &id).exists() {
            return None;
        }
        let text = std::fs::read_to_string(request_path(home, &id)).ok()?;
        serde_json::from_str(&text).ok()
    })
}

/// Records the verdict and report for `req` and retires the request.
pub fn finish(home: &Home, req: &Request, pass: bool, report: &str) -> Result<(), String> {
    let head = if pass { "PASS" } else { "FAIL" };
    write_atomic(&result_path(home, &req.id), &format!("{head}\n{report}"))?;
    let _ = std::fs::remove_file(request_path(home, &req.id));
    Ok(())
}

/// Waits for `id`'s result: `(pass, report)`. `None` on timeout.
pub fn wait(home: &Home, id: &str, timeout: Option<Duration>) -> Option<(bool, String)> {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(result_path(home, id)) {
            let (head, rest) = text.split_once('\n').unwrap_or((&text, ""));
            return Some((head == "PASS", rest.to_string()));
        }
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// What the agent reads: every check's verdict, command and output tail.
pub fn render(suite: &SuiteResult) -> String {
    if let Some(e) = &suite.error {
        return format!("suite could not run: {e}\n");
    }
    let mut out = String::new();
    for c in &suite.checks {
        out.push_str(&render_check(c));
    }
    out.push_str(if suite.pass {
        "\nsuite passed\n"
    } else {
        "\nsuite FAILED\n"
    });
    out
}

fn render_check(c: &CheckResult) -> String {
    let mut s = format!(
        "{} {}  ({}; {} ms)\n",
        if c.pass { "ok  " } else { "FAIL" },
        c.name,
        c.cmd,
        c.millis
    );
    if let Some(why) = c.impurity() {
        s.push_str(&format!("  {why}\n"));
    }
    if c.timed_out {
        s.push_str(&format!("  killed past the {}s check timeout\n", c.timeout));
    }
    if !c.pass {
        if let Some(tail) = &c.output_tail {
            for line in tail.lines() {
                s.push_str(&format!("  | {line}\n"));
            }
        }
    }
    s
}
