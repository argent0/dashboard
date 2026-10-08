// Check runner and notes cache (spec.md §6, §9). The player prepares a
// scratch worktree of the hub, runs each check's `cmd` the configured
// number of times, and caches the verdict as a git note on the checked
// commit so rendering and repeated finish-claims never re-run tests for a
// commit already seen.

use crate::config::{self, Check, Claim, Evidence};
use crate::git::Git;
use crate::paths::Home;
use crate::sandbox::Sandbox;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Notes ref holding the check-result cache (spec §9, plan's "Check cache").
const NOTES_REF: &str = "refs/notes/dashboard";
/// `output_tail` is capped at roughly this many bytes of combined output.
const OUTPUT_TAIL_MAX: usize = 4000;

// --------------------------------------------------------------------------
// CheckResult
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub cmd: String,
    pub runs: u32,
    pub min_pass: u32,
    /// Upper bound on passing runs, set only for flaky-test-fix evidence
    /// (the check *must* fail sometimes). Absent from older cached notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pass: Option<u32>,
    /// The check's `retry_failed` / `retry_on` when it ran, so a changed
    /// policy never reuses an old verdict. Absent from older cached notes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retry_failed: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retry_on: Vec<String>,
    /// How many times the check was re-run after a failing attempt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retries: u32,
    /// Output tail of each failing attempt that was retried (the flake
    /// ledger's evidence). `exits`/`runs`/`pass` describe the last attempt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retry_outputs: Vec<String>,
    pub exits: Vec<i32>,
    pub pass: bool,
    pub output_tail: Option<String>,
    /// The JSON object after a final `METRICS ` stdout line of the last run,
    /// if the command printed one (spike claims report measurements this
    /// way; see spec §14). Absent from older cached notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<serde_json::Value>,
    /// The sandbox policy the check ran under (`Sandbox::fingerprint`), so a
    /// result is never reused under a different jail. Empty in older notes,
    /// which therefore always re-run.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub isolation: String,
    /// What a run left behind in the worktree (`git status --porcelain`
    /// lines): a check that does this is impure and fails, whatever its exit
    /// code, and isn't retried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leaked: Vec<String>,
    /// Output lines of a failing run that show it hit the jail's walls (a
    /// read-only filesystem, no network; `Sandbox::walls_hit`): impure like
    /// a leak, since that failure says nothing about the code under test.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl CheckResult {
    /// Passed, but only on a retry: a flake the director should know about.
    pub fn flaky(&self) -> bool {
        self.pass && self.retries > 0
    }

    /// `Some(why)` if a run left files in the worktree or was stopped by the
    /// sandbox: the check failed because it's impure, not because the code
    /// under test is wrong, so a caller that wants it to fail (a post, a
    /// send-back) mustn't count it.
    pub fn impurity(&self) -> Option<String> {
        if !self.leaked.is_empty() {
            return Some(format!(
                "check {:?} is impure: it left files in its worktree ({}). A test may only write \
                 to a temp dir it creates; build outputs belong in .gitignore",
                self.name,
                self.leaked.join(", ")
            ));
        }
        if !self.blocked.is_empty() {
            return Some(format!(
                "check {:?} is impure: the sandbox stopped it ({}). A test may only write to its \
                 worktree and a temp dir it creates, and may not use the network",
                self.name,
                self.blocked.join("; ")
            ));
        }
        None
    }

    /// What made the check impure: the files it left, else the lines
    /// showing it hit the sandbox. Empty for a pure one.
    pub fn impure_evidence(&self) -> Vec<String> {
        if self.leaked.is_empty() {
            self.blocked.clone()
        } else {
            self.leaked.clone()
        }
    }

    /// A cached result is reusable for `check` only if the name, cmd and
    /// threshold all still match — otherwise it's evidence for a different
    /// check that happens to share a name.
    fn reusable_for(&self, check: &Check, max_pass: Option<u32>, isolation: &str) -> bool {
        self.isolation == isolation
            && self.name == check.name
            && self.cmd == check.cmd
            && self.runs == check.runs
            && self.min_pass == check.min_pass
            && self.max_pass == max_pass
            && self.retry_failed == check.retry_failed
            && self.retry_on == check.retry_on
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
        Self {
            pass: false,
            checks: Vec::new(),
            error: Some(error.into()),
        }
    }

    /// The first impure check's `CheckResult::impurity`, if any.
    pub fn impurity(&self) -> Option<String> {
        self.checks.iter().find_map(CheckResult::impurity)
    }

    /// Short human text for rejection messages and relays to a worker, e.g.
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
            if let Some(why) = c.impurity() {
                lines.push(format!("suite failed: {why}"));
                continue;
            }
            let passed = c.exits.iter().filter(|&&e| e == 0).count();
            let mut line = format!(
                "suite failed: {} ({}/{} runs passed)",
                c.name, passed, c.runs
            );
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
    sandbox: Sandbox,
}

