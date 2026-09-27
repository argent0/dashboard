// Check runner and notes cache (spec.md §6, §9). The player prepares a
// scratch worktree of the hub, runs each check's `cmd` the configured
// number of times, and caches the verdict as a git note on the checked
// commit so rendering and repeated finish-claims never re-run tests for a
// commit already seen.

use crate::config::{self, Check};
use crate::git::Git;
use crate::paths::Home;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

/// Notes ref holding the check-result cache (spec §9, plan's "Check cache").
const NOTES_REF: &str = "refs/notes/dashboard";
/// `output_tail` is capped at roughly this many bytes of combined output.
const OUTPUT_TAIL_MAX: usize = 4000;

// --------------------------------------------------------------------------
// CheckResult
// --------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub cmd: String,
    pub runs: u32,
    pub min_pass: u32,
    pub exits: Vec<i32>,
    pub pass: bool,
    pub output_tail: Option<String>,
}

impl CheckResult {
    /// A cached result is reusable for `check` only if the name, cmd and
    /// threshold all still match — otherwise it's evidence for a different
    /// check that happens to share a name.
    fn reusable_for(&self, check: &Check) -> bool {
        self.name == check.name && self.cmd == check.cmd && self.runs == check.runs && self.min_pass == check.min_pass
    }
}

/// The JSON payload of a `refs/notes/dashboard` note: `{"checks": {name: CheckResult}}`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Note {
    #[serde(default)]
    checks: BTreeMap<String, CheckResult>,
}

// --------------------------------------------------------------------------
// SuiteResult
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    pub pass: bool,
    pub checks: Vec<CheckResult>,
    pub error: Option<String>,
}

impl SuiteResult {
    /// A suite that couldn't even be evaluated (missing/invalid `tests.toml`).
    pub fn failing(error: impl Into<String>) -> Self {
        Self { pass: false, checks: Vec::new(), error: Some(error.into()) }
    }

    /// Short human text for rejection messages and relays to a subagent, e.g.
    /// `suite failed: unit (0/1 runs passed)\n<output tail>`.
    pub fn summary(&self) -> String {
        if let Some(err) = &self.error {
            return format!("suite failed: {err}");
        }
        if self.pass {
            return "suite passed".to_string();
        }
        let mut lines = Vec::new();
        for c in self.checks.iter().filter(|c| !c.pass) {
            let passed = c.exits.iter().filter(|&&e| e == 0).count();
            let mut line = format!("suite failed: {} ({}/{} runs passed)", c.name, passed, c.runs);
            if let Some(tail) = &c.output_tail {
                line.push('\n');
                line.push_str(tail);
            }
            lines.push(line);
        }
        lines.join("\n")
    }
}

// --------------------------------------------------------------------------
// Runner
// --------------------------------------------------------------------------

pub struct Runner {
    hub: Git,
    work: PathBuf,
}

enum WorktreeState {
    Absent,
    Broken,
    Valid,
}

impl Runner {
    pub fn new(home: &Home) -> Self {
        Self { hub: Git::new(home.hub()), work: home.work() }
    }

    // ---- worktree management ----

