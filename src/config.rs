// roadmap.toml / tests.toml / task.toml schemas (spec.md §6.2, §8.2, §10),
// with serde defaults so every section and field is optional.

use serde::{Deserialize, Serialize};
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
    /// How many times a subagent may contest the tests it must pass
    /// (spec §18); one more escalates.
    pub max_contest_rounds: u32,
    pub strikes: StrikeLimits,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_review_rounds: 2, max_contest_rounds: 2, strikes: StrikeLimits::default() }
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
}

impl Default for StrikeLimits {
    fn default() -> Self {
        Self { history: 2, ownership: 2, pins: 2, suite: 2, plan: 2, post: 2, review: 2, accept: 2, retire: 2, claims: 2, evidence: 1 }
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
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PlayerConfig {
    /// Launches a task's implementing subagent (spec §9's launch row).
    pub agent_cmd: String,
    /// Launches a task's task agent, the director's delegate that posts and
    /// reviews it (spec §17). Empty turns task agents off: the director
    /// posts and sends back itself.
    pub task_agent_cmd: String,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self { agent_cmd: "claude --dangerously-skip-permissions".to_string(), task_agent_cmd: "claude --dangerously-skip-permissions".to_string() }
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
}

impl Reason {
    pub const ALL: [Reason; 11] = [
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
            return Err(format!("check {:?}: runs must be >= 1 (got {})", self.name, self.runs));
        }
        if self.min_pass < 1 || self.min_pass > self.runs {
            return Err(format!("check {:?}: min_pass must be between 1 and runs ({}), got {}", self.name, self.runs, self.min_pass));
        }
        for pattern in &self.retry_on {
            regex::Regex::new(pattern).map_err(|e| format!("check {:?}: retry_on {pattern:?}: {e}", self.name))?;
        }
        if !self.retry_on.is_empty() && self.retry_failed == 0 {
            return Err(format!("check {:?}: retry_on needs retry_failed >= 1", self.name));
        }
        Ok(())
    }

    /// A copy that never retries.
    pub fn without_retries(&self) -> Check {
        Check { retry_failed: 0, retry_on: Vec::new(), ..self.clone() }
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
    path.strip_prefix(SUITE_DIR).and_then(|rest| rest.strip_prefix('/')).is_some_and(|name| !name.contains('/') && name.ends_with(".toml"))
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
pub(crate) fn suite_from_parts(base: &str, fragments: &[(String, String)]) -> Result<TestsConfig, String> {
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
            return Err(format!("check name {:?} is declared more than once across tests.toml and tests.d/", c.name));
        }
    }
    Ok(cfg)
}

/// The project suite at `rev`: `tests.toml` plus every `tests.d/*.toml`.
pub(crate) fn suite_at(git: &crate::git::Git, rev: &str) -> Result<TestsConfig, String> {
    let base = git.show_file(rev, "tests.toml").ok_or_else(|| format!("tests.toml not found at {rev}"))?;
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
        Check { name: format!("evidence:{}", self.name), cmd: self.cmd.clone(), runs: self.runs, min_pass: self.min_pass, retry_failed: 0, retry_on: Vec::new() }
    }

    fn validate(&self) -> Result<(), String> {
        self.as_check().validate().map_err(|e| e.replacen("check", "evidence", 1))?;
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
}

impl TaskConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(c) = &self.check {
            c.validate()?;
        }
        for e in &self.evidence {
            e.validate()?;
        }
        if self.kind != TaskKind::FlakyTestFix {
            if !self.fix_paths.is_empty() || !self.replace.is_empty() || !self.evidence.is_empty() {
                return Err("fix_paths, [[replace]] and [[evidence]] are only for kind = \"flaky-test-fix\"".to_string());
            }
        } else {
            if self.check.is_none() {
                return Err("a flaky-test-fix needs a [check] proving the replacement is stable".to_string());
            }
            if self.replace.is_empty() {
                return Err("a flaky-test-fix needs at least one [[replace]]".to_string());
            }
            for r in &self.replace {
                if r.old.trim().is_empty() || r.new.trim().is_empty() || r.old == r.new {
                    return Err(format!("replace {:?} -> {:?}: old and new must be different, non-empty test names", r.old, r.new));
                }
            }
            if self.evidence.is_empty() {
                return Err("a flaky-test-fix needs at least one [[evidence]] showing the flake on main".to_string());
            }
            if self.fix_paths.is_empty() {
                return Err("a flaky-test-fix needs fix_paths: the files the subagent may change".to_string());
            }
            for p in &self.fix_paths {
                if p.trim().is_empty() || p.contains(['*', '?', '[']) || p.ends_with('/') {
                    return Err(format!("fix_paths entry {p:?} must be a plain file path, never a directory or a wildcard"));
                }
                if p == "tests.toml" || p.starts_with("tasks/") {
                    return Err(format!("fix_paths entry {p:?} may not be tests.toml or under tasks/"));
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
        Check { name: format!("claim:{}", self.name), cmd: self.cmd.clone(), runs: self.runs, min_pass: self.min_pass, retry_failed: 0, retry_on: Vec::new() }
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("a claim needs a name".to_string());
        }
        if self.cmd.trim().is_empty() {
            return Err(format!("claim {:?}: cmd must not be empty", self.name));
        }
        if self.runs < 1 || self.min_pass < 1 || self.min_pass > self.runs {
            return Err(format!("claim {:?}: min_pass must be between 1 and runs ({}), got {}", self.name, self.runs, self.min_pass));
        }
        if self.timeout == Some(0) {
            return Err(format!("claim {:?}: timeout must be at least 1 second", self.name));
        }
        Ok(())
    }
}

/// `tasks/<slug>/claims/<n>.toml`, written by the subagent: its answer to the
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
        return Err("answer must be set (one of the question's answers, or \"inconclusive\")".to_string());
    }
    Ok(f)
}

