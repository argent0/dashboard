// `dashboard draft speedup|flaky` (spec §15, §15.1): the director's plan of
// a test-speedup or a flaky-test-fix, drafted from what the player already
// recorded on the hub. Run in the director clone, it fetches the hub and
// creates `task/<slug>` on the hub's `main` with one plan commit adding
// `tasks/<slug>/task.toml`; the player imports it like any director plan,
// and the director reviews it, replans if needed and stages it.
//
// - speedup: the `[[slow]]` checks come from main's cached suite run under
//   the current `[budget]` and `[sandbox]` (every killed check, then every
//   check at `speedup::SLOW_SHARE_PERCENT` of `suite_fail`).
// - flaky <check>: the flake ledger's passes on retry of that check give
//   the failing outputs; the test inventory (`list_cmd`, run on main) read
//   against them gives the `[[replace]]` names, the check's `files` (or the
//   test files naming those tests) give `fix_paths`, and its `cmd` the
//   `[[evidence]]` and the stability `[check]`. A field it can't derive gets
//   a placeholder marked `TODO(director)`.

use crate::checks::{self, Runner};
use crate::config::{self, Suite};
use crate::git::Git;
use crate::lean::Exporter;
use crate::records::{self, FlakeRecord};
use crate::state::{self, HubState, Status};
use crate::{flaky, speedup};

/// How many times the drafted evidence and stability check run the
/// flaky check's command.
pub const FLAKY_RUNS: u32 = 20;

/// A drafted plan, before it is committed.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub slug: String,
    pub phase: String,
    /// `tasks/<slug>/task.toml`.
    pub toml: String,
    /// What was read from the hub, one line each, for the director.
    pub derived: Vec<String>,
    /// The fields left as `TODO(director)` placeholders, one line each.
    pub todo: Vec<String>,
}

/// A draft committed in the director clone, for the player to import.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub commit: String,
    pub draft: Draft,
}

/// What both drafts start from: the director clone fetched, the hub's state
/// and its `main`.
struct Ctx {
    state: HubState,
    main: String,
}

fn start(hub: &Git, director: &Git) -> Result<Ctx, String> {
    director
        .run(&["fetch", "--quiet", "hub"])
        .map_err(|e| format!("fetching the hub: {e}"))?;
    let state = state::derive(hub, &|_| false)?;
    let main = state.main_tip.clone();
    Ok(Ctx { state, main })
}

/// The suite on `main`, which must have a `list_cmd` (both kinds need one).
fn main_suite_with_list(hub: &Git, ctx: &Ctx, kind: &str, exporter: &Exporter) -> Result<(Suite, String), String> {
    let suite = records::suite_at(hub, &ctx.main, Some(exporter))?;
    let list = suite.list_cmd.clone().ok_or_else(|| {
        format!(
            "a {kind} needs `list_cmd` in the `suite where` block of {} on main (one test name per line, e.g. `cargo test -- --list`); add it on main first",
            config::TESTS_LEAN
        )
    })?;
    Ok((suite, list))
}

/// Drafts and commits a test-speedup (spec §15.1) naming every check slow
/// on `main`'s recorded, over-budget suite run.
pub fn speedup(
    hub: &Git,
    director: &Git,
    slug: Option<&str>,
    phase: Option<&str>,
    exporter: &Exporter,
) -> Result<Outcome, String> {
    let ctx = start(hub, director)?;
    let draft = speedup_draft(hub, director, &ctx, slug, phase, exporter)?;
    commit(director, &ctx.main, &draft, "speedup")
}