    /// Classifies `self.work` relative to the hub's own worktree registry.
    fn worktree_state(&self) -> WorktreeState {
        if !self.work.exists() {
            return WorktreeState::Absent;
        }
        // A real worktree has a `.git` *file* (not a directory) pointing at
        // the admin dir under the hub's `worktrees/`.
        if !self.work.join(".git").is_file() {
            return WorktreeState::Broken;
        }
        let target = std::fs::canonicalize(&self.work).unwrap_or_else(|_| self.work.clone());
        let listing = self.hub.run(&["worktree", "list", "--porcelain"]).unwrap_or_default();
        for block in listing.split("\n\n") {
            let mut path_matches = false;
            let mut prunable = false;
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path_matches = std::fs::canonicalize(p).map(|c| c == target).unwrap_or(false);
                } else if line.starts_with("prunable") {
                    prunable = true;
                }
            }
            if path_matches {
                return if prunable { WorktreeState::Broken } else { WorktreeState::Valid };
            }
        }
        // Has a `.git` file but isn't registered with the hub at all.
        WorktreeState::Broken
    }

    fn add_worktree(&self, commit: &str) -> Result<(), String> {
        if let Some(parent) = self.work.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let work_str = self.work.to_str().ok_or_else(|| format!("{} is not valid UTF-8", self.work.display()))?;
        self.hub.run(&["worktree", "add", "--detach", work_str, commit]).map_err(|e| format!("worktree add: {e}")).map(|_| ())
    }

    /// Checks the worktree out at `commit`, creating or recreating it as
    /// needed (see the three cases in `WorktreeState`).
    fn prepare(&self, commit: &str) -> Result<(), String> {
        match self.worktree_state() {
            WorktreeState::Absent => self.add_worktree(commit),
            WorktreeState::Broken => {
                // Ignore errors: pruning is best-effort cleanup of the hub's
                // registry, not a precondition for what follows.
                let _ = self.hub.run(&["worktree", "prune"]);
                if self.work.exists() {
                    std::fs::remove_dir_all(&self.work).map_err(|e| format!("removing stale worktree dir: {e}"))?;
                }
                self.add_worktree(commit)
            }
            WorktreeState::Valid => {
                let work_git = Git::new(&self.work);
                work_git.run(&["checkout", "--detach", "--force", commit]).map_err(|e| format!("checkout in worktree: {e}"))?;
                // Deliberately `-ffd`, not `-ffdx`: ignored build caches (a
                // Rust `target/`, `node_modules/`, etc.) survive between
                // check runs. The worktree is player-only — no agent commit
                // ever lands here — so there's no risk of an agent planting
                // ignored files for a later check run to trust.
                work_git.run(&["clean", "-ffd"]).map_err(|e| format!("cleaning worktree: {e}"))?;
                Ok(())
            }
        }
    }

    // ---- notes cache ----

    fn read_note(&self, commit: &str) -> Note {
        match self.hub.notes_show(NOTES_REF, commit) {
            Some(text) => serde_json::from_str(&text).unwrap_or_default(),
            None => Note::default(),
        }
    }

    fn write_result(&self, commit: &str, result: &CheckResult) -> Result<(), String> {
        let mut note = self.read_note(commit);
        note.checks.insert(result.name.clone(), result.clone());
        let json = serde_json::to_string(&note).map_err(|e| e.to_string())?;
        self.hub.notes_add(NOTES_REF, commit, &json).map_err(|e| e.to_string())
    }

    // ---- running checks ----

    /// Runs `check` at `commit`, using the notes cache first. Returns
    /// without preparing the worktree at all on a cache hit.
    pub fn run_check(&self, commit: &str, check: &Check) -> CheckResult {
        if let Some(cached) = self.read_note(commit).checks.get(&check.name).filter(|r| r.reusable_for(check)).cloned() {
            return cached;
        }

        let result = self.execute(commit, check);
        // Only cache a result backed by at least one real run — a worktree
        // that couldn't even be prepared is an infra problem, not evidence,
        // and shouldn't be remembered as a verdict on `commit`.
        if !result.exits.is_empty() {
            let _ = self.write_result(commit, &result);
        }
        result
    }

    /// Prepares the worktree and actually runs `check.cmd` up to `check.runs`
    /// times, stopping early once the verdict is decided.
    fn execute(&self, commit: &str, check: &Check) -> CheckResult {
        if let Err(e) = self.prepare(commit) {
            return CheckResult {
                name: check.name.clone(),
                cmd: check.cmd.clone(),
                runs: check.runs,
                min_pass: check.min_pass,
                exits: Vec::new(),
                pass: false,
                output_tail: Some(format!("worktree setup failed: {e}")),
            };
        }

        let mut exits = Vec::new();
        let mut passes: u32 = 0;
        let mut output_tail: Option<String> = None;

        for i in 0..check.runs {
            let remaining_after_this = check.runs - i - 1;

            let (code, combined) = match Command::new("sh").arg("-c").arg(&check.cmd).current_dir(&self.work).output() {
                Ok(out) => {
                    let mut bytes = out.stdout;
                    bytes.extend_from_slice(&out.stderr);
                    // `ExitStatus::code()` is `None` when the process was
                    // killed by a signal; record that case as -1.
                    (out.status.code().unwrap_or(-1), bytes)
                }
                Err(e) => (-1, e.to_string().into_bytes()),
            };

            exits.push(code);
            if code == 0 {
                passes += 1;
            } else if output_tail.is_none() {
                output_tail = Some(tail_str(&combined));
            }

            // Stop early once the verdict can't change: enough passes
            // already, or not enough runs left to ever reach min_pass.
            if passes >= check.min_pass || passes + remaining_after_this < check.min_pass {
                break;
            }
        }

        let pass = passes >= check.min_pass;
        CheckResult { name: check.name.clone(), cmd: check.cmd.clone(), runs: exits.len() as u32, min_pass: check.min_pass, exits, pass, output_tail }
    }

    /// Reads `tests.toml` at `commit` and runs every `[[check]]`. Missing or
    /// invalid `tests.toml` is reported via `error`, not a panic. An empty
    /// check list passes.
    pub fn run_suite(&self, commit: &str) -> SuiteResult {
        let text = match self.hub.show_file(commit, "tests.toml") {
            Some(t) => t,
            None => return SuiteResult::failing(format!("tests.toml not found at {commit}")),
        };
        let cfg = match config::tests_config_from_str(&text) {
            Ok(c) => c,
            Err(e) => return SuiteResult::failing(format!("tests.toml: {e}")),
        };

        let mut checks = Vec::with_capacity(cfg.check.len());
        let mut pass = true;
        for check in &cfg.check {
            let result = self.run_check(commit, check);
            pass &= result.pass;
            checks.push(result);
        }
        SuiteResult { pass, checks, error: None }
    }

    /// Reads `tasks/<slug>/task.toml` at `commit` and runs its `[check]`.
    /// Errors (not a `CheckResult`) if the file is missing, invalid, or has
    /// no check — there's nothing to run.
    pub fn run_task_check(&self, commit: &str, slug: &str) -> Result<CheckResult, String> {
        let path = format!("tasks/{slug}/task.toml");
        let text = self.hub.show_file(commit, &path).ok_or_else(|| format!("{path} not found at {commit}"))?;
        let cfg = config::task_config_from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        let check = cfg.check.ok_or_else(|| format!("{path}: no [check] section"))?;
        Ok(self.run_check(commit, &check))
    }
}

