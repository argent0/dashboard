// Budget raises (architect-v3.md, "Budget increase path"): a director commit
// on `main` that increases a budget in the architecture (the Lean package
// under `architecture/`). It must change nothing but
// budget fields (and no other file), carry a `raise: <reason>` (or
// `raise(<slug>): <reason>`, naming the task that hit the budget), and stay
// within the step cap. Lowering, turning a budget on, and any edit that
// raises nothing need none of that. Pure: the player (`validate_main_commit`)
// feeds it the two architectures and the commit message, and notes what it
// returns.

use crate::config::{self, ArchConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

/// An architecture as the raise judge sees it: the parsed manifest and the
/// Lean export it came from, for the "only budgets changed" comparison.
#[derive(Debug, Clone)]
pub struct Source {
    pub cfg: ArchConfig,
    pub doc: Value,
}

impl Source {
    /// From a Lean export.
    pub fn from_export(e: &crate::lean::Exported) -> Source {
        Source { cfg: e.arch.clone(), doc: e.json.clone() }
    }
}

const MODULE_BUDGETS: [&str; 2] = ["max_ast", "max_bytes"];
const API_BUDGETS: [&str; 10] = [
    "max_constructors",
    "max_types",
    "max_elements",
    "max_fields",
    "max_fns",
    "max_inputs",
    "max_string_len",
    "max_bytes_len",
    "max_rules",
    "max_tokens",
];

/// Every budget in `cfg`, by display name. 0 means off.
pub fn budgets(cfg: &ArchConfig) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    out.insert("max_modules".to_string(), cfg.max_modules);
    out.insert("max_roots".to_string(), cfg.max_roots);
    for m in &cfg.modules {
        out.insert(format!("module {} max_ast", m.name), m.max_ast);
        out.insert(format!("module {} max_bytes", m.name), m.max_bytes);
    }
    for r in &cfg.roots {
        out.insert(format!("root {} max_ast", r.name), r.max_ast);
        out.insert(format!("root {} max_bytes", r.name), r.max_bytes);
    }
    for a in &cfg.apis {
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
            out.insert(format!("api {} {k}", a.name), v);
        }
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

/// `doc` with every budget field removed, to compare what else changed.
/// Works on a Lean export (`{"arch": ...}`).
fn without_budgets(doc: &Value) -> Value {
    let mut v = doc.clone();
    if let Some(obj) = v.get_mut("arch").and_then(Value::as_object_mut) {
        obj.remove("max_modules");
        obj.remove("max_roots");
        for (key, fields) in [("module", &MODULE_BUDGETS[..]), ("root", &MODULE_BUDGETS[..]), ("api", &API_BUDGETS[..])] {
            if let Some(Value::Array(items)) = obj.get_mut(key) {
                for item in items {
                    if let Some(tab) = item.as_object_mut() {
                        for f in fields {
                            tab.remove(*f);
                        }
                    }
                }
            }
        }
    }
    v
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

/// Judges a main commit that changes the architecture from `old` to `new`
/// (`None`: absent), touching `files`. `Ok` holds the raises to record
/// (empty when it raises nothing); `Err` is the `ownership` rejection text.
pub fn judge(
    old: Option<&Source>,
    new: &Source,
    subject: &str,
    body: &str,
    files: &[String],
    at: u64,
) -> Result<Vec<Raise>, String> {
    let Some(old) = old else {
        return Ok(Vec::new());
    };
    let ups = raised(&old.cfg, &new.cfg);
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
    let what = config::ARCH_DIR.to_string() + "/";
    if let Some(other) = files.iter().find(|f| !f.starts_with("architecture/")) {
        return Err(format!("ownership: a budget raise changes only {what}, not {other:?}"));
    }
    if without_budgets(&old.doc) != without_budgets(&new.doc) {
        return Err(format!(
            "ownership: a budget raise changes only budget fields in {what}; make the other edits in their own commit"
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

    use serde_json::json;

    fn old() -> Value {
        json!({
            "max_modules": 4,
            "module": [
                {"name": "a", "lang": "rust", "paths": ["a"], "max_ast": 100, "max_bytes": 1000},
                {"name": "b", "lang": "rust", "paths": ["b"]}
            ],
            "api": [{"name": "ab", "generator": "a", "consumer": "b", "paths": ["api"], "max_constructors": 3}]
        })
    }
    const ARCH: &[&str] = &["architecture/Architecture.lean"];

    fn files(f: &[&str]) -> Vec<String> {
        f.iter().map(|s| s.to_string()).collect()
    }

    fn source(arch: &Value) -> Source {
        let idls = json!({"ab": {"api": "ab", "ctors": [{"name": "X"}]}});
        let text = json!({"arch": arch, "idls": idls, "errors": []}).to_string();
        Source::from_export(&crate::lean::parse(&text).unwrap())
    }

    fn go(new: &Value, subject: &str, f: &[&str]) -> Result<Vec<Raise>, String> {
        judge(Some(&source(&old())), &source(new), subject, "", &files(f), 7)
    }

    /// `old()` with `edit` applied.
    fn with(edit: impl FnOnce(&mut Value)) -> Value {
        let mut v = old();
        edit(&mut v);
        v
    }

    #[test]
    fn step_cap_is_a_quarter_or_one() {
        assert_eq!((step_cap(1), step_cap(3), step_cap(4), step_cap(100)), (2, 4, 5, 125));
    }

    #[test]
    fn lowering_turning_on_and_non_budget_edits_need_nothing() {
        assert_eq!(go(&with(|v| v["module"][0]["max_ast"] = json!(50)), "tighten", ARCH).unwrap(), vec![]);
        // b's max_ast was off (0): turning it on is not a raise.
        assert_eq!(go(&with(|v| v["module"][1]["max_ast"] = json!(9)), "budget b", ARCH).unwrap(), vec![]);
        let other = with(|v| v["module"][0]["paths"] = json!(["a", "a2"]));
        assert_eq!(go(&other, "more files", &["architecture/Architecture.lean", "a2/x.rs"]).unwrap(), vec![]);
    }

    #[test]
    fn a_raise_needs_a_reason_the_manifest_alone_and_a_small_step() {
        let up = with(|v| v["module"][0]["max_ast"] = json!(120));
        assert!(go(&up, "bump", ARCH).unwrap_err().contains("raise: <reason>"));
        let r = go(&up, "raise(parser): the parser grew", ARCH).unwrap();
        assert_eq!((r[0].budget.as_str(), r[0].old, r[0].new), ("module a max_ast", 100, 120));
        assert_eq!((r[0].task.as_deref(), r[0].reason.as_str(), r[0].at), (Some("parser"), "the parser grew", 7));
        assert!(go(&up, "raise: x", &["architecture/Architecture.lean", "src/x.rs"]).unwrap_err().contains("src/x.rs"));
        let mut mixed = up.clone();
        mixed["module"][0]["paths"] = json!(["a", "c"]);
        assert!(go(&mixed, "raise: x", ARCH).unwrap_err().contains("only budget fields"));
        let big = with(|v| v["module"][0]["max_ast"] = json!(126));
        assert!(go(&big, "raise: x", ARCH).unwrap_err().contains("step cap (125"));
        let off = with(|v| {
            v["module"][0].as_object_mut().unwrap().remove("max_ast");
        });
        assert!(go(&off, "raise: x", ARCH).unwrap_err().contains("-> off"));
        let small = with(|v| v["api"][0]["max_constructors"] = json!(5));
        assert!(go(&small, "raise: x", ARCH).unwrap_err().contains("step cap (4"));
    }

    #[test]
    fn a_lean_raise_is_judged_on_the_export() {
        let doc = |max_ast: u64, extra: &str| {
            let text = format!(
                r#"{{"arch": {{"max_modules": 4, "module": [{{"name": "a", "lang": "rust", "paths": ["a"{extra}], "max_ast": {max_ast}}}]}},
                    "idls": {{}}, "errors": []}}"#
            );
            Source::from_export(&crate::lean::parse(&text).unwrap())
        };
        let old = doc(100, "");
        let arch = |f: &[&str]| files(f);
        let r = judge(Some(&old), &doc(120, ""), "raise: grew", "", &arch(&["architecture/Architecture.lean"]), 1).unwrap();
        assert_eq!((r[0].budget.as_str(), r[0].old, r[0].new), ("module a max_ast", 100, 120));
        let e = judge(Some(&old), &doc(120, ""), "raise: grew", "", &arch(&["architecture/Architecture.lean", "src/x.rs"]), 1).unwrap_err();
        assert!(e.contains("only architecture/, not \"src/x.rs\""), "{e}");
        let e = judge(Some(&old), &doc(120, r#", "a2""#), "raise: grew", "", &arch(&["architecture/Architecture.lean"]), 1).unwrap_err();
        assert!(e.contains("only budget fields in architecture/"), "{e}");
        // A comment-only edit beside the raise is fine: the export is unchanged.
        assert!(judge(Some(&old), &doc(120, ""), "raise: grew", "", &arch(&["architecture/Apis/ab.lean", "architecture/Architecture.lean"]), 1).is_ok());
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
