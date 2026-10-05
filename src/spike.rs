// Spikes (spec.md §14): a task kind whose product is an answer to a pinned
// question, backed by claims the player runs itself, never merged into
// `main`. This module holds the parts that don't need the player's session
// plumbing: reading a round's files, running claims against the right code
// version, deciding whether a finish claim's answer stands, and the text
// handed to the subagent. The transitions themselves (post, finish,
// send-back, conclude) live in src/player.rs next to their normal-task
// counterparts.

use crate::checks::{CheckResult, Runner};
use crate::config::{self, Claim, ClaimKind, Question, INCONCLUSIVE};
use crate::git::Git;
use crate::records::{self, ClaimNote};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};

// --------------------------------------------------------------------------
// Paths
// --------------------------------------------------------------------------

pub fn question_path(slug: &str, round: u32) -> String {
    format!("tasks/{slug}/questions/{round}.toml")
}

pub fn baseline_path(slug: &str, round: u32) -> String {
    format!("tasks/{slug}/baseline/{round}.toml")
}

pub fn claims_path(slug: &str, round: u32) -> String {
    format!("tasks/{slug}/claims/{round}.toml")
}

pub fn findings_path(slug: &str, round: u32) -> String {
    format!("tasks/{slug}/findings/{round}.md")
}

// --------------------------------------------------------------------------
// Reading a round
// --------------------------------------------------------------------------

/// `tasks/<slug>/questions/<round>.toml` at `commit`, parsed and validated.
pub fn read_question(hub: &Git, commit: &str, slug: &str, round: u32) -> Result<Question, String> {
    let path = question_path(slug, round);
    let text = hub.show_file(commit, &path).ok_or_else(|| format!("{path} not found"))?;
    config::question_from_str(&text).map_err(|e| format!("{path}: {e}"))
}

/// The director's baseline claims for `round` (none if the file is absent).
pub fn read_baseline(hub: &Git, commit: &str, slug: &str, round: u32) -> Result<Vec<Claim>, String> {
    let path = baseline_path(slug, round);
    match hub.show_file(commit, &path) {
        None => Ok(Vec::new()),
        Some(text) => config::baseline_file_from_str(&text).map(|f| f.claim).map_err(|e| format!("{path}: {e}")),
    }
}

