// flaky-test-fix helpers: reading a test inventory against a task's
// `[[replace]]` list, the evidence verdict text, and the subagent's extra
// prompt. Pure functions; the player does the running.

use crate::checks::CheckResult;
use crate::config::{Evidence, Replace};
use std::collections::BTreeSet;

/// One inventory line without the `: test` / `: benchmark` suffix `cargo
/// test -- --list` appends.
fn normalize(entry: &str) -> &str {
    entry.strip_suffix(": test").or_else(|| entry.strip_suffix(": benchmark")).unwrap_or(entry).trim()
}

/// Whether inventory `entry` is the test called `name`: the whole entry, or
/// its last `::` path segment (so `name` need not repeat the module path).
fn names_test(entry: &str, name: &str) -> bool {
    let entry = normalize(entry);
    entry == name || entry.ends_with(&format!("::{name}"))
}

/// What a finish's inventory breaks, as human-readable lines (empty when it
/// holds): every test on `base` must still be there unless it is an `old`
/// one, and every `new` test must exist. Inventories are compared as sets,
/// so a name reused across test binaries counts once.
pub fn inventory_problems(base: &[String], candidate: &[String], replace: &[Replace]) -> Vec<String> {
    let have: BTreeSet<&str> = candidate.iter().map(|e| normalize(e)).collect();
    let mut problems = Vec::new();

    let mut gone: Vec<&str> =
        base.iter().map(|e| normalize(e)).filter(|e| !have.contains(e) && !replace.iter().any(|r| names_test(e, &r.old))).collect();
    gone.sort_unstable();
    gone.dedup();
    for g in gone {
        problems.push(format!("test {g:?} was removed or renamed, but only the replaced tests may go"));
    }
    for r in replace {
        if !candidate.iter().any(|e| names_test(e, &r.new)) {
            problems.push(format!("replacement test {:?} is not in the test inventory", r.new));
        }
    }
    problems
}

/// `Some(text)` when `result` shows the evidence did not reproduce the
/// flake: `evidence did not reproduce (k/N): <name> needs a..=b passing runs`.
pub fn miss(evidence: &Evidence, result: &CheckResult) -> Option<String> {
    if result.pass {
        return None;
    }
    let passed = result.exits.iter().filter(|&&e| e == 0).count();
    Some(format!(
        "evidence did not reproduce ({passed}/{}): {:?} needs {}..={} passing runs of {}",
        result.runs, evidence.name, evidence.min_pass, evidence.max_pass, evidence.runs
    ))
}

/// The `{fix_paths}` and `{replacements}` lists of a flaky-test-fix
/// subagent's prompt (`subagent.flaky` in prompts.kdl).
pub fn prompt_lists(fix_paths: &[String], replace: &[Replace]) -> (String, String) {
    let files = fix_paths.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n");
    let tests = replace.iter().map(|r| format!("  - replace `{}` with a stable `{}`", r.old, r.new)).collect::<Vec<_>>().join("\n");
    (files, tests)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| format!("{n}: test")).collect()
    }

    fn replace(old: &str, new: &str) -> Vec<Replace> {
        vec![Replace { old: old.to_string(), new: new.to_string() }]
    }

    #[test]
    fn replacing_the_old_test_is_fine() {
        let base = inv(&["a::keep", "a::old_one"]);
        assert!(inventory_problems(&base, &inv(&["a::keep", "a::new_one"]), &replace("old_one", "new_one")).is_empty());
        // Leaving the old test in place is allowed too.
        assert!(inventory_problems(&base, &inv(&["a::keep", "a::old_one", "a::new_one"]), &replace("old_one", "new_one")).is_empty());
    }

    #[test]
    fn removing_another_test_or_missing_the_new_one_is_reported() {
        let base = inv(&["a::keep", "a::old_one"]);
        let problems = inventory_problems(&base, &inv(&["a::old_one"]), &replace("old_one", "new_one"));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("a::keep"));
        assert!(problems[1].contains("new_one"));
    }

    #[test]
    fn a_name_matches_whole_or_last_segment_only() {
        assert!(names_test("m::foo: test", "foo"));
        assert!(names_test("foo", "foo"));
        assert!(!names_test("m::foo_stable: test", "foo"));
        assert!(!names_test("m::xfoo: test", "foo"));
    }
}
