// roadmap.toml / tests.toml / task.toml schemas (spec.md §6.2, §8.2, §10),
// with serde defaults so every section and field is optional.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::str::FromStr;

// --------------------------------------------------------------------------
// roadmap.toml (spec §8.2, §10)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RoadmapConfig {
    pub title: Option<String>,
    pub phases: Vec<Phase>,
    pub docs_paths: Vec<String>,
    pub limits: Limits,
    pub player: PlayerConfig,
    pub sandbox: SandboxConfig,
    pub budget: BudgetConfig,
}

/// `[budget]`: how long the project suite may take. Like `[sandbox]` it lives
/// in `roadmap.toml`, so a task agent editing the suite can't loosen it. Every
/// field is seconds; 0 (the default) turns that limit off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    /// Kills any single run of a suite check that takes longer (the check
    /// fails, a `budget` strike). Not applied to a task's own `[check]`.
    pub check_timeout: u64,
    /// A suite whose checks took longer in total is reported (`suite_slow`).
    pub suite_warn: u64,
    /// A suite whose checks took longer in total is rejected (`budget`).
    pub suite_fail: u64,
}

impl BudgetConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.suite_warn > 0 && self.suite_fail > 0 && self.suite_warn >= self.suite_fail {
            return Err(format!(
                "[budget] suite_warn ({}) must be below suite_fail ({})",
                self.suite_warn, self.suite_fail
            ));
        }
        Ok(())
    }
}

/// `[sandbox]`: the jail every check runs in (`src/sandbox.rs`). It's in
/// `roadmap.toml`, not `tests.toml`, because task agents may edit the
/// suite but must never be able to loosen the jail it runs in.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// Run checks under bubblewrap. Off runs them as plain `sh -c`.
    pub enabled: bool,
    /// Keep the host network inside the jail (off: loopback only).
    pub network: bool,
    /// Host paths checks may write to besides their worktree and a private
    /// `/tmp`, e.g. a toolchain cache. A leading `~/` is the player's home.
    pub writable: Vec<String>,
}

impl SandboxConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for w in &self.writable {
            if !(w.starts_with('/') || w.starts_with("~/")) {
                return Err(format!(
                    "[sandbox] writable {w:?}: must be an absolute path or start with ~/"
                ));
            }
        }
        Ok(())
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            network: false,
            writable: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Phase {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    /// Left out of `status`'s phase list and milestones (e.g. a finished,
    /// old phase); its tasks still count toward the overall progress.
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_review_rounds: u32,
    /// How many times a worker may contest the tests it must pass
    /// (spec §18); one more escalates.
    pub max_contest_rounds: u32,
    /// How many times `dashboard relaunch` may restart a task's worker, and
    /// separately its task agent, since that session last made progress;
    /// one more escalates.
    pub max_relaunches: u32,
    pub strikes: StrikeLimits,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_review_rounds: 2,
            max_contest_rounds: 2,
            max_relaunches: 2,
            strikes: StrikeLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StrikeLimits {
    pub history: u32,
    pub ownership: u32,
    pub pins: u32,
    pub suite: u32,
    pub plan: u32,
    pub post: u32,
    pub review: u32,
    pub accept: u32,
    pub retire: u32,
    pub claims: u32,
    /// A `flaky-test-fix` post whose evidence doesn't reproduce the flake.
    /// Defaults to 1: the player retires the task on the first miss.
    pub evidence: u32,
    /// A suite that timed out or exceeded `[budget] suite_fail`.
    pub budget: u32,
}

impl Default for StrikeLimits {
    fn default() -> Self {
        Self {
            history: 2,
            ownership: 2,
            pins: 2,
            suite: 2,
            plan: 2,
            post: 2,
            review: 2,
            accept: 2,
            retire: 2,
            claims: 2,
            evidence: 1,
            budget: 2,
        }
    }
}

impl StrikeLimits {
    pub fn limit(&self, reason: Reason) -> u32 {
        match reason {
            Reason::History => self.history,
            Reason::Ownership => self.ownership,
            Reason::Pins => self.pins,
            Reason::Suite => self.suite,
            Reason::Plan => self.plan,
            Reason::Post => self.post,
            Reason::Review => self.review,
            Reason::Accept => self.accept,
            Reason::Retire => self.retire,
            Reason::Claims => self.claims,
            Reason::Evidence => self.evidence,
            Reason::Budget => self.budget,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PlayerConfig {
    /// Launches a task's implementing worker (spec §9's launch row).
    pub agent_cmd: String,
    /// Launches a task's task agent, the director's delegate that posts and
    /// reviews it (spec §17). Empty turns task agents off: the director
    /// posts and sends back itself.
    pub task_agent_cmd: String,
    /// Seconds a live session that owes work may show the same screen
    /// before it's reported idle (its tmux pane unchanged: hung, or sitting
    /// on a dialog), whatever the agent. 0 turns this off.
    pub idle_after: u64,
    /// Lock files (`Cargo.lock`, …) a task agent may commit with a post,
    /// send-back or contest reply, because a test-only dependency's check
    /// rewrites them. Read from main's tip, so an agent can't widen it. The
    /// impurity check still proves a committed lock is the one the build
    /// produces. Default none.
    pub lock_files: Vec<String>,
    /// Lock file path -> command that regenerates it. When an accept's merge
    /// of `main` conflicts *only* in paths named here, the player checks out
    /// the branch's version of each, runs its command in the jail, stages
    /// the result and lets the suite judge it; any other conflict still goes
    /// to the director. Default none.
    pub lock_regen: BTreeMap<String, String>,
    /// How tasks run (spec §4). Default sequential.
    pub runs: Runs,
}

/// `[player] runs`: how many tasks may be in flight, and who runs tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runs {
    /// One task in flight at a time: while one is staged, posted, being
    /// worked, under review, conflicted, contested or escalated, every
    /// other task that would be `ready` stays `pending`.
    #[default]
    Sequential,
    /// Any number of tasks in flight at once.
    Parallel,
    /// Parallel tasks, but test runs go through a queue the player serves
    /// one at a time; agents ask the player to run them (no strikes).
    Queued,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            agent_cmd: "claude --dangerously-skip-permissions".to_string(),
            task_agent_cmd: "claude --dangerously-skip-permissions".to_string(),
            idle_after: 900,
            lock_files: Vec::new(),
            lock_regen: BTreeMap::new(),
            runs: Runs::Sequential,
        }
    }
}

impl PlayerConfig {
    pub fn task_agents_enabled(&self) -> bool {
        !self.task_agent_cmd.trim().is_empty()
    }
}

// --------------------------------------------------------------------------
// Rejection reasons (spec §10)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    History,
    Ownership,
    Pins,
    Suite,
    Plan,
    Post,
    Review,
    Accept,
    Retire,
    /// A spike's finish claim whose findings or claims don't validate (spec §14).
    Claims,
    /// A `flaky-test-fix` post whose evidence shows no flake (not a
    /// structural mistake by the director).
    Evidence,
    /// The suite ran past its `[budget]`: a check timed out, or the total
    /// exceeded `suite_fail`.
    Budget,
}

