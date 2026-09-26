// Provenance and enforcement records the player writes to the hub, plus the
// read side used by both the player and the renderer (spec.md §4, §5, §8.3,
// §9, §10, §11). Everything here is git plumbing: refs under
// `refs/rejected/`, `refs/dashboard/`, `refs/retired/`, and JSON notes on
// `refs/notes/dashboard-*`. No transition validation lives here — that's the
// player's job (steps 5-6); this module only records outcomes and reads them
// back.
#![allow(dead_code)]

use crate::config::{self, Reason};
use crate::git::Git;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// --------------------------------------------------------------------------
// Actor / Transition (spec §4, §9)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Director,
    Subagent,
    Player,
}

/// The kind of commit an import note attaches to. Director-on-main paths
/// (`docs`) and player-only merges (`merge_unblock`, `merge_accept`) are
/// included alongside the per-task transitions of spec §9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    Plan,
    Replan,
    Post,
    Finish,
    Block,
    SendBack,
    Accept,
    Retire,
    Docs,
    MergeUnblock,
    MergeAccept,
}

impl Transition {
    pub fn as_str(self) -> &'static str {
        match self {
            Transition::Plan => "plan",
            Transition::Replan => "replan",
            Transition::Post => "post",
            Transition::Finish => "finish",
            Transition::Block => "block",
            Transition::SendBack => "send_back",
            Transition::Accept => "accept",
            Transition::Retire => "retire",
            Transition::Docs => "docs",
            Transition::MergeUnblock => "merge_unblock",
            Transition::MergeAccept => "merge_accept",
        }
    }
}

// --------------------------------------------------------------------------
// Import notes: refs/notes/dashboard-imports (spec §4)
// --------------------------------------------------------------------------

const IMPORTS_NOTES_REF: &str = "refs/notes/dashboard-imports";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportNote {
    pub actor: Actor,
    pub transition: Transition,
    pub slug: Option<String>,
    pub at: u64,
}

/// Records provenance for a commit the player just imported (or created
/// itself, for its own merges).
pub fn record_import(hub: &Git, commit: &str, actor: Actor, transition: Transition, slug: Option<&str>) -> Result<ImportNote, String> {
    let note = ImportNote { actor, transition, slug: slug.map(|s| s.to_string()), at: now_unix() };
    let json = serde_json::to_string(&note).map_err(|e| e.to_string())?;
    hub.notes_add(IMPORTS_NOTES_REF, commit, &json).map_err(|e| e.to_string())?;
    Ok(note)
}

/// Reads back the import note on `commit`, if any.
pub fn import_note(hub: &Git, commit: &str) -> Option<ImportNote> {
    let text = hub.notes_show(IMPORTS_NOTES_REF, commit)?;
    serde_json::from_str(&text).ok()
}

// --------------------------------------------------------------------------
// Rejections: refs/rejected/<scope>/<n> (spec §5, §10)
// --------------------------------------------------------------------------

const REJECTIONS_NOTES_REF: &str = "refs/notes/dashboard-rejections";

/// The JSON payload stored on the rejected commit's note. `scope` and `n` are
/// duplicated here (rather than read back from the ref name alone) so a
/// listing built from notes never needs to re-parse ref names.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RejectionNote {
    scope: String,
    n: u32,
    reason: Reason,
    actor: Actor,
    message: String,
    at: u64,
}

#[derive(Debug, Clone)]
pub struct RejectionRecord {
    pub scope: String,
    pub n: u32,
    pub reason: Reason,
    pub actor: Actor,
    pub message: String,
    pub at: u64,
    pub commit: String,
}

/// Every `n` currently used under `refs/rejected/<scope>/`.
fn scope_rejection_ns(hub: &Git, scope: &str) -> Vec<u32> {
    hub.for_each_ref(&format!("refs/rejected/{scope}/*")).into_iter().filter_map(|(refname, _)| refname.rsplit('/').next()?.parse::<u32>().ok()).collect()
}