pub(crate) fn baseline_file_from_str(s: &str) -> Result<BaselineFile, String> {
    let mut f: BaselineFile = toml::from_str(s).map_err(|e| e.to_string())?;
    validate_claim_list(&f.claim)?;
    for c in &mut f.claim {
        if c.kind != ClaimKind::Baseline {
            return Err(format!("claim {:?}: baseline/<n>.toml only holds baseline claims", c.name));
        }
        if c.supports.is_some() {
            return Err(format!("claim {:?}: a baseline claim backs no answer (drop `supports`)", c.name));
        }
    }
    Ok(f)
}

// --------------------------------------------------------------------------
// Loading helpers
// --------------------------------------------------------------------------

pub(crate) fn roadmap_config_from_str(s: &str) -> Result<RoadmapConfig, String> {
    toml::from_str(s).map_err(|e| e.to_string())
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
        assert_eq!(cfg.limits.strikes.history, 2);
        assert_eq!(cfg.limits.strikes.suite, 2);
        assert_eq!(cfg.player.agent_cmd, "claude --dangerously-skip-permissions");
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

            [limits.strikes]
            suite = 5
            pins = 1

            [player]
            agent_cmd = "codex run"
        "#;
        let cfg = roadmap_config_from_str(toml).unwrap();
        assert_eq!(cfg.title.as_deref(), Some("My Project"));
        assert_eq!(cfg.docs_paths, vec!["docs/adr".to_string()]);
        assert_eq!(cfg.phases.len(), 1);
        assert_eq!(cfg.phases[0].id, "p1");
        assert_eq!(cfg.phases[0].title.as_deref(), Some("Phase One"));
        assert_eq!(cfg.limits.max_review_rounds, 3);
        assert_eq!(cfg.limits.strikes.suite, 5);
        assert_eq!(cfg.limits.strikes.pins, 1);
        // Untouched strike reasons keep their default.
        assert_eq!(cfg.limits.strikes.history, 2);
        assert_eq!(cfg.player.agent_cmd, "codex run");
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
            ("tests.d/b.toml".to_string(), "[[check]]\nname = \"b\"\ncmd = \"true\"\n".to_string()),
            ("tests.d/a.toml".to_string(), "[[check]]\nname = \"a\"\ncmd = \"true\"\n".to_string()),
        ];
        let cfg = suite_from_parts(base, &frags).unwrap();
        let names: Vec<&str> = cfg.check.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["unit", "a", "b"]);
        assert_eq!(cfg.test_paths, ["tests/"]);
    }

    #[test]
    fn suite_rejects_duplicate_names_and_bad_fragments() {
        let base = "[[check]]\nname = \"unit\"\ncmd = \"true\"\n";
        let dup = vec![("tests.d/a.toml".to_string(), "[[check]]\nname = \"unit\"\ncmd = \"false\"\n".to_string())];
        assert!(suite_from_parts(base, &dup).unwrap_err().contains("more than once"));
        let bad = vec![("tests.d/a.toml".to_string(), "[[check]]\nname = \"x\"\ncmd = \"true\"\nruns = 0\n".to_string())];
        assert!(suite_from_parts(base, &bad).unwrap_err().starts_with("tests.d/a.toml"));
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
        assert_eq!(task_config_from_str("title = \"t\"\n").unwrap().kind, TaskKind::Task);
        assert!(task_config_from_str("kind = \"spike\"\ndepends_on = [\"a\"]\n").is_err());
        assert!(task_config_from_str("kind = \"spike\"\nmilestone = true\n").is_err());
        assert!(task_config_from_str("kind = \"spike\"\n[check]\nname = \"c\"\ncmd = \"true\"\n").is_err());
    }

    #[test]
    fn question_parses_and_validates() {
        let q = question_from_str("question = \"Can we?\"\nanswers = [\"yes\", \"no\"]\nrefines = 1\n").unwrap();
        assert_eq!(q.refines, Some(1));
        assert!(q.allows("yes"));
        assert!(q.allows("inconclusive"));
        assert!(!q.allows("maybe"));
        assert!(question_from_str("answers = [\"yes\"]\n").is_err(), "empty question");
        assert!(question_from_str("question = \"q\"\n").is_err(), "no answers");
        assert!(question_from_str("question = \"q\"\nanswers = [\"a\", \"a\"]\n").is_err(), "duplicate answers");
    }

    #[test]
    fn claims_file_needs_an_answer_and_valid_claims() {
        let f = claims_file_from_str("answer = \"yes\"\n\n[[claim]]\nname = \"c\"\nkind = \"spike\"\nsupports = \"yes\"\ncmd = \"true\"\n").unwrap();
        assert_eq!(f.claim[0].kind, ClaimKind::Spike);
        assert_eq!(f.claim[0].timeout_secs(), DEFAULT_CLAIM_TIMEOUT_SECS);
        assert_eq!(f.claim[0].as_check().name, "claim:c");
        assert!(claims_file_from_str("[[claim]]\nname = \"c\"\ncmd = \"true\"\n").is_err(), "no answer");
        assert!(claims_file_from_str("answer = \"yes\"\n[[claim]]\nname = \"c\"\ncmd = \"\"\n").is_err(), "empty cmd");
        assert!(claims_file_from_str("answer = \"yes\"\n[[claim]]\nname = \"c\"\ncmd = \"x\"\nruns = 2\nmin_pass = 3\n").is_err());
    }

    #[test]
    fn baseline_file_only_holds_baseline_claims() {
        assert!(baseline_file_from_str("[[claim]]\nname = \"c\"\ncmd = \"true\"\n").is_ok());
        assert!(baseline_file_from_str("[[claim]]\nname = \"c\"\ncmd = \"true\"\nkind = \"spike\"\n").is_err());
        assert!(baseline_file_from_str("[[claim]]\nname = \"c\"\ncmd = \"true\"\nsupports = \"yes\"\n").is_err());
    }

    #[test]
    fn retry_policy_is_parsed_and_validated() {
        let ok = tests_config_from_str("[[check]]\nname = \"u\"\ncmd = \"x\"\nretry_failed = 2\nretry_on = [\"Text file busy\", \"os error \\\\d+\"]\n").unwrap();
        assert_eq!(ok.check[0].retry_failed, 2);
        assert_eq!(ok.check[0].retry_on.len(), 2);
        assert_eq!(tests_config_from_str("[[check]]\nname = \"u\"\ncmd = \"x\"\n").unwrap().check[0].retry_failed, 0);
        assert!(tests_config_from_str("[[check]]\nname = \"u\"\ncmd = \"x\"\nretry_failed = 1\nretry_on = [\"(\"]\n").is_err(), "bad regex");
        assert!(tests_config_from_str("[[check]]\nname = \"u\"\ncmd = \"x\"\nretry_on = [\"a\"]\n").is_err(), "retry_on alone");
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
        let bad = |from: &str, to: &str| assert!(task_config_from_str(&FLAKY.replace(from, to)).is_err(), "{from} -> {to}");
        bad("max_pass = 99", "max_pass = 100"); // never fails: not a flake
        bad("min_pass = 1\n        max_pass", "min_pass = 5\n        max_pass = 4\n        #"); // empty window
        bad("fix_paths = [\"tests/a.rs\", \"tests/common/stub.rs\"]", "fix_paths = [\"tests/\"]");
        bad("fix_paths = [\"tests/a.rs\", \"tests/common/stub.rs\"]", "fix_paths = [\"tests/*.rs\"]");
        bad("fix_paths = [\"tests/a.rs\", \"tests/common/stub.rs\"]", "fix_paths = []");
        bad("new = \"a_stable\"", "new = \"a\"");
        bad("[[evidence]]", "[[unused]]");
        // The new fields are rejected on any other kind.
        assert!(task_config_from_str(&FLAKY.replace("kind = \"flaky-test-fix\"", "kind = \"task\"")).is_err());
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
