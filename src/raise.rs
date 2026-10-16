// Budget raises (architect-v3.md, "Budget increase path"): a director commit
// on `main` that increases a budget in `architecture.toml`. It must change
// nothing but budget fields (and no other file), carry a `raise: <reason>`
// (or `raise(<slug>): <reason>`, naming the task that hit the budget), and
// stay within the step cap. Lowering, turning a budget on, and any edit that
// raises nothing need none of that. Pure: the player (`validate_main_commit`)
// feeds it the two manifests and the commit message, and notes what it returns.

use crate::config::{self, ArchConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One raised budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Raise {
    /// `max_modules`, `module <name> max_ast`, `api <name> max_types`, ...
    pub budget: String,
    pub old: u64,
    pub new: u64,
    pub reason: String,
    /// The task whose work hit the budget, if the commit named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default)]
    pub at: u64,
}

/// Every budget in `cfg`, by display name. 0 means off.
pub fn budgets(cfg: &ArchConfig) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    out.insert("max_modules".to_string(), cfg.max_modules);
    for m in &cfg.modules {
        out.insert(format!("module {} max_ast", m.name), m.max_ast);
        out.insert(format!("module {} max_bytes", m.name), m.max_bytes);
    }
    for a in &cfg.apis {
        out.insert(format!("api {} max_constructors", a.name), a.max_constructors);
        out.insert(format!("api {} max_types", a.name), a.max_types);
    }
    out
}

/// The most a budget may be raised to in one commit: +25%, or +1 for a
/// small count. A larger jump takes several visible raises.
pub fn step_cap(old: u64) -> u64 {
    (old + 1).max(old + old / 4)
}

/// `(budget, old, new)` for every budget present in both whose limit went
/// up. Turning one off (new 0) is an unbounded raise; turning one on (old 0)
/// tightens, so it is not a raise.
pub fn raised(old: &ArchConfig, new: &ArchConfig) -> Vec<(String, u64, u64)> {
    let before = budgets(old);
    budgets(new)
        .into_iter()
        .filter_map(|(k, n)| {
            let o = *before.get(&k)?;
            (o > 0 && (n == 0 || n > o)).then_some((k, o, n))
        })
        .collect()
}

/// `manifest` with every budget field removed, to compare what else changed.
fn without_budgets(text: &str) -> Result<toml::Table, String> {
    let mut t: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    t.remove("max_modules");
    for (key, fields) in [
        ("module", &["max_ast", "max_bytes"][..]),
        ("api", &["max_constructors", "max_types"][..]),
    ] {
        if let Some(toml::Value::Array(items)) = t.get_mut(key) {
            for item in items {
                if let toml::Value::Table(tab) = item {
                    for f in fields {
                        tab.remove(*f);
                    }
                }
            }
        }
    }
    Ok(t)
}

/// `raise: why` / `raise(slug): why` in a commit message: `(task, reason)`.
pub fn parse_reason(subject: &str, body: &str) -> Option<(Option<String>, String)> {
    let rest = subject.strip_prefix("raise")?;
    let (task, rest) = match rest.strip_prefix('(') {
        Some(r) => {
            let (slug, r) = r.split_once(')')?;
            (Some(slug.trim().to_string()).filter(|s| !s.is_empty()), r)
        }
        None => (None, rest),
    };
    let first = rest.strip_prefix(':')?.trim();
    let reason = format!("{first}\n{}", body.trim()).trim().to_string();
    (!reason.is_empty()).then_some((task, reason))
}