fn speedup_draft(
    hub: &Git,
    director: &Git,
    ctx: &Ctx,
    slug: Option<&str>,
    phase: Option<&str>,
    exporter: &Exporter,
) -> Result<Draft, String> {
    let roadmap = &ctx.state.roadmap;
    let budget = roadmap.budget;
    let run = state::main_suite(hub, &ctx.main, roadmap);
    let over = run.budget_overrun(&budget).ok_or_else(|| {
        format!(
            "nothing to draft: the player has recorded no over-budget run of main's suite at {} under the current [budget] and [sandbox] (the main_over_budget event and `status` show one)",
            short(&ctx.main)
        )
    })?;
    let slow = speedup::slow_checks(&over, &budget);
    if slow.is_empty() {
        return Err(format!(
            "nothing to draft: main's suite is over suite_fail ({}s), yet no check takes {}% of it on its own; raise [budget] in roadmap.toml, or plan an ordinary task",
            budget.suite_fail,
            speedup::SLOW_SHARE_PERCENT
        ));
    }
    let (_, list_cmd) = main_suite_with_list(hub, ctx, "test-speedup", exporter)?;
    let slug = resolve_slug(director, &ctx.state, slug, "test-speedup")?;
    let phase = resolve_phase(&ctx.state, phase)?;

    let timed: Vec<String> = slow
        .iter()
        .map(|n| {
            let t = checks::fmt_secs(over.millis.get(n).copied().unwrap_or(0));
            if over.timed_out.contains(n) {
                format!("{n} (killed at check_timeout {}s)", budget.check_timeout)
            } else {
                format!("{n} ({t})")
            }
        })
        .collect();
    let brief = format!(
        "main's suite is over its [budget]: {}. Make {} faster. The finish passes the suite with no check killed; while it is still over suite_fail, each named check must be strictly faster than on main, no other check slower and the total no higher. Share expensive setup, cut duplicate cases and fixed sleeps, run independent work in parallel; keep what each test asserts. Every test removed and every `cmd` rewired is listed for the director at accept.",
        over.why.trim_end_matches('.'),
        timed.join(", ")
    );
    let mut toml = format!(
        "# Drafted by `dashboard draft speedup` from main's recorded suite run at {}.\n",
        short(&ctx.main)
    );
    toml.push_str(&format!("title = {}\n", q(&format!("Speed up {}", slow.join(", ")))));
    toml.push_str(&format!("phase = {}\n", q(&phase)));
    toml.push_str("kind = \"test-speedup\"\n");
    toml.push_str(&format!("brief = {}\n", q(&brief)));
    for s in &slow {
        toml.push_str(&format!("\n[[slow]]\ncheck = {}\n", q(s)));
    }
    let derived = vec![
        format!("main at {}: {}", short(&ctx.main), over.why),
        format!("[[slow]]: {}", timed.join(", ")),
        format!("list_cmd: {list_cmd}"),
    ];
    Ok(Draft { slug, phase, toml, derived, todo: Vec::new() })
}

/// Drafts and commits a flaky-test-fix (spec §15) for suite check `check`,
/// from its passes on retry in the flake ledger. `runner` runs `list_cmd`
/// on `main` and exports its suite.
pub fn flaky(
    hub: &Git,
    director: &Git,
    check: &str,
    slug: Option<&str>,
    phase: Option<&str>,
    runner: &Runner,
) -> Result<Outcome, String> {
    let ctx = start(hub, director)?;
    let draft = flaky_draft(hub, director, &ctx, check, slug, phase, runner)?;
    commit(director, &ctx.main, &draft, &format!("flaky {check}"))
}

