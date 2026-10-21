// test-speedup (spec §15.1): the pure parts. Is a finish within its scope
// (only the checks' `cmd`s and the test tree may change), does it beat the
// ratchet against `main`'s recorded timings, and what did it remove. The
// player does the running and the ref work.

use crate::checks::{self, SuiteResult};
use crate::config::{BudgetConfig, Check, Suite};
use std::collections::BTreeMap;

/// A check is slow on `main` for a test-speedup's evidence if it is killed
/// for its timeout, or its recorded wall time is at least this percentage
/// of `[budget] suite_fail`. A constant, like the tolerance.
pub const SLOW_SHARE_PERCENT: u64 = 10;

/// What a finish changes in the suite definition beyond what a test-speedup
/// may: only a check's `cmd`, `retry_failed` and `retry_on` (and the test
/// tree itself, which the path check covers) are its to change. Everything
/// that would let it widen or reshape the suite is a problem.
pub(crate) fn scope_problems(base: &Suite, cand: &Suite) -> Vec<String> {
    let mut problems = Vec::new();
    if base.test_paths != cand.test_paths {
        problems.push("test_paths changed".to_string());
    }
    if base.list_cmd != cand.list_cmd {
        problems.push("list_cmd changed".to_string());
    }
    let by_name = |cfg: &Suite| -> BTreeMap<String, Check> {
        cfg.checks.iter().map(|c| (c.name.clone(), c.clone())).collect()
    };
    let (before, after) = (by_name(base), by_name(cand));
    for name in before.keys().filter(|n| !after.contains_key(*n)) {
        problems.push(format!("check {name:?} was removed"));
    }
    for name in after.keys().filter(|n| !before.contains_key(*n)) {
        problems.push(format!("check {name:?} was added"));
    }
    for (name, old) in &before {
        if let Some(new) = after.get(name) {
            if old.runs != new.runs || old.min_pass != new.min_pass {
                problems.push(format!("check {name:?}: runs / min_pass changed"));
            }
            if old.covers != new.covers || old.all != new.all || old.files != new.files {
                problems.push(format!("check {name:?}: covers / files changed"));
            }
        }
    }
    problems
}

/// `name: `old` -> `new`` for every check whose `cmd` or retry policy the
/// finish changed: a `cmd` turned into `true` removes a test the inventory
/// diff can't see, so the director is shown it.
pub(crate) fn changed_checks(base: &Suite, cand: &Suite) -> Vec<String> {
    let mut out = Vec::new();
    for new in &cand.checks {
        let Some(old) = base.checks.iter().find(|c| c.name == new.name) else {
            continue;
        };
        if old.cmd != new.cmd {
            out.push(format!("{}: `{}` -> `{}`", new.name, old.cmd, new.cmd));
        }
        if old.retry_failed != new.retry_failed || old.retry_on != new.retry_on {
            out.push(format!(
                "{}: retry_failed {} -> {}, retry_on {:?} -> {:?}",
                new.name, old.retry_failed, new.retry_failed, old.retry_on, new.retry_on
            ));
        }
    }
    out
}

/// Why a finish that is still over budget isn't progress, judged against
/// `main`'s recorded `main_millis` per check (empty: it is). Every named
/// check strictly faster and not killed; no other inherited check slower
/// beyond `checks::within_tolerance`; the total no higher than main's; a
/// check `main` lacks fits `suite_fail` on its own.
pub fn ratchet_problems(
    named: &[String],
    main_millis: &BTreeMap<String, u64>,
    budget: &BudgetConfig,
    suite: &SuiteResult,
) -> Vec<String> {
    let mut problems = Vec::new();
    for name in named {
        match suite.checks.iter().find(|c| &c.name == name) {
            None => problems.push(format!("named check {name:?} is not in the suite")),
            Some(c) if c.timed_out => {
                problems.push(format!("named check {name:?} still gets killed"))
            }
            Some(c) => {
                if let Some(m) = main_millis.get(name) {
                    if c.millis >= *m {
                        problems.push(format!(
                            "named check {name:?} is not faster: {} against {} on main",
                            checks::fmt_secs(c.millis),
                            checks::fmt_secs(*m)
                        ));
                    }
                }
            }
        }
    }
    for c in &suite.checks {
        match main_millis.get(&c.name) {
            Some(m) => {
                if !named.contains(&c.name) && !checks::within_tolerance(c.millis, *m) {
                    problems.push(format!(
                        "check {:?} got slower: {} against {} on main",
                        c.name,
                        checks::fmt_secs(c.millis),
                        checks::fmt_secs(*m)
                    ));
                }
            }
            None => {
                if budget.suite_fail > 0 && c.millis > budget.suite_fail * 1000 {
                    problems.push(format!(
                        "new check {:?} takes {}, past suite_fail ({}s) on its own",
                        c.name,
                        checks::fmt_secs(c.millis),
                        budget.suite_fail
                    ));
                }
            }
        }
    }
    let main_total: u64 = main_millis.values().sum();
    if suite.millis() > main_total {
        problems.push(format!(
            "the suite takes {} against {} on main",
            checks::fmt_secs(suite.millis()),
            checks::fmt_secs(main_total)
        ));
    }
    problems
}