/// A claim name may appear once per round across the director's baseline
/// file and the subagent's claims file.
fn check_unique_names(claims: &[&Claim]) -> Result<(), String> {
    for (i, c) in claims.iter().enumerate() {
        if claims[..i].iter().any(|o| o.name == c.name) {
            return Err(format!("claim name {:?} is used twice", c.name));
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------
// The base-with-overlay commit
// --------------------------------------------------------------------------

/// A commit holding `base`'s tree with `tasks/<slug>/` replaced by
/// `candidate`'s copy: the code as it is without the spike, plus the claim
/// files and scripts, so claims can be run "on the current code". The commit
/// is deterministic (fixed identity and dates), so the check-result cache
/// hits whenever the same overlay is evaluated again. It is never referenced
/// by a ref.
pub fn overlay_commit(hub: &Git, base: &str, candidate: &str, slug: &str) -> Result<String, String> {
    let prefix = format!("tasks/{slug}/");
    let index = hub.dir.join(format!("dashboard-overlay-{}.index", std::process::id()));
    let result = build_overlay(hub, base, candidate, &prefix, &index);
    let _ = std::fs::remove_file(&index);
    result
}

fn build_overlay(hub: &Git, base: &str, candidate: &str, prefix: &str, index: &std::path::Path) -> Result<String, String> {
    let index_str = index.to_str().ok_or_else(|| "overlay index path is not valid UTF-8".to_string())?;
    let env = [("GIT_INDEX_FILE", index_str)];
    hub.run_with_env(&["read-tree", base], &env).map_err(|e| e.to_string())?;

    // Mode 0 in --index-info removes an entry; a normal line adds one.
    let zero = "0".repeat(40);
    let mut info = String::new();
    for path in hub.ls_tree_files(base, prefix) {
        info.push_str(&format!("0 {zero}\t{path}\n"));
    }
    let listing = hub.run(&["ls-tree", "-r", candidate, "--", prefix]).map_err(|e| e.to_string())?;
    for line in listing.lines() {
        // "<mode> blob <sha>\t<path>"
        let Some((meta, path)) = line.split_once('\t') else { continue };
        let mut parts = meta.split_whitespace();
        let (Some(mode), Some(_), Some(sha)) = (parts.next(), parts.next(), parts.next()) else { continue };
        info.push_str(&format!("{mode} {sha}\t{path}\n"));
    }

    let mut child = Command::new("git")
        .args(["update-index", "--index-info"])
        .current_dir(&hub.dir)
        .env("GIT_INDEX_FILE", index_str)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git update-index: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(info.as_bytes()).map_err(|e| format!("git update-index: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("git update-index: {e}"))?;
    if !out.status.success() {
        return Err(format!("git update-index --index-info: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }

    let tree = hub.run_with_env(&["write-tree"], &env).map_err(|e| e.to_string())?;
    hub.run_with_env(
        &["commit-tree", &tree, "-p", base, "-m", "spike claims overlay"],
        &[
            ("GIT_AUTHOR_NAME", "dashboard-player"),
            ("GIT_AUTHOR_EMAIL", "dashboard-player@localhost"),
            ("GIT_COMMITTER_NAME", "dashboard-player"),
            ("GIT_COMMITTER_EMAIL", "dashboard-player@localhost"),
            ("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z"),
            ("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z"),
        ],
    )
    .map_err(|e| e.to_string())
}

// --------------------------------------------------------------------------
// Running claims
// --------------------------------------------------------------------------

/// Runs each of `claims` at `commit` and returns one failure line per claim
/// that does not pass (empty when all pass), each with the end of its output.
pub fn failing_at(runner: &Runner, commit: &str, claims: &[Claim], what: &str) -> Vec<String> {
    claims
        .iter()
        .filter_map(|c| {
            let r = runner.run_claim(commit, c);
            if r.pass { None } else { Some(failure_line(&c.name, &format!("fails {what}"), &r)) }
        })
        .collect()
}

fn failure_line(name: &str, what: &str, result: &CheckResult) -> String {
    let passed = result.exits.iter().filter(|&&e| e == 0).count();
    let mut line = format!("claim {name:?} {what} ({passed}/{} runs passed)", result.runs);
    if let Some(tail) = &result.output_tail {
        let tail = tail.trim_end();
        let tail: String = if tail.chars().count() > 600 { tail.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect() } else { tail.to_string() };
        line.push_str("\n    ");
        line.push_str(&tail.replace('\n', "\n    "));
    }
    line
}

// --------------------------------------------------------------------------
// A finish claim's evaluation (spec §14, "Finish")
// --------------------------------------------------------------------------

/// What the player verified about a finish claim, recorded in the
/// exploration note.
#[derive(Debug, Clone)]
pub struct Evaluation {
    pub round: u32,
    pub question: Question,
    pub answer: String,
    pub claims: BTreeMap<String, ClaimNote>,
}

/// Decides whether the finish claim `candidate` stands. `base` is the pinned
/// base commit (`records::base_commit`). Every rule is checked by running or
/// reading something; none trusts the findings' contents. Err carries the
/// whole rejection text (without the reason prefix) for the `claims` strike.
pub fn evaluate_finish(hub: &Git, runner: &Runner, slug: &str, base: &str, candidate: &str) -> Result<Evaluation, String> {
    let round = records::spike_round(hub, candidate, slug);
    if round == 0 {
        return Err(format!("task/{slug} has no question posted"));
    }
    let question = read_question(hub, candidate, slug, round)?;

    // 1. findings delivered
    let findings = findings_path(slug, round);
    if hub.show_file(candidate, &findings).is_none_or(|t| t.trim().is_empty()) {
        return Err(format!("{findings} is missing or empty: deliver your findings there (round {round})"));
    }

    // 2. the claims file and its answer
    let claims_file = claims_path(slug, round);
    let text = hub.show_file(candidate, &claims_file).ok_or_else(|| format!("{claims_file} is missing: declare your answer and claims there (round {round})"))?;
    let file = config::claims_file_from_str(&text).map_err(|e| format!("{claims_file}: {e}"))?;
    if !question.allows(&file.answer) {
        return Err(format!("{claims_file}: answer {:?} is not one of the question's answers {:?} or {INCONCLUSIVE:?}", file.answer, question.answers));
    }

    let director = read_baseline(hub, candidate, slug, round)?;
    let mut all: Vec<&Claim> = director.iter().collect();
    all.extend(file.claim.iter());
    check_unique_names(&all).map_err(|e| format!("round {round}: {e}"))?;

    for c in &file.claim {
        if let Some(a) = &c.supports {
            if c.kind != ClaimKind::Spike {
                return Err(format!("{claims_file}: claim {:?} sets `supports` but isn't a spike claim", c.name));
            }
            if !question.answers.contains(a) {
                return Err(format!("{claims_file}: claim {:?} supports {a:?}, which is not one of the question's answers {:?}", c.name, question.answers));
            }
        }
    }
    // 5. a real answer needs a claim behind it (checked before the runs: it's free)
    if file.answer != INCONCLUSIVE && !file.claim.iter().any(|c| c.kind == ClaimKind::Spike && c.supports.as_deref() == Some(file.answer.as_str())) {
        return Err(format!("{claims_file}: answer {:?} needs at least one spike claim with supports = {:?}", file.answer, file.answer));
    }

    // 3 + 4. run the claims against the right code version
    let overlay = overlay_commit(hub, base, candidate, slug)?;
    let mut failures: Vec<String> = Vec::new();
    let mut notes: BTreeMap<String, ClaimNote> = BTreeMap::new();

    for c in director.iter().chain(file.claim.iter().filter(|c| c.kind == ClaimKind::Baseline)) {
        let r = runner.run_claim(&overlay, c);
        if r.pass {
            notes.insert(c.name.clone(), ClaimNote { kind: ClaimKind::Baseline, passed: true, metrics: r.metrics });
        } else {
            failures.push(failure_line(&c.name, "is a baseline claim and fails on the base commit (it must pass there)", &r));
        }
    }
    for c in file.claim.iter().filter(|c| c.kind == ClaimKind::Spike) {
        let on_branch = runner.run_claim(candidate, c);
        if !on_branch.pass {
            failures.push(failure_line(&c.name, "is a spike claim and fails on your branch (it must pass there)", &on_branch));
            continue;
        }
        let on_base = runner.run_claim(&overlay, c);
        if on_base.pass {
            failures.push(format!("claim {:?} is a spike claim but also passes on the base commit without your work: it proves nothing, so make it depend on what you built", c.name));
            continue;
        }
        notes.insert(c.name.clone(), ClaimNote { kind: ClaimKind::Spike, passed: true, metrics: on_branch.metrics });
    }
    if !failures.is_empty() {
        return Err(failures.join("\n"));
    }

    Ok(Evaluation { round, question, answer: file.answer, claims: notes })
}

// --------------------------------------------------------------------------
// Who may touch what
// --------------------------------------------------------------------------

/// Paths in a subagent's finish claim that fall under `tasks/<slug>/` but
/// aren't the subagent's: it may add `claims/` and `findings/` files, only
/// for the current `round` where the name encodes one. Everything outside
/// `tasks/<slug>/` is its to change freely (a spike never merges).
pub fn subagent_path_violations(diffs: &[(char, String)], slug: &str, round: u32) -> Vec<String> {
    let prefix = format!("tasks/{slug}/");
    diffs
        .iter()
        .filter_map(|(_, path)| {
            let rest = path.strip_prefix(&prefix)?;
            let ok = if let Some(name) = rest.strip_prefix("claims/") {
                !is_round_file(name, "toml") || name == format!("{round}.toml")
            } else if let Some(name) = rest.strip_prefix("findings/") {
                !is_round_file(name, "md") || name == format!("{round}.md")
            } else {
                false
            };
            if ok { None } else { Some(path.clone()) }
        })
        .collect()
}

/// Whether an added `path` is allowed in a director's send-back opening round
/// `next`: that round's question, and baseline files for that round (the
/// `<next>.toml` list plus any scripts, but no other round's list).
pub fn send_back_path_ok(path: &str, slug: &str, next: u32) -> bool {
    if path == question_path(slug, next) {
        return true;
    }
    match path.strip_prefix(&format!("tasks/{slug}/baseline/")) {
        Some(name) => !is_round_file(name, "toml") || name == format!("{next}.toml"),
        None => false,
    }
}

/// `name` is `<digits>.<ext>`: the shape of a per-round file.
fn is_round_file(name: &str, ext: &str) -> bool {
    name.strip_suffix(&format!(".{ext}")).is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn d(paths: &[&str]) -> Vec<(char, String)> {
        paths.iter().map(|p| ('A', p.to_string())).collect()
    }

    #[test]
    fn subagent_may_add_current_round_claims_and_findings_and_any_code() {
        let diffs = d(&["src/lib.rs", "tasks/s/claims/2.toml", "tasks/s/claims/run.sh", "tasks/s/findings/2.md", "tasks/s/findings/notes.txt"]);
        assert!(subagent_path_violations(&diffs, "s", 2).is_empty());
    }

    #[test]
    fn subagent_may_not_touch_other_rounds_or_the_directors_files() {
        let diffs = d(&["tasks/s/claims/1.toml", "tasks/s/findings/3.md", "tasks/s/questions/2.toml", "tasks/s/baseline/2.toml", "tasks/s/task.toml", "tasks/s/prompt.md", "tasks/s/reviews/r1.md"]);
        assert_eq!(subagent_path_violations(&diffs, "s", 2).len(), 7);
    }

    #[test]
    fn other_tasks_directories_are_not_this_spikes_business() {
        assert!(subagent_path_violations(&d(&["tasks/other/task.toml"]), "s", 1).is_empty());
    }

    #[test]
    fn send_back_may_add_only_the_next_questions_files() {
        assert!(send_back_path_ok("tasks/s/questions/2.toml", "s", 2));
        assert!(send_back_path_ok("tasks/s/baseline/2.toml", "s", 2));
        assert!(send_back_path_ok("tasks/s/baseline/check.sh", "s", 2));
        assert!(!send_back_path_ok("tasks/s/questions/3.toml", "s", 2));
        assert!(!send_back_path_ok("tasks/s/baseline/3.toml", "s", 2));
        assert!(!send_back_path_ok("src/lib.rs", "s", 2));
        assert!(!send_back_path_ok("tasks/s/claims/2.toml", "s", 2));
    }
}
