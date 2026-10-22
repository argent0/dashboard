// `dashboard trim <slug> <check>…` (spec §6.5): the director's trim, built
// mechanically in the director clone. Given the names of checks a `tested`
// task added, it writes ONE commit on top of the hub's `task/<slug>` that
// removes their `check` blocks from `architecture/Tests/<slug>.lean` and
// `<slug>.worker.lean` (a file left with no check goes whole) and deletes
// the test files only those checks own. The player imports it like any
// director commit and validates it as a trim (`player::try_trim`).
//
// The Lean source is edited as text: a block is its `check <name> where`
// line, the comment lines right above it and every more-indented line
// below. The suite exported from the edited tree is then compared with the
// tip's: exactly the named checks are gone and every other is unchanged.

use crate::config::{self, Suite};
use crate::git::Git;
use crate::lean::Exporter;
use crate::state::{self, Status};

/// A trim committed in the director clone, for the player to import.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub commit: String,
    pub plan: Plan,
}

/// Fetches the hub into `director` (its `hub` remote) and commits the trim
/// of `names` on `task/<slug>` there (`plan`, `commit`, `verify`,
/// `advance`). The task must be `tested` on the hub.
pub fn run(hub: &Git, director: &Git, slug: &str, names: &[String], exporter: &Exporter) -> Result<Outcome, String> {
    director
        .run(&["fetch", "--quiet", "hub"])
        .map_err(|e| format!("fetching the hub: {e}"))?;
    let hub_state = state::derive(hub, &|_| false)?;
    let task = hub_state
        .tasks
        .iter()
        .find(|t| t.slug == slug)
        .ok_or_else(|| format!("the hub has no task/{slug}"))?;
    if task.status != Status::Tested {
        return Err(format!(
            "task/{slug} is {}: a trim goes on a tested task, before its accept",
            task.status.as_str()
        ));
    }
    let tip = task.tip.clone();
    let main = hub
        .rev_parse("refs/heads/main")
        .ok_or("the hub has no main")?;
    let suite = crate::records::suite_at(hub, &tip, Some(exporter))?;
    let plan = plan(hub, slug, &tip, &main, &suite, names)?;
    let commit = commit(director, &tip, &plan, &message(slug, &plan))?;
    let tree = director
        .rev_parse(&format!("{commit}:{}", config::ARCH_DIR))
        .ok_or_else(|| format!("{}: no tree at the trim", config::ARCH_DIR))?;
    let after = exporter
        .export_tree(director, &tree)
        .map_err(|e| format!("the trimmed suite: {e}"))?
        .suite;
    verify(&suite, &after, &plan.dropped)?;
    director.advance_branch(&format!("task/{slug}"), &tip, &commit, "trim")?;
    Ok(Outcome { commit, plan })
}

/// What a trim does, before it is committed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The dropped checks, in the order given.
    pub dropped: Vec<String>,
    /// Each edited Lean file and its new text; `None` deletes it.
    pub lean: Vec<(String, Option<String>)>,
    /// Test files deleted.
    pub deleted: Vec<String>,
    /// Test files of dropped checks that stay, and why.
    pub kept: Vec<(String, String)>,
}

/// The Lean identifier at the start of `s` (plain or `«…»`) and the rest.
fn lean_ident(s: &str) -> Option<(String, &str)> {
    if let Some(rest) = s.strip_prefix('«') {
        let end = rest.find('»')?;
        return Some((rest[..end].to_string(), &rest[end + '»'.len_utf8()..]));
    }
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '\'' | '!' | '?')))
        .map_or(s.len(), |(i, _)| i);
    (end > 0).then(|| (s[..end].to_string(), &s[end..]))
}