/// Stores `commit` under the next free `refs/rejected/<scope>/<n>` and notes
/// the reason. `scope` is the task slug, or `_main` for a director commit to
/// `main` that isn't tied to a task (never counted as a strike — see
/// `state::derive`, which only counts rejections whose scope is a task slug).
pub fn record_rejection(hub: &Git, scope: &str, commit: &str, reason: Reason, actor: Actor, message: &str) -> Result<RejectionRecord, String> {
    let n = scope_rejection_ns(hub, scope).into_iter().max().unwrap_or(0) + 1;
    let refname = format!("refs/rejected/{scope}/{n}");
    hub.update_ref(&refname, commit).map_err(|e| e.to_string())?;

    let at = now_unix();
    let note = RejectionNote { scope: scope.to_string(), n, reason, actor, message: message.to_string(), at };
    let json = serde_json::to_string(&note).map_err(|e| e.to_string())?;
    hub.notes_add(REJECTIONS_NOTES_REF, commit, &json).map_err(|e| e.to_string())?;

    Ok(RejectionRecord { scope: scope.to_string(), n, reason, actor, message: message.to_string(), at, commit: commit.to_string() })
}

/// True when `commit` is stored under any `refs/rejected/<scope>/<n>` — the
/// player never re-evaluates a rejected tip (spec §5).
pub fn is_rejected(hub: &Git, commit: &str) -> bool {
    hub.for_each_ref("refs/rejected/*/*").iter().any(|(_, sha)| sha == commit)
}

/// Every recorded rejection, sorted by time then `n`.
pub fn rejections(hub: &Git) -> Vec<RejectionRecord> {
    let mut out: Vec<RejectionRecord> = hub
        .for_each_ref("refs/rejected/*/*")
        .into_iter()
        .filter_map(|(_, sha)| {
            let text = hub.notes_show(REJECTIONS_NOTES_REF, &sha)?;
            let note: RejectionNote = serde_json::from_str(&text).ok()?;
            Some(RejectionRecord { scope: note.scope, n: note.n, reason: note.reason, actor: note.actor, message: note.message, at: note.at, commit: sha })
        })
        .collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then(a.n.cmp(&b.n)));
    out
}

// --------------------------------------------------------------------------
// Escalation records: refs/dashboard/escalated/<slug> (spec §10)
// --------------------------------------------------------------------------

const ESCALATIONS_NOTES_REF: &str = "refs/notes/dashboard-escalations";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRecord {
    pub slug: String,
    pub cause: String,
    pub at: u64,
}

/// Records an escalation whose cause isn't a strike limit or a block (e.g. a
/// send-back that would exceed `max_review_rounds`). `commit` is the
/// offending commit, which is deliberately NOT imported onto the task
/// branch.
pub fn record_escalation(hub: &Git, slug: &str, commit: &str, cause: &str) -> Result<EscalationRecord, String> {
    hub.update_ref(&format!("refs/dashboard/escalated/{slug}"), commit).map_err(|e| e.to_string())?;
    let record = EscalationRecord { slug: slug.to_string(), cause: cause.to_string(), at: now_unix() };
    let json = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    hub.notes_add(ESCALATIONS_NOTES_REF, commit, &json).map_err(|e| e.to_string())?;
    Ok(record)
}

/// The escalation record for `slug`, if any.
pub fn escalation(hub: &Git, slug: &str) -> Option<EscalationRecord> {
    let sha = hub.rev_parse(&format!("refs/dashboard/escalated/{slug}"))?;
    let text = hub.notes_show(ESCALATIONS_NOTES_REF, &sha)?;
    serde_json::from_str(&text).ok()
}

// --------------------------------------------------------------------------
// Pins: refs/dashboard/pins/<slug> (spec §8.3)
// --------------------------------------------------------------------------

/// Records that `slug`'s pins are taken from `commit`.
pub fn set_pins(hub: &Git, slug: &str, commit: &str) -> Result<(), String> {
    hub.update_ref(&format!("refs/dashboard/pins/{slug}"), commit).map_err(|e| e.to_string())
}

/// Adds every file git resolves `path` to at `commit` (a single file, or
/// every file under a directory prefix) to `map`, keyed by path with its
/// blob hash. `ls_tree_files` already treats a plain file path and a
/// directory prefix uniformly, so no separate blob/tree check is needed.
fn add_pinned_path(hub: &Git, commit: &str, path: &str, map: &mut BTreeMap<String, String>) {
    for file in hub.ls_tree_files(commit, path) {
        if let Some(hash) = hub.blob_hash(commit, &file) {
            map.insert(file, hash);
        }
    }
}