impl Reason {
    pub const ALL: [Reason; 12] = [
        Reason::History,
        Reason::Ownership,
        Reason::Pins,
        Reason::Suite,
        Reason::Plan,
        Reason::Post,
        Reason::Review,
        Reason::Accept,
        Reason::Retire,
        Reason::Claims,
        Reason::Evidence,
        Reason::Budget,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Reason::History => "history",
            Reason::Ownership => "ownership",
            Reason::Pins => "pins",
            Reason::Suite => "suite",
            Reason::Plan => "plan",
            Reason::Post => "post",
            Reason::Review => "review",
            Reason::Accept => "accept",
            Reason::Retire => "retire",
            Reason::Claims => "claims",
            Reason::Evidence => "evidence",
            Reason::Budget => "budget",
        }
    }
}

impl FromStr for Reason {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "history" => Ok(Reason::History),
            "ownership" => Ok(Reason::Ownership),
            "pins" => Ok(Reason::Pins),
            "suite" => Ok(Reason::Suite),
            "plan" => Ok(Reason::Plan),
            "post" => Ok(Reason::Post),
            "review" => Ok(Reason::Review),
            "accept" => Ok(Reason::Accept),
            "retire" => Ok(Reason::Retire),
            "claims" => Ok(Reason::Claims),
            "evidence" => Ok(Reason::Evidence),
            "budget" => Ok(Reason::Budget),
            other => Err(format!("unknown rejection reason: {other:?}")),
        }
    }
}

// --------------------------------------------------------------------------
// Checks (spec §6.1) — shared by tests.toml and task.toml
// --------------------------------------------------------------------------

// `pub` (not `pub(crate)` like the rest of this module): src/checks.rs's
// public `Runner::run_check` takes `&Check` directly, and integration tests
// outside this crate build `Check` literals to drive it.
#[derive(Debug, Clone, Deserialize)]
pub struct Check {
    pub name: String,
    pub cmd: String,
    #[serde(default = "default_runs")]
    pub runs: u32,
    #[serde(default = "default_min_pass")]
    pub min_pass: u32,
    /// On a failing verdict, re-run the whole check up to this many more
    /// times; it passes if any attempt passes. Director-owned, like the rest
    /// of the check. Never applied to a task's own `[check]` (spec §6.3).
    #[serde(default)]
    pub retry_failed: u32,
    /// Regexes: only retry when a failing run's output matches one of them.
    /// Empty means every failure is retried (while `retry_failed` lasts).
    #[serde(default)]
    pub retry_on: Vec<String>,
}

fn default_runs() -> u32 {
    1
}

fn default_min_pass() -> u32 {
    1
}

impl Check {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.runs < 1 {
            return Err(format!(
                "check {:?}: runs must be >= 1 (got {})",
                self.name, self.runs
            ));
        }
        if self.min_pass < 1 || self.min_pass > self.runs {
            return Err(format!(
                "check {:?}: min_pass must be between 1 and runs ({}), got {}",
                self.name, self.runs, self.min_pass
            ));
        }
        for pattern in &self.retry_on {
            regex::Regex::new(pattern)
                .map_err(|e| format!("check {:?}: retry_on {pattern:?}: {e}", self.name))?;
        }
        if !self.retry_on.is_empty() && self.retry_failed == 0 {
            return Err(format!(
                "check {:?}: retry_on needs retry_failed >= 1",
                self.name
            ));
        }
        Ok(())
    }

    /// A copy that never retries.
    pub fn without_retries(&self) -> Check {
        Check {
            retry_failed: 0,
            retry_on: Vec::new(),
            ..self.clone()
        }
    }
}

// --------------------------------------------------------------------------
// tests.toml (spec §6.2)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TestsConfig {
    pub(crate) check: Vec<Check>,
    pub(crate) test_paths: Vec<String>,
    /// Prints the project's test inventory, one test per line (e.g. `cargo
    /// test -- --list`). Only a `flaky-test-fix` task needs it.
    pub(crate) list_cmd: Option<String>,
}

impl TestsConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for c in &self.check {
            c.validate()?;
        }
        Ok(())
    }
}

/// The directory of per-task suite fragments (spec §6.2): each post adds its
/// task check as `tests.d/<slug>.toml` instead of appending to `tests.toml`,
/// so parallel tasks never edit the same file.
pub const SUITE_DIR: &str = "tests.d";

/// `tests.d/<slug>.toml`: where a task's post puts its task check.
pub fn suite_fragment_path(slug: &str) -> String {
    format!("{SUITE_DIR}/{slug}.toml")
}

/// Whether `path` is a suite fragment: a `.toml` file directly in `tests.d/`.
pub fn is_suite_fragment(path: &str) -> bool {
    path.strip_prefix(SUITE_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| !name.contains('/') && name.ends_with(".toml"))
}

/// A `tests.d/*.toml` fragment: only `[[check]]` entries. `test_paths` and
/// `list_cmd` stay project-wide, in `tests.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TestsFragment {
    check: Vec<Check>,
}

/// The whole project suite: `tests.toml` (`base`) plus every fragment, in
/// path order, as `(path, text)`. Check names must be unique across all of
/// them, since results are keyed by name.
pub(crate) fn suite_from_parts(
    base: &str,
    fragments: &[(String, String)],
) -> Result<TestsConfig, String> {
    let mut cfg = tests_config_from_str(base).map_err(|e| format!("tests.toml: {e}"))?;
    let mut sorted: Vec<&(String, String)> = fragments.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, text) in sorted {
        let frag: TestsFragment = toml::from_str(text).map_err(|e| format!("{path}: {e}"))?;
        for c in &frag.check {
            c.validate().map_err(|e| format!("{path}: {e}"))?;
        }
        cfg.check.extend(frag.check);
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in &cfg.check {
        if !seen.insert(c.name.as_str()) {
            return Err(format!(
                "check name {:?} is declared more than once across tests.toml and tests.d/",
                c.name
            ));
        }
    }
    Ok(cfg)
}

/// The project suite at `rev`: `tests.toml` plus every `tests.d/*.toml`.
pub(crate) fn suite_at(git: &crate::git::Git, rev: &str) -> Result<TestsConfig, String> {
    let base = git
        .show_file(rev, "tests.toml")
        .ok_or_else(|| format!("tests.toml not found at {rev}"))?;
    let fragments: Vec<(String, String)> = git
        .ls_tree_files(rev, &format!("{SUITE_DIR}/"))
        .into_iter()
        .filter(|p| is_suite_fragment(p))
        .map(|p| {
            let text = git.show_file(rev, &p).unwrap_or_default();
            (p, text)
        })
        .collect();
    suite_from_parts(&base, &fragments)
}

// --------------------------------------------------------------------------
// tasks/<slug>/task.toml (spec §8.2)
// --------------------------------------------------------------------------

/// What a task produces: ordinary code that merges into `main`, or a spike —
/// an answer to a pinned question, backed by claims, that never merges
/// (spec §14).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Task,
    Spike,
    /// Replaces a flaky test with a stable one: `fix_paths` are unpinned for
    /// this task, and the director's `evidence` proves the flake on `main`.
    #[serde(rename = "flaky-test-fix")]
    FlakyTestFix,
    /// Makes slow suite checks faster (spec §15.1): the test tree and the
    /// checks' `cmd`s are unpinned for this task, the code stays out of
    /// reach, and the finish is judged by a ratchet against `main`'s
    /// recorded timings while `main` is over its `[budget]`.
    #[serde(rename = "test-speedup")]
    TestSpeedup,
}