/// The check a `check <name> where` line declares, with its indentation.
fn check_header(line: &str) -> Option<(usize, String)> {
    let line = line.trim_end();
    let indent = line.len() - line.trim_start().len();
    let rest = line.trim_start().strip_prefix("check")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let (name, after) = lean_ident(rest.trim_start())?;
    let after = after.trim_start();
    (after == "where" || after.starts_with("where ") || after.starts_with("where\t") || after.starts_with("where--"))
        .then_some((indent, name))
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The names of the checks `text` declares, in order.
pub fn declared_checks(text: &str) -> Vec<String> {
    text.lines().filter_map(check_header).map(|(_, n)| n).collect()
}

/// `text` without the `check` blocks named in `names`, and the names it
/// removed. A block runs from its comment lines (`--`, or a `/- … -/`
/// ending right above) through its header to the line before the next
/// non-blank line indented no deeper than the header; the blank lines that
/// follow it go with it.
pub fn remove_checks(text: &str, names: &[&str]) -> (String, Vec<String>) {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut drop = vec![false; lines.len()];
    let mut removed = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some((indent, name)) = check_header(lines[i]) else {
            i += 1;
            continue;
        };
        let mut end = i + 1;
        while end < lines.len() && (lines[end].trim().is_empty() || indent_of(lines[end]) > indent) {
            end += 1;
        }
        if names.contains(&name.as_str()) {
            let mut start = i;
            while start > 0 {
                let prev = lines[start - 1].trim();
                if prev.starts_with("--") {
                    start -= 1;
                } else if prev.ends_with("-/") {
                    let open = (0..start).rev().find(|&k| lines[k].contains("/-"));
                    match open {
                        Some(k) => start = k,
                        None => break,
                    }
                } else {
                    break;
                }
            }
            drop[start..end].iter_mut().for_each(|d| *d = true);
            removed.push(name);
        }
        i = end;
    }
    let mut out: String = lines
        .iter()
        .zip(&drop)
        .filter(|(_, d)| !**d)
        .map(|(l, _)| *l)
        .collect();
    // A block at the end leaves the blank line that separated it.
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    (out, removed)
}