fn flaky_draft(
    hub: &Git,
    director: &Git,
    ctx: &Ctx,
    name: &str,
    slug: Option<&str>,
    phase: Option<&str>,
    runner: &Runner,
) -> Result<Draft, String> {
    let flakes: Vec<&FlakeRecord> = ctx.state.flakes.iter().filter(|f| f.check == name).collect();
    if flakes.is_empty() {
        let mut known: Vec<&str> = ctx.state.flakes.iter().map(|f| f.check.as_str()).collect();
        known.sort_unstable();
        known.dedup();
        return Err(format!(
            "nothing to draft: the flake ledger has no pass on retry of check {name:?} (recorded: {})",
            if known.is_empty() { "none".to_string() } else { known.join(", ") }
        ));
    }
    let (suite, list_cmd) = main_suite_with_list(hub, ctx, "flaky-test-fix", runner.exporter())?;
    let check = suite.check(name).ok_or_else(|| {
        format!("check {name:?} is not in main's suite: a flaky-test-fix replaces a test on main")
    })?;
    let slug = resolve_slug(director, &ctx.state, slug, &format!("fix-flaky-{}", slugify(name)))?;
    let phase = resolve_phase(&ctx.state, phase)?;
    let mut derived = Vec::new();
    let mut todo = Vec::new();

    let scopes: Vec<String> = flakes
        .iter()
        .map(|f| format!("{} (retry {}/{})", f.scope, f.retries, f.retry_failed))
        .collect();
    derived.push(format!("flake ledger: {name} passed on retry in {}", scopes.join(", ")));

    // [[replace]]: the inventory's tests the failing outputs name.
    let outputs: Vec<String> = flakes.iter().flat_map(|f| f.outputs.iter().cloned()).collect();
    let olds = match runner.run_list(&ctx.main, &list_cmd) {
        Ok(inventory) => {
            let named = flaky::named_in(&inventory, &outputs);
            if named.is_empty() {
                todo.push(format!(
                    "[[replace]]: no test of the inventory ({} from `{list_cmd}`) is named in the failing output; old/new are placeholders",
                    inventory.len()
                ));
            } else {
                derived.push(format!("[[replace]]: {} named in the failing output (inventory from `{list_cmd}`)", named.join(", ")));
            }
            named
        }
        Err(e) => {
            todo.push(format!("[[replace]]: `{list_cmd}` failed on main ({e}); old/new are placeholders"));
            Vec::new()
        }
    };

    // fix_paths: the check's own files, narrowed to those naming the tests.
    let plain = |p: &str| !(p.starts_with("architecture/") || p.starts_with("tasks/"));
    let expand = |entries: &[String]| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in entries {
            for p in hub.ls_tree_files(&ctx.main, e) {
                if plain(&p) && !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        out
    };
    let own = expand(&check.files);
    let naming = |paths: &[String]| -> Vec<String> {
        let lasts: Vec<&str> = olds.iter().map(|o| o.rsplit("::").next().unwrap_or(o)).collect();
        paths
            .iter()
            .filter(|p| {
                hub.show_file(&ctx.main, p)
                    .is_some_and(|text| lasts.iter().any(|l| !flaky::named_in(&[l.to_string()], &[text.clone()]).is_empty()))
            })
            .cloned()
            .collect()
    };
    let mut fix_paths = Vec::new();
    if !olds.is_empty() {
        fix_paths = naming(&own);
        if fix_paths.is_empty() {
            fix_paths = naming(&expand(&suite.test_paths));
        }
        if !fix_paths.is_empty() {
            derived.push(format!("fix_paths: {} (the files naming the flaky tests)", fix_paths.join(", ")));
        }
    }
    if fix_paths.is_empty() && !own.is_empty() {
        fix_paths = own.clone();
        derived.push(format!("fix_paths: {} (the files of check {name})", fix_paths.join(", ")));
    }
    let fix_todo = fix_paths.is_empty();
    if fix_todo {
        fix_paths = vec!["TODO/flaky-test-file".to_string()];
        todo.push("fix_paths: no file of the check or the test tree names the flaky test; the path is a placeholder".to_string());
    }

    let replace: Vec<(String, String)> = if olds.is_empty() {
        vec![("TODO-flaky-test".to_string(), "TODO-flaky-test_stable".to_string())]
    } else {
        olds.iter()
            .map(|o| (o.clone(), format!("{}_stable", o.rsplit("::").next().unwrap_or(o))))
            .collect()
    };

    let mut stable = format!("{}-stable", name);
    let mut n = 2;
    while suite.check(&stable).is_some() {
        stable = format!("{name}-stable-{n}");
        n += 1;
    }
    derived.push(format!(
        "[[evidence]] and [check] {stable}: `{}`, {FLAKY_RUNS} runs (raise runs for a rarer flake)",
        check.cmd
    ));

    let last = flakes
        .iter()
        .rev()
        .flat_map(|f| f.outputs.iter().rev())
        .find(|o| !o.trim().is_empty())
        .map(|o| tail(o, 5))
        .unwrap_or_default();
    let tests = if olds.is_empty() {
        "the flaky test".to_string()
    } else {
        olds.join(", ")
    };
    let mut brief = format!(
        "Check `{name}` passed only on a retry ({}), so a test in it is flaky. Replace {tests} with a stable test in fix_paths. Helpers first, keep assertions as strong: make the helper, fixture or wait the test relies on deterministic before touching its body, and keep every assertion at least as strong as the old test's. A flake whose cause is in the product code is a `block:`, never this task.",
        scopes.join(", ")
    );
    if !last.is_empty() {
        brief.push_str(&format!("\n\nLast failing output:\n{last}"));
    }

    let mut toml = format!(
        "# Drafted by `dashboard draft flaky {name}` from the flake ledger and main at {}.\n",
        short(&ctx.main)
    );
    toml.push_str(&format!("title = {}\n", q(&format!("Replace the flaky test in {name}"))));
    toml.push_str(&format!("phase = {}\n", q(&phase)));
    toml.push_str("kind = \"flaky-test-fix\"\n");
    if fix_todo {
        toml.push_str("# TODO(director): the files the worker may change to replace the test.\n");
    }
    toml.push_str(&format!(
        "fix_paths = [{}]\n",
        fix_paths.iter().map(|p| q(p)).collect::<Vec<_>>().join(", ")
    ));
    toml.push_str(&format!("brief = {}\n", q(&brief)));
    for (old, new) in &replace {
        toml.push_str("\n[[replace]]\n");
        if olds.is_empty() {
            toml.push_str("# TODO(director): the flaky test's name in the inventory, and its replacement's.\n");
        }
        toml.push_str(&format!("old = {}\nnew = {}\n", q(old), q(new)));
    }
    toml.push_str(&format!(
        "\n[[evidence]]\nname = {}\ncmd = {}\nruns = {FLAKY_RUNS}\nmin_pass = 1\nmax_pass = {}\n",
        q(name),
        q(&check.cmd),
        FLAKY_RUNS - 1
    ));
    toml.push_str(&format!(
        "\n[check]\nname = {}\ncmd = {}\nruns = {FLAKY_RUNS}\nmin_pass = {FLAKY_RUNS}\n",
        q(&stable),
        q(&check.cmd)
    ));
    Ok(Draft { slug, phase, toml, derived, todo })
}

/// A TOML string literal.
fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// The last `n` non-blank lines of `text`, each cut at 200 characters.
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take(200).collect())
        .collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// `name` as a slug fragment: lowercase `[a-z0-9-]`, runs of anything else