/// One `[[slow]]` of a test-speedup: a suite check that is slow on `main`
/// (killed for its timeout, or with a recorded wall time).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Slow {
    pub check: String,
}

/// One `[[replace]]` of a flaky-test-fix: the flaky test and its replacement.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Replace {
    pub old: String,
    pub new: String,
}

/// One `[[evidence]]` of a flaky-test-fix: the command is run `runs` times
/// on `main`, and between `min_pass` and `max_pass` of them must pass —
/// it works sometimes and fails sometimes.
#[derive(Debug, Clone, Deserialize)]
pub struct Evidence {
    pub name: String,
    pub cmd: String,
    #[serde(default = "default_runs")]
    pub runs: u32,
    #[serde(default = "default_min_pass")]
    pub min_pass: u32,
    pub max_pass: u32,
}

impl Evidence {
    /// The runner's view of this evidence. Prefixed so it can't share a
    /// result-cache slot with a suite check of the same name.
    pub fn as_check(&self) -> Check {
        Check {
            name: format!("evidence:{}", self.name),
            cmd: self.cmd.clone(),
            runs: self.runs,
            min_pass: self.min_pass,
            retry_failed: 0,
            retry_on: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.as_check()
            .validate()
            .map_err(|e| e.replacen("check", "evidence", 1))?;
        if self.max_pass < self.min_pass || self.max_pass >= self.runs {
            return Err(format!(
                "evidence {:?}: max_pass must be between min_pass ({}) and runs - 1 ({}), got {}: a flake fails sometimes",
                self.name,
                self.min_pass,
                self.runs.saturating_sub(1),
                self.max_pass
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TaskConfig {
    pub title: String,
    pub kind: TaskKind,
    pub phase: String,
    pub milestone: bool,
    pub depends_on: Vec<String>,
    pub check: Option<Check>,
    /// The director's context for the task agent (spec §17): pasted into
    /// its prompt verbatim.
    pub brief: String,
    /// flaky-test-fix only: the files unpinned for this task.
    pub fix_paths: Vec<String>,
    /// flaky-test-fix only.
    pub replace: Vec<Replace>,
    /// flaky-test-fix only.
    pub evidence: Vec<Evidence>,
    /// test-speedup only: the slow checks the task is to speed up.
    pub slow: Vec<Slow>,
}

impl TaskConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(c) = &self.check {
            c.validate()?;
        }
        for e in &self.evidence {
            e.validate()?;
        }
        if self.kind == TaskKind::TestSpeedup {
            if self.slow.is_empty() {
                return Err(
                    "a test-speedup needs at least one [[slow]] naming a suite check".to_string(),
                );
            }
            for s in &self.slow {
                if s.check.trim().is_empty() {
                    return Err("[[slow]] check must name a suite check".to_string());
                }
            }
            if self.check.is_some() {
                return Err("a test-speedup has no task [check]: its evidence is the [[slow]] checks' recorded times on main".to_string());
            }
            if self.milestone {
                return Err("a test-speedup can't be a milestone".to_string());
            }
        } else if !self.slow.is_empty() {
            return Err("[[slow]] is only for kind = \"test-speedup\"".to_string());
        }
        if self.kind == TaskKind::TestSpeedup {
            if !self.fix_paths.is_empty() || !self.replace.is_empty() || !self.evidence.is_empty() {
                return Err("fix_paths, [[replace]] and [[evidence]] are only for kind = \"flaky-test-fix\"".to_string());
            }
        } else if self.kind != TaskKind::FlakyTestFix {
            if !self.fix_paths.is_empty() || !self.replace.is_empty() || !self.evidence.is_empty() {
                return Err("fix_paths, [[replace]] and [[evidence]] are only for kind = \"flaky-test-fix\"".to_string());
            }
        } else {
            if self.check.is_none() {
                return Err(
                    "a flaky-test-fix needs a [check] proving the replacement is stable"
                        .to_string(),
                );
            }
            if self.replace.is_empty() {
                return Err("a flaky-test-fix needs at least one [[replace]]".to_string());
            }
            for r in &self.replace {
                if r.old.trim().is_empty() || r.new.trim().is_empty() || r.old == r.new {
                    return Err(format!(
                        "replace {:?} -> {:?}: old and new must be different, non-empty test names",
                        r.old, r.new
                    ));
                }
            }
            if self.evidence.is_empty() {
                return Err(
                    "a flaky-test-fix needs at least one [[evidence]] showing the flake on main"
                        .to_string(),
                );
            }
            if self.fix_paths.is_empty() {
                return Err(
                    "a flaky-test-fix needs fix_paths: the files the worker may change".to_string(),
                );
            }
            for p in &self.fix_paths {
                if p.trim().is_empty() || p.contains(['*', '?', '[']) || p.ends_with('/') {
                    return Err(format!(
                        "fix_paths entry {p:?} must be a plain file path, never a directory or a wildcard"
                    ));
                }
                if p == "tests.toml" || p.starts_with("tasks/") {
                    return Err(format!(
                        "fix_paths entry {p:?} may not be tests.toml or under tasks/"
                    ));
                }
            }
        }
        if self.kind == TaskKind::Spike {
            if self.check.is_some() {
                return Err("a spike has no task [check]: its evidence is its claims".to_string());
            }
            if !self.depends_on.is_empty() {
                return Err("a spike can't depend on other tasks".to_string());
            }
            if self.milestone {
                return Err("a spike can't be a milestone".to_string());
            }
        }
        Ok(())
    }
}

// --------------------------------------------------------------------------
// Spikes (spec §14): questions/<n>.toml, baseline/<n>.toml, claims/<n>.toml
// --------------------------------------------------------------------------

/// The one answer every spike question allows besides its own `answers`.
pub const INCONCLUSIVE: &str = "inconclusive";

/// `tasks/<slug>/questions/<n>.toml`: the pinned question of round `n`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Question {
    pub question: String,
    pub why: String,
    pub answers: Vec<String>,
    /// The earlier round this question sharpens, if any.
    pub refines: Option<u32>,
}

impl Question {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.question.trim().is_empty() {
            return Err("question must not be empty".to_string());
        }
        if self.answers.is_empty() {
            return Err("answers must list at least one answer".to_string());
        }
        for (i, a) in self.answers.iter().enumerate() {
            if a.trim().is_empty() {
                return Err("answers must not contain an empty answer".to_string());
            }
            if self.answers[..i].contains(a) {
                return Err(format!("answers lists {a:?} twice"));
            }
        }
        Ok(())
    }

    /// Whether `answer` is one this question allows.
    pub fn allows(&self, answer: &str) -> bool {
        answer == INCONCLUSIVE || self.answers.iter().any(|a| a == answer)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// Describes the code as it is now; must pass on the base commit.
    #[default]
    Baseline,
    /// Describes what the exploration achieved; passes on the branch, fails
    /// on the base commit.
    Spike,
}

impl ClaimKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimKind::Baseline => "baseline",
            ClaimKind::Spike => "spike",
        }
    }
}

