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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_review_rounds: u32,
    pub strikes: StrikeLimits,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_review_rounds: 2, strikes: StrikeLimits::default() }
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
}

impl Default for StrikeLimits {
    fn default() -> Self {
        Self { history: 2, ownership: 2, pins: 2, suite: 2, plan: 2, post: 2, review: 2, accept: 2, retire: 2 }
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
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PlayerConfig {
    pub agent_cmd: String,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self { agent_cmd: "claude --dangerously-skip-permissions".to_string() }
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
}

impl Reason {
    pub const ALL: [Reason; 9] =
        [Reason::History, Reason::Ownership, Reason::Pins, Reason::Suite, Reason::Plan, Reason::Post, Reason::Review, Reason::Accept, Reason::Retire];

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
        Ok(())
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
}

impl TestsConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for c in &self.check {
            c.validate()?;
        }
        Ok(())
    }
}

// --------------------------------------------------------------------------
// tasks/<slug>/task.toml (spec §8.2)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TaskConfig {
    pub title: String,
    pub phase: String,
    pub milestone: bool,
    pub depends_on: Vec<String>,
    pub check: Option<Check>,
}

impl TaskConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(c) = &self.check {
            c.validate()?;
        }
        Ok(())
    }
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