/// one `-`.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "check".to_string() } else { out }
}

fn valid_slug(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Whether `slug` is in use: a task the hub knows (live, accepted, retired
/// or explored) or a branch of the director clone.
fn taken(director: &Git, state: &HubState, slug: &str) -> bool {
    state.tasks.iter().any(|t| t.slug == slug) || director.rev_parse(&format!("refs/heads/task/{slug}")).is_some()
}

/// `given`, checked; else `base`, or `base-2`, `base-3`… when taken.
fn resolve_slug(director: &Git, state: &HubState, given: Option<&str>, base: &str) -> Result<String, String> {
    if let Some(s) = given {
        if !valid_slug(s) {
            return Err(format!("{s:?} is not a valid slug (must match [a-z0-9][a-z0-9-]*)"));
        }
        if taken(director, state, s) {
            return Err(format!("task/{s} already exists; slugs are never reused"));
        }
        return Ok(s.to_string());
    }
    let mut slug = base.to_string();
    let mut n = 2;
    while taken(director, state, &slug) {
        slug = format!("{base}-{n}");
        n += 1;
    }
    Ok(slug)
}

/// `given`, checked against roadmap.toml's phases; else the first phase,
/// in roadmap order, with a task still open (neither accepted, retired nor
/// explored), else the last phase, else none.
fn resolve_phase(state: &HubState, given: Option<&str>) -> Result<String, String> {
    let phases = &state.roadmap.phases;
    if let Some(p) = given {
        if !phases.iter().any(|ph| ph.id == p) {
            let ids: Vec<&str> = phases.iter().map(|ph| ph.id.as_str()).collect();
            return Err(format!("unknown phase {p:?}; roadmap.toml has: {}", ids.join(", ")));
        }
        return Ok(p.to_string());
    }
    let open = |id: &str| {
        state.tasks.iter().any(|t| {
            t.config.phase == id && !matches!(t.status, Status::Accepted | Status::Retired | Status::Explored)
        })
    };
    Ok(phases
        .iter()
        .find(|ph| open(&ph.id))
        .or(phases.last())
        .map(|ph| ph.id.clone())
        .unwrap_or_default())
}

/// Commits `draft` in `director` as a new `task/<slug>` on `main`: one
/// commit adding `tasks/<slug>/task.toml`, built in a private index so the
/// working tree and the real index stay as they are. The TOML is validated
/// first, as the player will.
fn commit(director: &Git, main: &str, draft: &Draft, what: &str) -> Result<Outcome, String> {
    config::task_config_from_str(&draft.toml).map_err(|e| format!("the drafted task.toml is invalid ({e}):\n{}", draft.toml))?;
    let slug = &draft.slug;
    let git_dir = director
        .run(&["rev-parse", "--absolute-git-dir"])
        .map_err(|e| e.to_string())?;
    let index = format!("{git_dir}/dashboard-draft.index");
    let env = [("GIT_INDEX_FILE", index.as_str())];
    let path = format!("tasks/{slug}/task.toml");
    let message = format!("plan: {slug}\n\nDrafted by `dashboard draft {what}`.");
    let result = (|| {
        director.run_with_env(&["read-tree", main], &env).map_err(|e| e.to_string())?;
        let blob = director
            .run_with_stdin(&["hash-object", "-w", "--stdin"], &draft.toml)
            .map_err(|e| e.to_string())?;
        director
            .run_with_env(&["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{path}")], &env)
            .map_err(|e| e.to_string())?;
        let tree = director.run_with_env(&["write-tree"], &env).map_err(|e| e.to_string())?;
        director
            .run(&["commit-tree", &tree, "-p", main, "-m", &message])
            .map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&index);
    let commit = result?;
    // Created only where absent: the zero old value refuses an existing branch.
    director
        .run(&["update-ref", "-m", "dashboard draft", &format!("refs/heads/task/{slug}"), &commit, &"0".repeat(commit.len())])
        .map_err(|e| e.to_string())?;
    Ok(Outcome { commit, draft: draft.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_from_check_names() {
        assert_eq!(slugify("Unit_Tests::a"), "unit-tests-a");
        assert_eq!(slugify("«x»"), "x");
        assert_eq!(slugify("__"), "check");
        assert!(valid_slug("fix-flaky-a") && !valid_slug("-a") && !valid_slug("A"));
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\n\nb\nc\n", 2), "b\nc");
        assert_eq!(tail("", 3), "");
    }

    #[test]
    fn toml_strings_escape() {
        let s = q("a \"b\"\nc\\");
        let v: toml::Table = toml::from_str(&format!("x = {s}")).unwrap();
        assert_eq!(v["x"].as_str(), Some("a \"b\"\nc\\"));
    }
}