/// How long a claim run may take before the player kills it, unless the
/// claim sets its own `timeout` (seconds).
pub const DEFAULT_CLAIM_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Deserialize)]
pub struct Claim {
    pub name: String,
    pub cmd: String,
    #[serde(default)]
    pub kind: ClaimKind,
    /// The answer a spike claim backs.
    #[serde(default)]
    pub supports: Option<String>,
    #[serde(default = "default_runs")]
    pub runs: u32,
    #[serde(default = "default_min_pass")]
    pub min_pass: u32,
    #[serde(default)]
    pub timeout: Option<u64>,
}

impl Claim {
    pub fn timeout_secs(&self) -> u64 {
        self.timeout.unwrap_or(DEFAULT_CLAIM_TIMEOUT_SECS)
    }

    /// The runner's view of this claim. The name is prefixed so a claim can
    /// never share a result-cache slot with a suite check of the same name.
    pub fn as_check(&self) -> Check {
        Check {
            name: format!("claim:{}", self.name),
            cmd: self.cmd.clone(),
            runs: self.runs,
            min_pass: self.min_pass,
            retry_failed: 0,
            retry_on: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("a claim needs a name".to_string());
        }
        if self.cmd.trim().is_empty() {
            return Err(format!("claim {:?}: cmd must not be empty", self.name));
        }
        if self.runs < 1 || self.min_pass < 1 || self.min_pass > self.runs {
            return Err(format!(
                "claim {:?}: min_pass must be between 1 and runs ({}), got {}",
                self.name, self.runs, self.min_pass
            ));
        }
        if self.timeout == Some(0) {
            return Err(format!(
                "claim {:?}: timeout must be at least 1 second",
                self.name
            ));
        }
        Ok(())
    }
}

/// `tasks/<slug>/claims/<n>.toml`, written by the worker: its answer to the
/// round's question plus the claims that back it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaimsFile {
    pub answer: String,
    pub claim: Vec<Claim>,
}

/// `tasks/<slug>/baseline/<n>.toml`, written by the director: baseline
/// claims on the current code, pinned.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BaselineFile {
    pub claim: Vec<Claim>,
}

fn validate_claim_list(claims: &[Claim]) -> Result<(), String> {
    for c in claims {
        c.validate()?;
    }
    Ok(())
}

pub(crate) fn question_from_str(s: &str) -> Result<Question, String> {
    let q: Question = toml::from_str(s).map_err(|e| e.to_string())?;
    q.validate()?;
    Ok(q)
}

pub(crate) fn claims_file_from_str(s: &str) -> Result<ClaimsFile, String> {
    let f: ClaimsFile = toml::from_str(s).map_err(|e| e.to_string())?;
    validate_claim_list(&f.claim)?;
    if f.answer.trim().is_empty() {
        return Err(
            "answer must be set (one of the question's answers, or \"inconclusive\")".to_string(),
        );
    }
    Ok(f)
}

pub(crate) fn baseline_file_from_str(s: &str) -> Result<BaselineFile, String> {
    let mut f: BaselineFile = toml::from_str(s).map_err(|e| e.to_string())?;
    validate_claim_list(&f.claim)?;
    for c in &mut f.claim {
        if c.kind != ClaimKind::Baseline {
            return Err(format!(
                "claim {:?}: baseline/<n>.toml only holds baseline claims",
                c.name
            ));
        }
        if c.supports.is_some() {
            return Err(format!(
                "claim {:?}: a baseline claim backs no answer (drop `supports`)",
                c.name
            ));
        }
    }
    Ok(f)
}

// --------------------------------------------------------------------------
// Loading helpers
// --------------------------------------------------------------------------

/// `path` is exactly one of `test_paths` (spec §6.2), or falls under one of
/// them treated as a directory prefix. Used by the send-back path check.
pub(crate) fn path_under_test_paths(path: &str, test_paths: &[String]) -> bool {
    test_paths.iter().any(|t| {
        let prefix = t.trim_end_matches('/');
        path == prefix || path.starts_with(&format!("{prefix}/"))
    })
}

/// A `[player] lock_files` entry under `test_paths` would be pinned against
/// the worker, who then couldn't add the product's own dependencies.
pub(crate) fn check_lock_files(lock_files: &[String], test_paths: &[String]) -> Result<(), String> {
    match lock_files
        .iter()
        .find(|f| path_under_test_paths(f, test_paths))
    {
        Some(f) => Err(format!(
            "[player] lock_files: {f:?} is under test_paths, so it is pinned against the worker; a lock file is the task agent's and the worker's both"
        )),
        None => Ok(()),
    }
}

// --------------------------------------------------------------------------
// architecture.toml (architect-v3.md)
// --------------------------------------------------------------------------

/// `architecture.toml` on `main`: the modules, the API between each connected
/// pair, and every budget. Pinned like `test_paths`. A budget of 0 is off.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ArchConfig {
    /// Total modules budget: the most modules the project may have.
    pub max_modules: u64,
    /// Total composition roots budget.
    pub max_roots: u64,
    /// Paths visible in every module's jail view (build files, lock files).
    pub shared: Vec<String>,
    /// Build caches (`node_modules`, `target`) every view links to: symlinks
    /// into the checkout, never copied and never counted toward a budget.
    pub linked: Vec<String>,
    /// Every file under these must be owned by a module, an API, a root or
    /// `shared` (ownership completeness).
    pub source_paths: Vec<String>,
    #[serde(rename = "module")]
    pub modules: Vec<ModuleConfig>,
    /// Composition roots: call modules and forward their values; budgeted
    /// like a module, story-tested only (no `test_cmd`, no view).
    #[serde(rename = "root")]
    pub roots: Vec<RootConfig>,
    #[serde(rename = "api")]
    pub apis: Vec<ApiConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RootConfig {
    pub name: String,
    pub lang: String,
    pub paths: Vec<String>,
    pub max_ast: u64,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ModuleConfig {
    pub name: String,
    /// `rust`, `typescript` or `kotlin`: picks the AST grammar for `.rs`,
    /// `.ts`/`.tsx` and `.kt`/`.kts` files.
    pub lang: String,
    /// Files and directories (relative to the repo root) that make up the module.
    pub paths: Vec<String>,
    pub max_ast: u64,
    pub max_bytes: u64,
    /// Run in the module's jail view (only its files, its APIs' files and
    /// `shared`): a stray import of another module fails to resolve.
    pub test_cmd: Option<String>,
    /// A project's own lint adapter, run in the checkout root with
    /// `DASHBOARD_MODULE` set. It prints a JSON array of
    /// `{"file", "line", "message"}` findings on stdout; any finding fails
    /// the check. Runs beside the built-in contract checks, not instead.
    pub lint_cmd: Option<String>,
}

/// One API per connected pair: `consumer` consumes `generator`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiConfig {
    pub name: String,
    pub generator: String,
    pub consumer: String,
    pub paths: Vec<String>,
    pub max_constructors: u64,
    pub max_types: u64,
    /// Variants across the API's elements: readable data widens a
    /// consumer's decision space like a constructor does.
    pub max_elements: u64,
    /// Fields across those variants.
    pub max_fields: u64,
    /// `fn`s of the API, and input slots across them.
    pub max_fns: u64,
    pub max_inputs: u64,
    /// Caps on every string / bytes `max_len` in the API: 0 means the
    /// default (256 for strings, none for bytes).
    pub max_string_len: u64,
    pub max_bytes_len: u64,
    /// Comparisons across the API's `where` refinements, and tokens
    /// declared (architect-v3.md, "Proofs supplied by the caller").
    pub max_rules: u64,
    pub max_tokens: u64,
    /// The hand-written instance of the API's container classes: under the
    /// generator module's paths (owned, budgeted, the worker's to edit, so
    /// never pinned), mounted beneath the generated binding and copied into
    /// every consumer's view. Several APIs may share one file.
    #[serde(rename = "impl")]
    pub impls: Vec<String>,
}