/// The finish event's text (and the review prompt's `{notes}`): what the
/// speed-up gave up, so the director decides at `accept`. Empty when it
/// removed and rewired nothing.
pub fn removal_text(removed: &[String], changed: &[String]) -> String {
    let mut lines = Vec::new();
    if !removed.is_empty() {
        lines.push(format!(
            "removed {} tests: {}",
            removed.len(),
            removed.join(", ")
        ));
    }
    if !changed.is_empty() {
        lines.push(format!("changed checks: {}", changed.join("; ")));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::CheckResult;

    fn cfg(checks: &[(&str, &str, u32)]) -> Suite {
        Suite {
            checks: checks
                .iter()
                .map(|(n, c, runs)| Check {
                    name: n.to_string(),
                    cmd: c.to_string(),
                    runs: *runs,
                    ..Check::default()
                })
                .collect(),
            test_paths: vec!["tests/".into()],
            list_cmd: Some("ls".into()),
        }
    }

    fn suite(rows: &[(&str, u64)]) -> SuiteResult {
        SuiteResult {
            pass: true,
            error: None,
            checks: rows
                .iter()
                .map(|(n, ms)| CheckResult {
                    name: n.to_string(),
                    millis: *ms,
                    ..Default::default()
                })
                .collect(),
        }
    }

    fn main_ms(rows: &[(&str, u64)]) -> BTreeMap<String, u64> {
        rows.iter().map(|(n, m)| (n.to_string(), *m)).collect()
    }

    #[test]
    fn only_cmd_may_change() {
        let base = cfg(&[("a", "x", 1)]);
        assert!(scope_problems(&base, &cfg(&[("a", "y", 1)])).is_empty());
        assert_eq!(scope_problems(&base, &cfg(&[("a", "x", 3)])).len(), 1);
        assert_eq!(scope_problems(&base, &cfg(&[("b", "x", 1)])).len(), 2);
        let mut covers = cfg(&[("a", "x", 1)]);
        covers.checks[0].all = true;
        assert_eq!(scope_problems(&base, &covers).len(), 1);
        let mut moved = cfg(&[("a", "x", 1)]);
        moved.test_paths.clear();
        assert_eq!(scope_problems(&base, &moved).len(), 1);
        assert_eq!(
            changed_checks(&base, &cfg(&[("a", "y", 1)])),
            vec!["a: `x` -> `y`".to_string()]
        );
    }

    #[test]
    fn ratchet_edges() {
        let budget = BudgetConfig {
            check_timeout: 0,
            suite_warn: 0,
            suite_fail: 100,
        };
        let main = main_ms(&[("slow", 90_000), ("other", 40_000)]);
        let named = vec!["slow".to_string()];
        let ok = suite(&[("slow", 50_000), ("other", 40_000)]);
        assert!(ratchet_problems(&named, &main, &budget, &ok).is_empty());
        // not strictly faster
        let same = suite(&[("slow", 90_000), ("other", 30_000)]);
        assert_eq!(ratchet_problems(&named, &main, &budget, &same).len(), 1);
        // an inherited check beyond tolerance (the total is still under main's)
        let slower = suite(&[("slow", 50_000), ("other", 60_000)]);
        assert_eq!(ratchet_problems(&named, &main, &budget, &slower).len(), 1);
        // within tolerance
        let jitter = suite(&[("slow", 50_000), ("other", 48_000)]);
        assert!(ratchet_problems(&named, &main, &budget, &jitter).is_empty());
        // total above main's, every check within tolerance
        let main2 = main_ms(&[("slow", 90_000), ("other", 10_000)]);
        let total = suite(&[("slow", 89_000), ("other", 12_000)]);
        assert_eq!(ratchet_problems(&named, &main2, &budget, &total).len(), 1);
        // a new check too slow on its own
        let new = suite(&[("slow", 10_000), ("other", 40_000), ("fresh", 101_000)]);
        assert_eq!(ratchet_problems(&named, &main, &budget, &new).len(), 2);
        // a named check still killed
        let killed = suite(&[("slow", 10_000), ("other", 40_000)]);
        let mut killed = killed;
        killed.checks[0].timed_out = true;
        assert_eq!(ratchet_problems(&named, &main, &budget, &killed).len(), 1);
    }

    #[test]
    fn removal_text_lists_both() {
        assert_eq!(removal_text(&[], &[]), "");
        let t = removal_text(&["a".into(), "b".into()], &["c: `x` -> `y`".into()]);
        assert!(t.contains("removed 2 tests: a, b") && t.contains("changed checks: c"));
    }
}