/// Whether `path` is `entry` or lies under it (a directory entry).
fn under(path: &str, entry: &str) -> bool {
    let prefix = entry.trim_end_matches('/');
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// The trim of `names` on `task/<slug>` at `tip` (spec §6.5). `suite` is the
/// suite at `tip`; `main` is the hub's main. Refuses an unknown name and a
/// check declared anywhere but the task's two Lean files.
pub fn plan(git: &Git, slug: &str, tip: &str, main: &str, suite: &Suite, names: &[String]) -> Result<Plan, String> {
    let own = [config::tests_file(slug), config::worker_tests_file(slug)];
    let ours: Vec<&str> = suite
        .checks
        .iter()
        .filter(|c| c.source.as_ref().is_some_and(|s| own.contains(s)))
        .map(|c| c.name.as_str())
        .collect();
    let mut dropped: Vec<String> = Vec::new();
    for name in names {
        if dropped.contains(name) {
            continue;
        }
        match suite.check(name) {
            None => {
                return Err(format!(
                    "no check {name:?} in the suite at task/{slug}; its own checks: {}",
                    if ours.is_empty() { "none".to_string() } else { ours.join(", ") }
                ));
            }
            Some(c) if !ours.contains(&c.name.as_str()) => {
                let from = c.source.as_deref().unwrap_or("the suite");
                return Err(format!(
                    "check {name:?} is declared in {from}; a trim drops only checks task/{slug} added, in {} or {}",
                    own[0], own[1]
                ));
            }
            Some(_) => dropped.push(name.clone()),
        }
    }

    let mut lean = Vec::new();
    for file in &own {
        let mine: Vec<&str> = dropped
            .iter()
            .filter(|n| suite.check(n).and_then(|c| c.source.as_ref()) == Some(file))
            .map(String::as_str)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let text = git
            .show_file(tip, file)
            .ok_or_else(|| format!("{file} is missing at task/{slug}"))?;
        let (edited, removed) = remove_checks(&text, &mine);
        if let Some(missing) = mine.iter().find(|n| !removed.iter().any(|r| r == *n)) {
            return Err(format!("{file}: no `check {missing} where` block to remove"));
        }
        let left = declared_checks(&edited);
        lean.push((file.clone(), (!left.is_empty()).then_some(edited)));
    }

    // The files of the dropped checks, path by path: one also under a
    // remaining check's `files`, under `test_paths` or on main stays.
    let names_dropped: Vec<&str> = dropped.iter().map(String::as_str).collect();
    let remaining: Vec<&config::Check> = suite
        .checks
        .iter()
        .filter(|c| !names_dropped.contains(&c.name.as_str()))
        .collect();
    let mut deleted: Vec<String> = Vec::new();
    let mut kept: Vec<(String, String)> = Vec::new();
    for entry in suite.files_of(&names_dropped) {
        for path in git.ls_tree_files(tip, &entry) {
            if deleted.contains(&path) || kept.iter().any(|(p, _)| *p == path) {
                continue;
            }
            let why = if let Some(c) = remaining.iter().find(|c| c.files.iter().any(|f| under(&path, f))) {
                Some(format!("also in the files of check {}", c.name))
            } else if suite.test_paths.iter().any(|t| under(&path, t)) {
                Some("under test_paths, shared by the suite".to_string())
            } else if git.blob_hash(main, &path).is_some() {
                Some("already on main".to_string())
            } else {
                None
            };
            match why {
                Some(why) => kept.push((path, why)),
                None => deleted.push(path),
            }
        }
    }
    Ok(Plan { dropped, lean, deleted, kept })
}

/// The first check of the task's Lean files (`own`) in `after` that
/// `before` lacks verbatim, or `None` when `after` only lost checks.
pub fn additions(before: &Suite, after: &Suite, own: &[String]) -> Option<String> {
    after
        .checks
        .iter()
        .filter(|c| c.source.as_ref().is_some_and(|s| own.contains(s)))
        .find(|c| before.check(&c.name) != Some(*c))
        .map(|c| c.name.clone())
}

/// Whether `after` is `before` without exactly the `dropped` checks.
pub fn verify(before: &Suite, after: &Suite, dropped: &[String]) -> Result<(), String> {
    if let Some(c) = dropped.iter().find(|n| after.check(n).is_some()) {
        return Err(format!("check {c:?} is still in the suite after the edit"));
    }
    for c in &before.checks {
        if dropped.contains(&c.name) {
            continue;
        }
        if after.check(&c.name) != Some(c) {
            return Err(format!("check {:?} changed or vanished with the edit", c.name));
        }
    }
    if after.checks.len() + dropped.len() != before.checks.len() {
        return Err("the edit added a check".to_string());
    }
    Ok(())
}

/// The commit of `plan` on top of `tip` in `git`, built in a private index
/// so the working tree and the real index stay as they are.
pub fn commit(git: &Git, tip: &str, plan: &Plan, message: &str) -> Result<String, String> {
    let git_dir = git
        .run(&["rev-parse", "--absolute-git-dir"])
        .map_err(|e| e.to_string())?;
    let index = format!("{git_dir}/dashboard-trim.index");
    let env = [("GIT_INDEX_FILE", index.as_str())];
    let result = (|| {
        git.run_with_env(&["read-tree", tip], &env).map_err(|e| e.to_string())?;
        for (path, text) in &plan.lean {
            match text {
                Some(text) => {
                    let blob = git
                        .run_with_stdin(&["hash-object", "-w", "--stdin"], text)
                        .map_err(|e| e.to_string())?;
                    git.run_with_env(&["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{path}")], &env)
                        .map_err(|e| e.to_string())?;
                }
                None => {
                    git.run_with_env(&["update-index", "--force-remove", "--", path], &env)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        for path in &plan.deleted {
            git.run_with_env(&["update-index", "--force-remove", "--", path], &env)
                .map_err(|e| e.to_string())?;
        }
        let tree = git.run_with_env(&["write-tree"], &env).map_err(|e| e.to_string())?;
        git.run(&["commit-tree", &tree, "-p", tip, "-m", message])
            .map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&index);
    result
}

/// The commit message of `plan`: the dropped checks, and the kept files.
pub fn message(slug: &str, plan: &Plan) -> String {
    let mut msg = format!("trim: {slug}: drop {}", plan.dropped.join(", "));
    if !plan.kept.is_empty() {
        msg.push_str("\n\nKept:\n");
        for (path, why) in &plan.kept {
            msg.push_str(&format!("- {path} ({why})\n"));
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "import Architecture\nopen Dashboard\n\n\
-- keeps\ncheck «keep-me» where\n  cmd = \"true\"\n  covers = [a]\n\n\
/-- Scaffolding,\n  over two lines. -/\ncheck drop_me where\n  cmd = \"sh x.sh\"\n\n  covers = [a]\n  files = [\"x.sh\"]\n\n\
check «check» where\n  cmd = \"true\"\n  covers = all\n";

    #[test]
    fn removes_blocks_with_their_comments_and_keeps_the_rest_verbatim() {
        assert_eq!(declared_checks(TEXT), ["keep-me", "drop_me", "check"]);
        let (out, removed) = remove_checks(TEXT, &["drop_me"]);
        assert_eq!(removed, ["drop_me"]);
        assert_eq!(
            out,
            "import Architecture\nopen Dashboard\n\n-- keeps\ncheck «keep-me» where\n  cmd = \"true\"\n  covers = [a]\n\n\
check «check» where\n  cmd = \"true\"\n  covers = all\n"
        );
        let (out, removed) = remove_checks(TEXT, &["check", "keep-me", "absent"]);
        assert_eq!(removed, ["keep-me", "check"]);
        assert_eq!(declared_checks(&out), ["drop_me"]);
        assert!(out.ends_with("  files = [\"x.sh\"]\n") && !out.contains("keeps"), "{out}");
    }

    #[test]
    fn a_header_needs_where() {
        assert_eq!(check_header("check a where"), Some((0, "a".into())));
        assert_eq!(check_header("  check «a-b» where  "), Some((2, "a-b".into())));
        assert_eq!(check_header("checkx where"), None);
        assert_eq!(check_header("check a"), None);
        assert_eq!(check_header("-- check a where"), None);
    }
}