/// The pinned path -> blob hash map for `slug`, computed fresh from the
/// commit `set_pins` last recorded (`None` if pins were never taken):
/// `tests.toml`, every `test_paths` entry of the tests.toml *at that
/// commit*, and everything under `tasks/<slug>/`.
pub fn pinned_files(hub: &Git, slug: &str) -> Option<BTreeMap<String, String>> {
    let commit = hub.rev_parse(&format!("refs/dashboard/pins/{slug}"))?;
    let mut map = BTreeMap::new();

    add_pinned_path(hub, &commit, "tests.toml", &mut map);
    if let Some(text) = hub.show_file(&commit, "tests.toml") {
        if let Ok(cfg) = config::tests_config_from_str(&text) {
            for test_path in &cfg.test_paths {
                add_pinned_path(hub, &commit, test_path, &mut map);
            }
        }
    }
    add_pinned_path(hub, &commit, &format!("tasks/{slug}/"), &mut map);

    Some(map)
}

/// Every pinned path that `candidate_commit` leaves missing or changed. A
/// slug with no recorded pins has nothing to violate. New files at
/// `candidate_commit` that aren't in the pinned set are never violations.
pub fn check_pins(hub: &Git, slug: &str, candidate_commit: &str) -> Vec<String> {
    let pinned = match pinned_files(hub, slug) {
        Some(m) => m,
        None => return Vec::new(),
    };
    pinned
        .into_iter()
        .filter_map(|(path, expected)| match hub.blob_hash(candidate_commit, &path) {
            Some(actual) if actual == expected => None,
            _ => Some(path),
        })
        .collect()
}

// --------------------------------------------------------------------------
// Retired: refs/retired/<slug> (spec §8.1, §9)
// --------------------------------------------------------------------------

/// Every slug currently retired.
pub fn retired_slugs(hub: &Git) -> Vec<String> {
    hub.for_each_ref("refs/retired/*").into_iter().filter_map(|(refname, _)| refname.strip_prefix("refs/retired/").map(|s| s.to_string())).collect()
}

pub fn is_retired(hub: &Git, slug: &str) -> bool {
    hub.ref_exists(&format!("refs/retired/{slug}"))
}

/// The retire marker commit's body (the post-mortem), if `slug` is retired.
pub fn post_mortem(hub: &Git, slug: &str) -> Option<String> {
    let sha = hub.rev_parse(&format!("refs/retired/{slug}"))?;
    hub.commit_body(&sha)
}

// --------------------------------------------------------------------------
// Accepted: `Task: <slug>` merges on main (spec §9, §11, §12)
// --------------------------------------------------------------------------

/// Walks `main_tip`'s first-parent history for merge commits with a
/// `Task: <slug>` trailer line, returning slug -> merge commit sha. Only the
/// first (most recent) merge found for a slug is kept — a slug is merged at
/// most once.
pub fn accepted_tasks(hub: &Git, main_tip: &str) -> BTreeMap<String, String> {
    // %x01 separates the hash from the body; %x02 separates commits. Both are
    // control characters that never appear in ordinary commit text.
    let log = match hub.run(&["log", "--first-parent", "--format=%H%x01%B%x02", main_tip]) {
        Ok(out) => out,
        Err(_) => return BTreeMap::new(),
    };

    let mut map = BTreeMap::new();
    for entry in log.split('\u{2}') {
        let mut parts = entry.splitn(2, '\u{1}');
        let sha = match parts.next() {
            Some(s) if !s.is_empty() => s.trim_start_matches('\n'),
            _ => continue,
        };
        let body = parts.next().unwrap_or("");
        if let Some(slug) = task_trailer(body) {
            map.entry(slug).or_insert_with(|| sha.to_string());
        }
    }
    map
}

/// The `Task: <slug>` trailer value from a commit body, if present.
fn task_trailer(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("Task:") {
            let slug = rest.trim();
            if !slug.is_empty() {
                return Some(slug.to_string());
            }
        }
    }
    None
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
// Everything here needs a real git repo (refs, notes); covered by
// tests/state.rs and tests/records.rs-style coverage folded into it. These
// unit tests only cover the pure helper.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_trailer_extracts_slug() {
        assert_eq!(task_trailer("Merge task/foo\n\nTask: foo\n"), Some("foo".to_string()));
        assert_eq!(task_trailer("no trailer here"), None);
        assert_eq!(task_trailer("Task:    padded-slug  \n"), Some("padded-slug".to_string()));
        assert_eq!(task_trailer("Task: \n"), None);
    }
}