enum WorktreeState {
    Absent,
    Broken,
    Valid,
}

impl Runner {
    pub fn new(home: &Home) -> Self {
        Self {
            hub: Git::new(home.hub()),
            work: home.work(),
            sandbox: Sandbox::off(),
        }
    }

    /// Runs every check from now on under `sandbox` (off by default). The
    /// hub stays readable inside it: the worktree's `.git` points there.
    pub fn set_sandbox(&mut self, sandbox: Sandbox) {
        self.sandbox = sandbox.with_readable(&self.hub.dir);
    }

    pub fn sandbox(&self) -> &Sandbox {
        &self.sandbox
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
        let listing = self
            .hub
            .run(&["worktree", "list", "--porcelain"])
            .unwrap_or_default();
        for block in listing.split("\n\n") {
            let mut path_matches = false;
            let mut prunable = false;
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path_matches = std::fs::canonicalize(p)
                        .map(|c| c == target)
                        .unwrap_or(false);
                } else if line.starts_with("prunable") {
                    prunable = true;
                }
            }
            if path_matches {
                return if prunable {
                    WorktreeState::Broken
                } else {
                    WorktreeState::Valid
                };
            }
        }
        // Has a `.git` file but isn't registered with the hub at all.
        WorktreeState::Broken
    }

    fn add_worktree(&self, commit: &str) -> Result<(), String> {
        if let Some(parent) = self.work.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let work_str = self
            .work
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", self.work.display()))?;
        self.hub
            .run(&["worktree", "add", "--detach", work_str, commit])
            .map_err(|e| format!("worktree add: {e}"))
            .map(|_| ())
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
                    std::fs::remove_dir_all(&self.work)
                        .map_err(|e| format!("removing stale worktree dir: {e}"))?;
                }
                self.add_worktree(commit)
            }
            WorktreeState::Valid => {
                let work_git = Git::new(&self.work);
                work_git
                    .run(&["checkout", "--detach", "--force", commit])
                    .map_err(|e| format!("checkout in worktree: {e}"))?;
                // Deliberately `-ffd`, not `-ffdx`: ignored build caches (a
                // Rust `target/`, `node_modules/`, etc.) survive between
                // check runs. The worktree is player-only — no agent commit
                // ever lands here — so there's no risk of an agent planting
                // ignored files for a later check run to trust.
                work_git
                    .run(&["clean", "-ffd"])
                    .map_err(|e| format!("cleaning worktree: {e}"))?;
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
        self.hub
            .notes_add(NOTES_REF, commit, &json)
            .map_err(|e| e.to_string())
    }

    // ---- running checks ----

    /// Runs `check` at `commit`, using the notes cache first. Returns
    /// without preparing the worktree at all on a cache hit.
    pub fn run_check(&self, commit: &str, check: &Check) -> CheckResult {
        self.run_check_timed(commit, check, None, None)
    }

    /// Runs a flaky-test-fix's evidence at `commit`: every run executes (no
    /// early stop, so the cached result and its k/N are exact), and the
    /// verdict is `pass` when between `min_pass` and `max_pass` runs passed,
    /// i.e. the flake reproduced.
    pub fn run_evidence(&self, commit: &str, evidence: &Evidence) -> CheckResult {
        self.run_check_timed(commit, &evidence.as_check(), None, Some(evidence.max_pass))
    }

    /// Runs `cmd` at `commit` and returns its stdout as trimmed, non-empty
    /// lines: the project's test inventory (`tests.toml`'s `list_cmd`).
    pub fn run_list(&self, commit: &str, cmd: &str) -> Result<Vec<String>, String> {
        self.prepare(commit)?;
        let (code, stdout, combined) = run_command(&self.sandbox, cmd, &self.work, None);
        if code != 0 {
            return Err(format!("list_cmd exited {code}: {}", tail_str(&combined)));
        }
        Ok(String::from_utf8_lossy(&stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// Resolves the conflicts of merging `theirs` into `ours` when every
    /// conflicted path is a key of `regen` (`[player] lock_regen`): in the
    /// scratch worktree it merges, takes our side of each such file, runs its
    /// command in the jail and stages the result. Returns the merged tree for
    /// the caller's merge commit (the suite then judges it), or why it
    /// couldn't. Only the named files are taken from the worktree.
    pub fn regen_locks(
        &self,
        ours: &str,
        theirs: &str,
        regen: &std::collections::BTreeMap<String, String>,
    ) -> Result<String, String> {
        self.prepare(ours)?;
        let work = Git::new(&self.work);
        // Exit 1 is the conflict we are here to resolve.
        let _ = work.run(&["merge", "--no-commit", "--no-ff", theirs]);
        let result = (|| {
            let unmerged = work
                .run(&["diff", "--name-only", "--diff-filter=U"])
                .map_err(|e| e.to_string())?;
            let unmerged: Vec<&str> = unmerged.lines().filter(|l| !l.is_empty()).collect();
            if let Some(p) = unmerged.iter().find(|p| !regen.contains_key(**p)) {
                return Err(format!("{p} is not a lock_regen file"));
            }
            for path in &unmerged {
                work.run(&["checkout", "--ours", "--", path])
                    .map_err(|e| e.to_string())?;
                let (code, _, out) = run_command(&self.sandbox, &regen[*path], &self.work, None);
                if code != 0 {
                    return Err(format!(
                        "lock_regen for {path} exited {code}: {}",
                        tail_str(&out)
                    ));
                }
                work.run(&["add", "--", path]).map_err(|e| e.to_string())?;
            }
            work.run(&["write-tree"]).map_err(|e| e.to_string())
        })();
        let _ = work.run(&["merge", "--abort"]);
        let _ = work.run(&["reset", "--hard", "-q"]);
        result
    }

    /// Runs a spike claim at `commit` (cached like any check, under the
    /// claim's prefixed name), killing any run that exceeds its timeout.
    pub fn run_claim(&self, commit: &str, claim: &Claim) -> CheckResult {
        self.run_check_timed(
            commit,
            &claim.as_check(),
            Some(Duration::from_secs(claim.timeout_secs())),
            None,
        )
    }

    fn run_check_timed(
        &self,
        commit: &str,
        check: &Check,
        timeout: Option<Duration>,
        max_pass: Option<u32>,
    ) -> CheckResult {
        if let Some(cached) = self
            .read_note(commit)
            .checks
            .get(&check.name)
            .filter(|r| r.reusable_for(check, max_pass, &self.sandbox.fingerprint()))
            .cloned()
        {
            return cached;
        }

        let result = self.execute(commit, check, timeout, max_pass);
        // Only cache a result backed by at least one real run — a worktree
        // that couldn't even be prepared is an infra problem, not evidence,
        // and shouldn't be remembered as a verdict on `commit`.
        if !result.exits.is_empty() {
            let _ = self.write_result(commit, &result);
        }
        result
    }

    /// Prepares the worktree and runs the check: one attempt, then up to
    /// `retry_failed` more after a failing verdict (only when the failure's
    /// output matches `retry_on`, if set). The check passes if any attempt
    /// does; the result describes the last attempt plus what the retries saw.
    fn execute(
        &self,
        commit: &str,
        check: &Check,
        timeout: Option<Duration>,
        max_pass: Option<u32>,
    ) -> CheckResult {
        let base = CheckResult {
            name: check.name.clone(),
            cmd: check.cmd.clone(),
            runs: check.runs,
            min_pass: check.min_pass,
            max_pass,
            retry_failed: check.retry_failed,
            retry_on: check.retry_on.clone(),
            retries: 0,
            retry_outputs: Vec::new(),
            exits: Vec::new(),
            pass: false,
            output_tail: None,
            metrics: None,
            isolation: self.sandbox.fingerprint(),
            leaked: Vec::new(),
            blocked: Vec::new(),
        };
        if let Err(e) = self.prepare(commit) {
            return CheckResult {
                output_tail: Some(format!("worktree setup failed: {e}")),
                ..base
            };
        }

        let patterns: Vec<regex::Regex> = check
            .retry_on
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();
        let mut retries = 0;
        let mut retry_outputs = Vec::new();
        loop {
            let (mut result, failed_output) = self.attempt(check, timeout, max_pass, base.clone());
            let retry = !result.pass
                && result.impurity().is_none()
                && retries < check.retry_failed
                && (patterns.is_empty() || patterns.iter().any(|p| p.is_match(&failed_output)));
            if !retry {
                result.retries = retries;
                result.retry_outputs = retry_outputs;
                return result;
            }
            retries += 1;
            retry_outputs.push(result.output_tail.unwrap_or_default());
        }
    }

    /// One attempt: runs `check.cmd` up to `check.runs` times, stopping early
    /// once the verdict is decided. Also returns the full output of every
    /// failing run, which `retry_on` is matched against.
    fn attempt(
        &self,
        check: &Check,
        timeout: Option<Duration>,
        max_pass: Option<u32>,
        mut result: CheckResult,
    ) -> (CheckResult, String) {
        let mut exits = Vec::new();
        let mut passes: u32 = 0;
        let mut failed_output = String::new();

        for i in 0..check.runs {
            let remaining_after_this = check.runs - i - 1;

            let (code, stdout, combined) =
                run_command(&self.sandbox, &check.cmd, &self.work, timeout);
            result.metrics = parse_metrics(&stdout);

            // Impure: the run changed the worktree. Every later run would
            // see that state, so stop here, failing whatever the exit code.
            let leaked = self.leaks();
            if !leaked.is_empty() {
                exits.push(code);
                result.leaked = leaked;
                result.output_tail = Some(tail_str(&combined));
                result.pass = false;
                result.runs = exits.len() as u32;
                result.exits = exits;
                return (result, failed_output);
            }

            // Failed against the jail's walls: the same in every run.
            if code != 0 {
                let blocked = self.sandbox.walls_hit(&String::from_utf8_lossy(&combined));
                if !blocked.is_empty() {
                    exits.push(code);
                    result.blocked = blocked;
                    result.output_tail = Some(tail_str(&combined));
                    result.pass = false;
                    result.runs = exits.len() as u32;
                    result.exits = exits;
                    return (result, failed_output);
                }
            }

            exits.push(code);
            if code == 0 {
                passes += 1;
            } else {
                if result.output_tail.is_none() {
                    result.output_tail = Some(tail_str(&combined));
                }
                failed_output.push_str(&String::from_utf8_lossy(&combined));
            }

            // Stop early once the verdict can't change: enough passes
            // already, or not enough runs left to ever reach min_pass.
            // Bounded (evidence) checks always run in full.
            if max_pass.is_none()
                && (passes >= check.min_pass || passes + remaining_after_this < check.min_pass)
            {
                break;
            }
        }

        result.pass = passes >= check.min_pass && max_pass.is_none_or(|m| passes <= m);
        result.runs = exits.len() as u32;
        result.exits = exits;
        (result, failed_output)
    }

    /// What a run left in the worktree, as `git status --porcelain` lines:
    /// changed tracked files and new untracked ones. Ignored files (build
    /// caches) don't count; `prepare` keeps those between runs anyway.
    fn leaks(&self) -> Vec<String> {
        match Git::new(&self.work).run(&["status", "--porcelain", "--untracked-files=all"]) {
            Ok(out) => out
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            Err(e) => vec![format!("(git status failed: {e})")],
        }
    }

    /// Reads the suite at `commit` (`tests.toml` plus every `tests.d/*.toml`)
    /// and runs every `[[check]]`. A missing or invalid suite is reported via
    /// `error`, not a panic. An empty check list passes.
    pub fn run_suite(&self, commit: &str) -> SuiteResult {
        let cfg = match config::suite_at(&self.hub, commit) {
            Ok(c) => c,
            Err(e) => return SuiteResult::failing(e),
        };

        let mut checks = Vec::with_capacity(cfg.check.len());
        let mut pass = true;
        for check in &cfg.check {
            let result = self.run_check(commit, check);
            pass &= result.pass;
            checks.push(result);
        }
        SuiteResult {
            pass,
            checks,
            error: None,
        }
    }

    /// Reads `tasks/<slug>/task.toml` at `commit` and runs its `[check]`.
    /// Errors (not a `CheckResult`) if the file is missing, invalid, or has
    /// no check — there's nothing to run.
    pub fn run_task_check(&self, commit: &str, slug: &str) -> Result<CheckResult, String> {
        let path = format!("tasks/{slug}/task.toml");
        let text = self
            .hub
            .show_file(commit, &path)
            .ok_or_else(|| format!("{path} not found at {commit}"))?;
        let cfg = config::task_config_from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        let check = cfg
            .check
            .ok_or_else(|| format!("{path}: no [check] section"))?;
        // A task check that fails must keep meaning "fails for the reason
        // the task exists" (spec §6.3): no retries here.
        Ok(self.run_check(commit, &check.without_retries()))
    }
}

/// Runs `sh -c <cmd>` in `cwd` (jailed by `sandbox`), returning `(exit code, stdout, stdout +
/// stderr)`. The exit code is -1 for a signal, a spawn failure, or a timeout.
/// With a timeout the command runs in its own process group so the whole tree
/// can be killed when the deadline passes.
fn run_command(
    sandbox: &Sandbox,
    cmd: &str,
    cwd: &std::path::Path,
    timeout: Option<Duration>,
) -> (i32, Vec<u8>, Vec<u8>) {
    let Some(timeout) = timeout else {
        return match sandbox.command(cmd, cwd).output() {
            Ok(out) => {
                let mut combined = out.stdout.clone();
                combined.extend_from_slice(&out.stderr);
                // `ExitStatus::code()` is `None` when the process was killed
                // by a signal; record that case as -1.
                (out.status.code().unwrap_or(-1), out.stdout, combined)
            }
            Err(e) => (-1, Vec::new(), e.to_string().into_bytes()),
        };
    };

    let spawned = sandbox
        .command(cmd, cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return (-1, Vec::new(), e.to_string().into_bytes()),
    };
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                // A negative pid addresses the whole process group.
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .output();
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };

    let stdout = out_reader.join().unwrap_or_default();
    let mut combined = stdout.clone();
    combined.extend_from_slice(&err_reader.join().unwrap_or_default());
    if timed_out {
        combined.extend_from_slice(
            format!("\n[killed: timed out after {}s]", timeout.as_secs()).as_bytes(),
        );
        return (-1, stdout, combined);
    }
    (
        status.and_then(|s| s.code()).unwrap_or(-1),
        stdout,
        combined,
    )
}

/// The JSON object on the last non-empty stdout line, if that line is
/// `METRICS <json-object>`.
fn parse_metrics(stdout: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(stdout);
    let last = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let json = last.trim().strip_prefix("METRICS ")?;
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .filter(|v| v.is_object())
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
        let check = Check {
            name: "unit".into(),
            cmd: "true".into(),
            runs: 3,
            min_pass: 2,
            retry_failed: 0,
            retry_on: vec![],
        };
        let result = CheckResult {
            name: "unit".into(),
            cmd: "true".into(),
            runs: 3,
            min_pass: 2,
            max_pass: None,
            retry_failed: 0,
            retry_on: vec![],
            retries: 0,
            retry_outputs: vec![],
            exits: vec![0, 0, 0],
            pass: true,
            output_tail: None,
            metrics: None,
            ..Default::default()
        };
        assert!(result.reusable_for(&check, None, ""));
        assert!(!result.reusable_for(&check, Some(2), ""));
        assert!(!result.reusable_for(&check, None, "v1 jail=off"));

        let mut other_cmd = check.clone();
        other_cmd.cmd = "false".into();
        assert!(!result.reusable_for(&other_cmd, None, ""));

        let mut other_threshold = check.clone();
        other_threshold.min_pass = 3;
        assert!(!result.reusable_for(&other_threshold, None, ""));
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
                max_pass: None,
                exits: vec![1],
                pass: false,
                output_tail: Some("boom".into()),
                ..Default::default()
            }],
            error: None,
        };
        let summary = suite.summary();
        assert!(
            summary.contains("suite failed: unit (0/1 runs passed)"),
            "unexpected summary: {summary}"
        );
        assert!(summary.contains("boom"), "unexpected summary: {summary}");
    }

    #[test]
    fn parse_metrics_reads_a_final_metrics_line() {
        let out = b"warming up\nMETRICS {\"throughput\": 2.1}\n\n";
        assert_eq!(
            parse_metrics(out),
            Some(serde_json::json!({"throughput": 2.1}))
        );
        assert_eq!(
            parse_metrics(b"METRICS {\"a\": 1}\nthen more output\n"),
            None,
            "must be the last line"
        );
        assert_eq!(
            parse_metrics(b"METRICS [1, 2]\n"),
            None,
            "must be an object"
        );
        assert_eq!(parse_metrics(b"METRICS nope\n"), None);
        assert_eq!(parse_metrics(b""), None);
    }

    #[test]
    fn run_command_kills_a_run_past_its_timeout() {
        let dir = std::env::temp_dir();
        let (code, _, combined) = run_command(
            &Sandbox::off(),
            "sleep 30",
            &dir,
            Some(Duration::from_secs(1)),
        );
        assert_eq!(code, -1);
        assert!(String::from_utf8_lossy(&combined).contains("timed out after 1s"));
        let (code, stdout, _) = run_command(
            &Sandbox::off(),
            "echo hi",
            &dir,
            Some(Duration::from_secs(5)),
        );
        assert_eq!(code, 0);
        assert_eq!(String::from_utf8_lossy(&stdout).trim(), "hi");
    }

    #[test]
    fn suite_summary_error_takes_precedence() {
        let suite = SuiteResult::failing("tests.toml not found");
        assert_eq!(suite.summary(), "suite failed: tests.toml not found");
    }

    #[test]
    fn suite_summary_pass() {
        let suite = SuiteResult {
            pass: true,
            checks: Vec::new(),
            error: None,
        };
        assert_eq!(suite.summary(), "suite passed");
    }
}