/// The TOML manifest's file name on `main`.
pub const ARCH_FILE: &str = "architecture.toml";
/// The Lean architecture package on `main` (a directory; `lean::DIR`).
pub const ARCH_DIR: &str = "architecture";
/// The file whose presence says a project's architecture is Lean.
pub const ARCH_LEAN_MANIFEST: &str = "architecture/Architecture.lean";

/// Whether `path` is the TOML manifest or inside the Lean package: either way
/// the director's, pinned against workers.
pub fn is_arch_path(path: &str) -> bool {
    path == ARCH_FILE || path == ARCH_DIR || path.starts_with("architecture/")
}

/// Whether `path` is an API's Lean IDL (`architecture/Apis/<name>.lean`): the
/// one part of the package a task agent may write.
pub fn is_arch_api_file(path: &str) -> bool {
    path.strip_prefix("architecture/Apis/").is_some_and(|rest| !rest.contains('/') && rest.ends_with(".lean"))
}

impl ArchConfig {
    /// Every path an API is made of, as written in the manifest.
    pub fn api_paths(&self) -> Vec<String> {
        self.apis.iter().flat_map(|a| a.paths.iter().cloned()).collect()
    }

    /// The module or root named `name`: `(lang, paths)`.
    pub fn node(&self, name: &str) -> Option<(&str, &[String])> {
        self.modules
            .iter()
            .find(|m| m.name == name)
            .map(|m| (m.lang.as_str(), m.paths.as_slice()))
            .or_else(|| self.roots.iter().find(|r| r.name == name).map(|r| (r.lang.as_str(), r.paths.as_slice())))
    }

    /// The generator -> consumer edges, with a cycle if there is one: the
    /// module graph must be a DAG (architect-v3.md, "The DAG").
    pub fn cycle(&self) -> Option<Vec<String>> {
        let mut nodes: Vec<&str> = self.modules.iter().map(|m| m.name.as_str()).collect();
        nodes.extend(self.roots.iter().map(|r| r.name.as_str()));
        let edges: Vec<(&str, &str)> = self.apis.iter().map(|a| (a.generator.as_str(), a.consumer.as_str())).collect();
        // Iterative DFS with colors; the first back edge found names the cycle.
        let mut color: std::collections::BTreeMap<&str, u8> = nodes.iter().map(|n| (*n, 0)).collect();
        for start in &nodes {
            if color[start] != 0 {
                continue;
            }
            let mut path: Vec<&str> = vec![start];
            let mut iters: Vec<usize> = vec![0];
            color.insert(start, 1);
            while let Some(&cur) = path.last() {
                let i = iters.last_mut().unwrap();
                let next = edges.iter().filter(|(g, _)| *g == cur).map(|(_, c)| *c).nth(*i);
                *i += 1;
                match next {
                    Some(n) => match color.get(n).copied().unwrap_or(2) {
                        1 => {
                            let at = path.iter().position(|p| *p == n).unwrap();
                            let mut cyc: Vec<String> = path[at..].iter().map(|s| s.to_string()).collect();
                            cyc.push(n.to_string());
                            return Some(cyc);
                        }
                        0 => {
                            color.insert(n, 1);
                            path.push(n);
                            iters.push(0);
                        }
                        _ => {}
                    },
                    None => {
                        color.insert(cur, 2);
                        path.pop();
                        iters.pop();
                    }
                }
            }
        }
        None
    }

    /// Module names in topological order, generators before consumers
    /// (roots last). Only meaningful when `cycle()` is `None`.
    pub fn topological(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let names: Vec<&str> = self.modules.iter().map(|m| m.name.as_str()).collect();
        let mut done: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        while done.len() < names.len() {
            let mut progressed = false;
            for n in &names {
                if done.contains(n) {
                    continue;
                }
                let ready = self.apis.iter().filter(|a| a.consumer == *n).all(|a| done.contains(a.generator.as_str()));
                if ready {
                    done.insert(n);
                    out.push(n.to_string());
                    progressed = true;
                }
            }
            if !progressed {
                // A cycle: append the rest in manifest order.
                out.extend(names.iter().filter(|n| !done.contains(*n)).map(|n| n.to_string()));
                break;
            }
        }
        out
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        let mut names = std::collections::BTreeSet::new();
        for m in &self.modules {
            if m.name.is_empty()
                || !m.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
                || !names.insert(m.name.as_str())
            {
                return Err(format!(
                    "[[module]] name {:?}: empty, duplicate, or not lowercase letters, digits, - and _",
                    m.name
                ));
            }
            if m.paths.is_empty() {
                return Err(format!("[[module]] {}: no paths", m.name));
            }
            if !matches!(m.lang.as_str(), "rust" | "typescript" | "kotlin") {
                return Err(format!(
                    "[[module]] {}: lang {:?} must be rust, typescript or kotlin",
                    m.name, m.lang
                ));
            }
        }
        let mut root_names = std::collections::BTreeSet::new();
        for r in &self.roots {
            if r.name.is_empty()
                || !r.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
                || names.contains(r.name.as_str())
                || !root_names.insert(r.name.as_str())
            {
                return Err(format!(
                    "[[root]] name {:?}: empty, a module's or another root's, or not lowercase letters, digits, - and _",
                    r.name
                ));
            }
            if r.paths.is_empty() {
                return Err(format!("[[root]] {}: no paths", r.name));
            }
            if !matches!(r.lang.as_str(), "rust" | "typescript" | "kotlin") {
                return Err(format!("[[root]] {}: lang {:?} must be rust, typescript or kotlin", r.name, r.lang));
            }
        }
        let mut api_names = std::collections::BTreeSet::new();
        let mut pairs = std::collections::BTreeSet::new();
        for a in &self.apis {
            if a.name.is_empty() || !api_names.insert(a.name.as_str()) {
                return Err(format!("[[api]] name {:?}: empty or duplicate", a.name));
            }
            if !names.contains(a.generator.as_str()) {
                return Err(format!("[[api]] {}: unknown module {:?}", a.name, a.generator));
            }
            if !names.contains(a.consumer.as_str()) && !root_names.contains(a.consumer.as_str()) {
                return Err(format!("[[api]] {}: unknown module or root {:?}", a.name, a.consumer));
            }
            if a.max_string_len > 0 && a.max_bytes_len > 0 && a.max_string_len > a.max_bytes_len {
                // No rule ties them; each caps its own repr. Nothing to check.
            }
            if a.generator == a.consumer {
                return Err(format!("[[api]] {}: generator and consumer are the same", a.name));
            }
            if !pairs.insert((a.generator.as_str(), a.consumer.as_str())) {
                return Err(format!(
                    "[[api]] {}: a second API for {} -> {} (one API per connected pair)",
                    a.name, a.generator, a.consumer
                ));
            }
            if a.paths.is_empty() {
                return Err(format!("[[api]] {}: no paths", a.name));
            }
            let generator = self.modules.iter().find(|m| m.name == a.generator).unwrap();
            for i in &a.impls {
                if !path_under_test_paths(i, &generator.paths) {
                    return Err(format!(
                        "[[api]] {}: impl {i:?} is not under generator module {}'s paths",
                        a.name, a.generator
                    ));
                }
                if path_under_test_paths(i, &self.api_paths()) {
                    return Err(format!("[[api]] {}: impl {i:?} is under an API's paths, so it would be pinned", a.name));
                }
            }
        }
        if let Some(cyc) = self.cycle() {
            return Err(format!("the module graph has a cycle: {} (generator -> consumer must be a DAG)", cyc.join(" -> ")));
        }
        // A link must never expose another module's sources into a view.
        let owned: Vec<&String> = self
            .modules
            .iter()
            .flat_map(|m| &m.paths)
            .chain(self.roots.iter().flat_map(|r| &r.paths))
            .chain(self.apis.iter().flat_map(|a| &a.paths))
            .collect();
        for l in &self.linked {
            let l = l.trim_end_matches('/');
            if l.is_empty() || l.starts_with('/') || l.split('/').any(|c| c == ".." || c == ".") {
                return Err(format!("linked {l:?}: must be a relative path without . or .."));
            }
            if let Some(p) = owned.iter().find(|p| {
                let p = p.trim_end_matches('/');
                path_under_test_paths(l, &[p.to_string()]) || path_under_test_paths(p, &[l.to_string()])
            }) {
                return Err(format!("linked {l:?}: overlaps module or API path {p:?}"));
            }
        }
        Ok(())
    }
}