/// Judges a main commit that changes `architecture.toml` from `old` to `new`
/// (`None`: absent), touching `files`. `Ok` holds the raises to record
/// (empty when it raises nothing); `Err` is the `ownership` rejection text.
pub fn judge(
    old: Option<&str>,
    new: &str,
    subject: &str,
    body: &str,
    files: &[String],
    at: u64,
) -> Result<Vec<Raise>, String> {
    let (Some(old_text), Ok(new_cfg)) = (old, config::arch_config_from_str(new)) else {
        return Ok(Vec::new());
    };
    let Ok(old_cfg) = config::arch_config_from_str(old_text) else {
        return Ok(Vec::new());
    };
    let ups = raised(&old_cfg, &new_cfg);
    if ups.is_empty() {
        return Ok(Vec::new());
    }
    let names: Vec<&str> = ups.iter().map(|(k, _, _)| k.as_str()).collect();
    let (task, reason) = parse_reason(subject, body).ok_or_else(|| {
        format!(
            "ownership: raising {} needs a `raise: <reason>` (or `raise(<slug>): <reason>`) commit message, and the raise discussed with the operator",
            names.join(", ")
        )
    })?;
    if let Some(other) = files.iter().find(|f| *f != config::ARCH_FILE) {
        return Err(format!(
            "ownership: a budget raise changes only {}, not {other:?}",
            config::ARCH_FILE
        ));
    }
    if without_budgets(old_text)? != without_budgets(new)? {
        return Err(format!(
            "ownership: a budget raise changes only budget fields in {}; make the other edits in their own commit",
            config::ARCH_FILE
        ));
    }
    let mut out = Vec::new();
    for (budget, o, n) in ups {
        let cap = step_cap(o);
        if n == 0 || n > cap {
            return Err(format!(
                "ownership: {budget}: {o} -> {} is past the step cap ({cap}: +25%, or +1); raise it in several visible steps",
                if n == 0 { "off".to_string() } else { n.to_string() }
            ));
        }
        out.push(Raise { budget, old: o, new: n, reason: reason.clone(), task: task.clone(), at });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "max_modules = 4\n[[module]]\nname = \"a\"\nlang = \"rust\"\npaths = [\"a\"]\nmax_ast = 100\nmax_bytes = 1000\n[[module]]\nname = \"b\"\nlang = \"rust\"\npaths = [\"b\"]\n[[api]]\nname = \"ab\"\ngenerator = \"a\"\nconsumer = \"b\"\npaths = [\"api\"]\nmax_constructors = 3\n";
    const ARCH: &[&str] = &["architecture.toml"];

    fn files(f: &[&str]) -> Vec<String> {
        f.iter().map(|s| s.to_string()).collect()
    }

    fn go(new: &str, subject: &str, f: &[&str]) -> Result<Vec<Raise>, String> {
        judge(Some(OLD), new, subject, "", &files(f), 7)
    }

    #[test]
    fn step_cap_is_a_quarter_or_one() {
        assert_eq!((step_cap(1), step_cap(3), step_cap(4), step_cap(100)), (2, 4, 5, 125));
    }

    #[test]
    fn lowering_turning_on_and_non_budget_edits_need_nothing() {
        assert_eq!(go(&OLD.replace("= 100", "= 50"), "tighten", ARCH).unwrap(), vec![]);
        // b's max_ast was off (0): turning it on is not a raise.
        let on = OLD.replace("paths = [\"b\"]\n", "paths = [\"b\"]\nmax_ast = 9\n");
        assert_eq!(go(&on, "budget b", ARCH).unwrap(), vec![]);
        let other = OLD.replace("paths = [\"a\"]", "paths = [\"a\", \"a2\"]");
        assert_eq!(go(&other, "more files", &["architecture.toml", "a2/x.rs"]).unwrap(), vec![]);
    }

    #[test]
    fn a_raise_needs_a_reason_the_manifest_alone_and_a_small_step() {
        let up = OLD.replace("max_ast = 100", "max_ast = 120");
        assert!(go(&up, "bump", ARCH).unwrap_err().contains("raise: <reason>"));
        let r = go(&up, "raise(parser): the parser grew", ARCH).unwrap();
        assert_eq!((r[0].budget.as_str(), r[0].old, r[0].new), ("module a max_ast", 100, 120));
        assert_eq!((r[0].task.as_deref(), r[0].reason.as_str(), r[0].at), (Some("parser"), "the parser grew", 7));
        assert!(go(&up, "raise: x", &["architecture.toml", "src/x.rs"]).unwrap_err().contains("src/x.rs"));
        let mixed = up.replace("paths = [\"a\"]", "paths = [\"a\", \"c\"]");
        assert!(go(&mixed, "raise: x", ARCH).unwrap_err().contains("only budget fields"));
        let big = OLD.replace("max_ast = 100", "max_ast = 126");
        assert!(go(&big, "raise: x", ARCH).unwrap_err().contains("step cap (125"));
        let off = OLD.replace("max_ast = 100\n", "");
        assert!(go(&off, "raise: x", ARCH).unwrap_err().contains("-> off"));
        let small = OLD.replace("max_constructors = 3", "max_constructors = 5");
        assert!(go(&small, "raise: x", ARCH).unwrap_err().contains("step cap (4"));
    }

    #[test]
    fn parse_reason_forms() {
        assert_eq!(parse_reason("raise: why", "more"), Some((None, "why\nmore".to_string())));
        assert_eq!(parse_reason("raise(t1): why", ""), Some((Some("t1".to_string()), "why".to_string())));
        assert_eq!(parse_reason("raise:", ""), None);
        assert_eq!(parse_reason("raised: x", ""), None);
        assert_eq!(parse_reason("fix", ""), None);
    }
}