/// Last ~`OUTPUT_TAIL_MAX` bytes of `data`, lossily decoded as UTF-8.
fn tail_str(data: &[u8]) -> String {
    let start = data.len().saturating_sub(OUTPUT_TAIL_MAX);
    String::from_utf8_lossy(&data[start..]).into_owned()
}

// --------------------------------------------------------------------------
// Read-only accessor for the renderer — never runs anything.
// --------------------------------------------------------------------------

/// Every cached check result on `commit`, straight from the notes cache.
pub fn cached(hub: &Git, commit: &str) -> Vec<CheckResult> {
    match hub.notes_show(NOTES_REF, commit) {
        Some(text) => match serde_json::from_str::<Note>(&text) {
            Ok(note) => note.checks.into_values().collect(),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
// Behavior that needs real git/worktrees/notes is covered by the
// integration tests in tests/checks.rs; these unit tests only cover the
// pure helpers.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_str_keeps_only_the_end() {
        let data = vec![b'a'; OUTPUT_TAIL_MAX + 100];
        let tail = tail_str(&data);
        assert_eq!(tail.len(), OUTPUT_TAIL_MAX);
    }

    #[test]
    fn tail_str_keeps_everything_when_short() {
        assert_eq!(tail_str(b"hello"), "hello");
    }

    #[test]
    fn reusable_for_checks_name_cmd_and_threshold() {
        let check = Check { name: "unit".into(), cmd: "true".into(), runs: 3, min_pass: 2 };
        let result = CheckResult { name: "unit".into(), cmd: "true".into(), runs: 3, min_pass: 2, exits: vec![0, 0, 0], pass: true, output_tail: None };
        assert!(result.reusable_for(&check));

        let mut other_cmd = check.clone();
        other_cmd.cmd = "false".into();
        assert!(!result.reusable_for(&other_cmd));

        let mut other_threshold = check.clone();
        other_threshold.min_pass = 3;
        assert!(!result.reusable_for(&other_threshold));
    }

    #[test]
    fn suite_summary_failing_check_includes_counts_and_tail() {
        let suite = SuiteResult {
            pass: false,
            checks: vec![CheckResult {
                name: "unit".into(),
                cmd: "false".into(),
                runs: 1,
                min_pass: 1,
                exits: vec![1],
                pass: false,
                output_tail: Some("boom".into()),
            }],
            error: None,
        };
        let summary = suite.summary();
        assert!(summary.contains("suite failed: unit (0/1 runs passed)"), "unexpected summary: {summary}");
        assert!(summary.contains("boom"), "unexpected summary: {summary}");
    }

    #[test]
    fn suite_summary_error_takes_precedence() {
        let suite = SuiteResult::failing("tests.toml not found");
        assert_eq!(suite.summary(), "suite failed: tests.toml not found");
    }

    #[test]
    fn suite_summary_pass() {
        let suite = SuiteResult { pass: true, checks: Vec::new(), error: None };
        assert_eq!(suite.summary(), "suite passed");
    }
}