pub fn arch_config_from_str(s: &str) -> Result<ArchConfig, String> {
    let cfg: ArchConfig = toml::from_str(s).map_err(|e| e.to_string())?;
    cfg.validate()?;
    Ok(cfg)
}

/// The `arch` object of a Lean export (`lean::export`), validated like the TOML.
pub fn arch_config_from_json(v: serde_json::Value) -> Result<ArchConfig, String> {
    let cfg: ArchConfig = serde_json::from_value(v).map_err(|e| e.to_string())?;
    cfg.validate()?;
    Ok(cfg)
}

fn lean_ident(name: &str) -> String {
    crate::lean::ident(name)
}

fn lean_strs(xs: &[String]) -> String {
    let items: Vec<String> = xs.iter().map(|s| format!("{s:?}")).collect();
    format!("[{}]", items.join(", "))
}

impl ArchConfig {
    /// The manifest as `architecture/Architecture.lean` (`dashboard arch
    /// migrate`): the same fields, one block per module, root and API.
    pub fn lean(&self) -> String {
        let mut o = String::from(
            "-- The architecture manifest (architect-v3.md): modules, roots, APIs and
             -- budgets, in the dashboard's Lean DSL (architecture/Dashboard.lean).
             -- Director-owned and pinned; each API's IDL is Apis/<name>.lean.
             import Dashboard
open Dashboard

architecture where
",
        );
        o.push_str(&format!("  max_modules = {}
", self.max_modules));
        if self.max_roots > 0 {
            o.push_str(&format!("  max_roots = {}
", self.max_roots));
        }
        o.push_str(&format!("  shared = {}
", lean_strs(&self.shared)));
        o.push_str(&format!("  linked = {}
", lean_strs(&self.linked)));
        if !self.source_paths.is_empty() {
            o.push_str(&format!("  source_paths = {}
", lean_strs(&self.source_paths)));
        }
        for m in &self.modules {
            o.push_str(&format!("
module {} where
  lang = {}
  paths = {}
", lean_ident(&m.name), m.lang, lean_strs(&m.paths)));
            if m.max_bytes > 0 {
                o.push_str(&format!("  max_bytes = {}
", m.max_bytes));
            }
            if m.max_ast > 0 {
                o.push_str(&format!("  max_ast = {}
", m.max_ast));
            }
            if let Some(c) = &m.test_cmd {
                o.push_str(&format!("  test_cmd = {c:?}
"));
            }
            if let Some(c) = &m.lint_cmd {
                o.push_str(&format!("  lint_cmd = {c:?}
"));
            }
        }
        for r in &self.roots {
            o.push_str(&format!("
root {} where
  lang = {}
  paths = {}
", lean_ident(&r.name), r.lang, lean_strs(&r.paths)));
            if r.max_bytes > 0 {
                o.push_str(&format!("  max_bytes = {}
", r.max_bytes));
            }
            if r.max_ast > 0 {
                o.push_str(&format!("  max_ast = {}
", r.max_ast));
            }
        }
        for a in &self.apis {
            o.push_str(&format!(
                "
api {} : {} → {} where
  paths = {}
",
                lean_ident(&a.name),
                lean_ident(&a.generator),
                lean_ident(&a.consumer),
                lean_strs(&a.paths)
            ));
            if !a.impls.is_empty() {
                o.push_str(&format!("  impl = {}
", lean_strs(&a.impls)));
            }
            for (k, v) in [
                ("max_constructors", a.max_constructors),
                ("max_types", a.max_types),
                ("max_elements", a.max_elements),
                ("max_fields", a.max_fields),
                ("max_fns", a.max_fns),
                ("max_inputs", a.max_inputs),
                ("max_string_len", a.max_string_len),
                ("max_bytes_len", a.max_bytes_len),
                ("max_rules", a.max_rules),
                ("max_tokens", a.max_tokens),
            ] {
                if v > 0 {
                    o.push_str(&format!("  {k} = {v}
"));
                }
            }
        }
        o
    }
}

pub fn roadmap_config_from_str(s: &str) -> Result<RoadmapConfig, String> {
    let cfg: RoadmapConfig = toml::from_str(s).map_err(|e| e.to_string())?;
    cfg.sandbox.validate()?;
    cfg.budget.validate()?;
    Ok(cfg)
}

pub(crate) fn tests_config_from_str(s: &str) -> Result<TestsConfig, String> {
    let cfg: TestsConfig = toml::from_str(s).map_err(|e| e.to_string())?;
    cfg.validate()?;
    Ok(cfg)
}

pub(crate) fn task_config_from_str(s: &str) -> Result<TaskConfig, String> {
    let cfg: TaskConfig = toml::from_str(s).map_err(|e| e.to_string())?;
    cfg.validate()?;
    Ok(cfg)
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roadmap_config_defaults_on_empty_input() {
        let cfg = roadmap_config_from_str("").unwrap();
        assert_eq!(cfg.title, None);
        assert!(cfg.phases.is_empty());
        assert!(cfg.docs_paths.is_empty());
        assert_eq!(cfg.limits.max_review_rounds, 2);
        assert_eq!(cfg.limits.max_contest_rounds, 2);
        assert_eq!(cfg.limits.max_relaunches, 2);
        assert_eq!(cfg.limits.strikes.history, 2);
        assert_eq!(cfg.limits.strikes.suite, 2);
        assert_eq!(
            cfg.player.agent_cmd,
            "claude --dangerously-skip-permissions"
        );
        assert_eq!(cfg.player.idle_after, 900);
    }

    #[test]
    fn roadmap_config_full_example() {
        let toml = r#"
            title = "My Project"
            docs_paths = ["docs/adr"]

            [[phases]]
            id = "p1"
            title = "Phase One"
            goal = "get started"

            [limits]
            max_review_rounds = 3
            max_relaunches = 4

            [limits.strikes]
            suite = 5
            pins = 1

            [player]
            agent_cmd = "codex run"
            idle_after = 0
            lock_files = ["Cargo.lock"]
            [player.lock_regen]
            "Cargo.lock" = "cargo metadata --offline"
        "#;
        let cfg = roadmap_config_from_str(toml).unwrap();
        assert_eq!(cfg.title.as_deref(), Some("My Project"));
        assert_eq!(cfg.docs_paths, vec!["docs/adr".to_string()]);
        assert_eq!(cfg.phases.len(), 1);
        assert_eq!(cfg.phases[0].id, "p1");
        assert_eq!(cfg.phases[0].title.as_deref(), Some("Phase One"));
        assert_eq!(cfg.limits.max_review_rounds, 3);
        assert_eq!(cfg.limits.max_relaunches, 4);
        assert_eq!(cfg.limits.strikes.suite, 5);
        assert_eq!(cfg.limits.strikes.pins, 1);
        // Untouched strike reasons keep their default.
        assert_eq!(cfg.limits.strikes.history, 2);
        assert_eq!(cfg.player.agent_cmd, "codex run");
        assert_eq!(cfg.player.idle_after, 0);
        assert_eq!(cfg.player.lock_files, vec!["Cargo.lock".to_string()]);
        assert_eq!(
            cfg.player.lock_regen["Cargo.lock"],
            "cargo metadata --offline"
        );
    }

    #[test]
    fn strike_limits_limit_lookup() {
        let mut limits = StrikeLimits::default();
        limits.review = 7;
        assert_eq!(limits.limit(Reason::Review), 7);
        assert_eq!(limits.limit(Reason::History), 2);
    }

    #[test]
    fn reason_round_trips_through_str() {
        for r in Reason::ALL {
            assert_eq!(Reason::from_str(r.as_str()).unwrap(), r);
        }
        assert!(Reason::from_str("bogus").is_err());
    }

    #[test]
    fn tests_config_parses_checks() {
        let toml = r#"
            test_paths = ["tests/"]

            [[check]]
            name = "unit"
            cmd = "cargo test"

            [[check]]
            name = "flaky"
            cmd = "pytest tests/llm"
            runs = 10
            min_pass = 8
        "#;
        let cfg = tests_config_from_str(toml).unwrap();
        assert_eq!(cfg.test_paths, vec!["tests/".to_string()]);
        assert_eq!(cfg.check.len(), 2);
        assert_eq!(cfg.check[0].runs, 1);
        assert_eq!(cfg.check[0].min_pass, 1);
        assert_eq!(cfg.check[1].runs, 10);
        assert_eq!(cfg.check[1].min_pass, 8);
    }

    #[test]
    fn tests_config_rejects_bad_threshold() {
        let toml = r#"
            [[check]]
            name = "bad"
            cmd = "true"
            runs = 3
            min_pass = 4
        "#;
        let err = tests_config_from_str(toml).unwrap_err();
        assert!(err.contains("min_pass"), "unexpected error: {err}");
    }

    #[test]
    fn suite_merges_fragments_in_path_order() {
        let base = "test_paths = [\"tests/\"]\n[[check]]\nname = \"unit\"\ncmd = \"true\"\n";
        let frags = vec![
            (
                "tests.d/b.toml".to_string(),
                "[[check]]\nname = \"b\"\ncmd = \"true\"\n".to_string(),
            ),
            (
                "tests.d/a.toml".to_string(),
                "[[check]]\nname = \"a\"\ncmd = \"true\"\n".to_string(),
            ),
        ];
        let cfg = suite_from_parts(base, &frags).unwrap();
        let names: Vec<&str> = cfg.check.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["unit", "a", "b"]);
        assert_eq!(cfg.test_paths, ["tests/"]);
    }

    #[test]
    fn suite_rejects_duplicate_names_and_bad_fragments() {
        let base = "[[check]]\nname = \"unit\"\ncmd = \"true\"\n";
        let dup = vec![(
            "tests.d/a.toml".to_string(),
            "[[check]]\nname = \"unit\"\ncmd = \"false\"\n".to_string(),
        )];
        assert!(
            suite_from_parts(base, &dup)
                .unwrap_err()
                .contains("more than once")
        );
        let bad = vec![(
            "tests.d/a.toml".to_string(),
            "[[check]]\nname = \"x\"\ncmd = \"true\"\nruns = 0\n".to_string(),
        )];
        assert!(
            suite_from_parts(base, &bad)
                .unwrap_err()
                .starts_with("tests.d/a.toml")
        );
    }

    #[test]
    fn suite_fragment_paths() {
        assert_eq!(suite_fragment_path("foo"), "tests.d/foo.toml");
        assert!(is_suite_fragment("tests.d/foo.toml"));
        assert!(!is_suite_fragment("tests.d/sub/foo.toml"));
        assert!(!is_suite_fragment("tests.d/README.md"));
        assert!(!is_suite_fragment("tests.dx/foo.toml"));
    }

    #[test]
    fn tests_config_rejects_zero_runs() {
        let toml = r#"
            [[check]]
            name = "bad"
            cmd = "true"
            runs = 0
        "#;
        let err = tests_config_from_str(toml).unwrap_err();
        assert!(err.contains("runs"), "unexpected error: {err}");
    }

    #[test]
    fn task_config_defaults_and_unknown_fields_ignored() {
        let toml = r#"
            title = "Do the thing"
            depends_on = ["a", "b"]
            mystery_field = "ignored"
        "#;
        let cfg = task_config_from_str(toml).unwrap();
        assert_eq!(cfg.title, "Do the thing");
        assert_eq!(cfg.phase, "");
        assert!(!cfg.milestone);
        assert_eq!(cfg.depends_on, vec!["a".to_string(), "b".to_string()]);
        assert!(cfg.check.is_none());
    }

    #[test]
    fn claims_strike_limit_defaults_to_two() {
        let cfg = roadmap_config_from_str("[limits.strikes]\nclaims = 4\n").unwrap();
        assert_eq!(cfg.limits.strikes.limit(Reason::Claims), 4);
        assert_eq!(StrikeLimits::default().claims, 2);
    }

    #[test]
    fn spike_task_config_is_validated() {
        let ok = task_config_from_str("kind = \"spike\"\ntitle = \"s\"\n").unwrap();
        assert_eq!(ok.kind, TaskKind::Spike);
        assert_eq!(
            task_config_from_str("title = \"t\"\n").unwrap().kind,
            TaskKind::Task
        );
        assert!(task_config_from_str("kind = \"spike\"\ndepends_on = [\"a\"]\n").is_err());
        assert!(task_config_from_str("kind = \"spike\"\nmilestone = true\n").is_err());
        assert!(
            task_config_from_str("kind = \"spike\"\n[check]\nname = \"c\"\ncmd = \"true\"\n")
                .is_err()
        );
    }

    #[test]
    fn question_parses_and_validates() {
        let q =
            question_from_str("question = \"Can we?\"\nanswers = [\"yes\", \"no\"]\nrefines = 1\n")
                .unwrap();
        assert_eq!(q.refines, Some(1));
        assert!(q.allows("yes"));
        assert!(q.allows("inconclusive"));
        assert!(!q.allows("maybe"));
        assert!(
            question_from_str("answers = [\"yes\"]\n").is_err(),
            "empty question"
        );
        assert!(
            question_from_str("question = \"q\"\n").is_err(),
            "no answers"
        );
        assert!(
            question_from_str("question = \"q\"\nanswers = [\"a\", \"a\"]\n").is_err(),
            "duplicate answers"
        );
    }

    #[test]
    fn claims_file_needs_an_answer_and_valid_claims() {
        let f = claims_file_from_str("answer = \"yes\"\n\n[[claim]]\nname = \"c\"\nkind = \"spike\"\nsupports = \"yes\"\ncmd = \"true\"\n").unwrap();
        assert_eq!(f.claim[0].kind, ClaimKind::Spike);
        assert_eq!(f.claim[0].timeout_secs(), DEFAULT_CLAIM_TIMEOUT_SECS);
        assert_eq!(f.claim[0].as_check().name, "claim:c");
        assert!(
            claims_file_from_str("[[claim]]\nname = \"c\"\ncmd = \"true\"\n").is_err(),
            "no answer"
        );
        assert!(
            claims_file_from_str("answer = \"yes\"\n[[claim]]\nname = \"c\"\ncmd = \"\"\n")
                .is_err(),
            "empty cmd"
        );
        assert!(
            claims_file_from_str(
                "answer = \"yes\"\n[[claim]]\nname = \"c\"\ncmd = \"x\"\nruns = 2\nmin_pass = 3\n"
            )
            .is_err()
        );
    }

    #[test]
    fn baseline_file_only_holds_baseline_claims() {
        assert!(baseline_file_from_str("[[claim]]\nname = \"c\"\ncmd = \"true\"\n").is_ok());
        assert!(
            baseline_file_from_str("[[claim]]\nname = \"c\"\ncmd = \"true\"\nkind = \"spike\"\n")
                .is_err()
        );
        assert!(
            baseline_file_from_str("[[claim]]\nname = \"c\"\ncmd = \"true\"\nsupports = \"yes\"\n")
                .is_err()
        );
    }

    #[test]
    fn budget_is_parsed_defaults_off_and_is_validated() {
        let none = roadmap_config_from_str("").unwrap();
        assert_eq!(none.budget, BudgetConfig::default());
        assert_eq!(none.limits.strikes.limit(Reason::Budget), 2);
        let cfg = roadmap_config_from_str(
            "[budget]\ncheck_timeout = 60\nsuite_warn = 120\nsuite_fail = 300\n[limits.strikes]\nbudget = 3\n",
        )
        .unwrap();
        assert_eq!(cfg.budget.check_timeout, 60);
        assert_eq!(cfg.budget.suite_fail, 300);
        assert_eq!(cfg.limits.strikes.limit(Reason::Budget), 3);
        assert!(roadmap_config_from_str("[budget]\nsuite_warn = 5\nsuite_fail = 5\n").is_err());
        assert!(roadmap_config_from_str("[budget]\nsuite_warn = 5\n").is_ok());
        assert_eq!("budget".parse::<Reason>(), Ok(Reason::Budget));
    }

    #[test]
    fn retry_policy_is_parsed_and_validated() {
        let ok = tests_config_from_str("[[check]]\nname = \"u\"\ncmd = \"x\"\nretry_failed = 2\nretry_on = [\"Text file busy\", \"os error \\\\d+\"]\n").unwrap();
        assert_eq!(ok.check[0].retry_failed, 2);
        assert_eq!(ok.check[0].retry_on.len(), 2);
        assert_eq!(
            tests_config_from_str("[[check]]\nname = \"u\"\ncmd = \"x\"\n")
                .unwrap()
                .check[0]
                .retry_failed,
            0
        );
        assert!(
            tests_config_from_str(
                "[[check]]\nname = \"u\"\ncmd = \"x\"\nretry_failed = 1\nretry_on = [\"(\"]\n"
            )
            .is_err(),
            "bad regex"
        );
        assert!(
            tests_config_from_str("[[check]]\nname = \"u\"\ncmd = \"x\"\nretry_on = [\"a\"]\n")
                .is_err(),
            "retry_on alone"
        );
    }

    #[test]
    fn evidence_strike_limit_defaults_to_one() {
        assert_eq!(StrikeLimits::default().limit(Reason::Evidence), 1);
        let cfg = roadmap_config_from_str("[limits.strikes]\nevidence = 3\n").unwrap();
        assert_eq!(cfg.limits.strikes.limit(Reason::Evidence), 3);
    }

    const FLAKY: &str = r#"
        kind = "flaky-test-fix"
        title = "t"
        fix_paths = ["tests/a.rs", "tests/common/stub.rs"]

        [[replace]]
        old = "a"
        new = "a_stable"

        [[evidence]]
        name = "flaky"
        cmd = "stress a"
        runs = 100
        min_pass = 1
        max_pass = 99

        [check]
        name = "stable"
        cmd = "stress a_stable"
        runs = 100
        min_pass = 100
    "#;

    #[test]
    fn flaky_test_fix_config_parses() {
        let cfg = task_config_from_str(FLAKY).unwrap();
        assert_eq!(cfg.kind, TaskKind::FlakyTestFix);
        assert_eq!(cfg.fix_paths.len(), 2);
        assert_eq!(cfg.replace[0].new, "a_stable");
        assert_eq!(cfg.evidence[0].max_pass, 99);
        assert_eq!(cfg.evidence[0].as_check().name, "evidence:flaky");
    }

    #[test]
    fn flaky_test_fix_config_is_validated() {
        let bad = |from: &str, to: &str| {
            assert!(
                task_config_from_str(&FLAKY.replace(from, to)).is_err(),
                "{from} -> {to}"
            )
        };
        bad("max_pass = 99", "max_pass = 100"); // never fails: not a flake
        bad(
            "min_pass = 1\n        max_pass",
            "min_pass = 5\n        max_pass = 4\n        #",
        ); // empty window
        bad(
            "fix_paths = [\"tests/a.rs\", \"tests/common/stub.rs\"]",
            "fix_paths = [\"tests/\"]",
        );
        bad(
            "fix_paths = [\"tests/a.rs\", \"tests/common/stub.rs\"]",
            "fix_paths = [\"tests/*.rs\"]",
        );
        bad(
            "fix_paths = [\"tests/a.rs\", \"tests/common/stub.rs\"]",
            "fix_paths = []",
        );
        bad("new = \"a_stable\"", "new = \"a\"");
        bad("[[evidence]]", "[[unused]]");
        // The new fields are rejected on any other kind.
        assert!(
            task_config_from_str(&FLAKY.replace("kind = \"flaky-test-fix\"", "kind = \"task\""))
                .is_err()
        );
    }

    #[test]
    fn task_config_with_check_validated() {
        let toml = r#"
            title = "Do the thing"

            [check]
            name = "t"
            cmd = "true"
            runs = 2
            min_pass = 3
        "#;
        assert!(task_config_from_str(toml).is_err());
    }
}
