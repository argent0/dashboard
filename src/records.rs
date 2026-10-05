// Provenance and enforcement records the player writes to the hub, plus the
// read side used by both the player and the renderer (spec.md §4, §5, §8.3,
// §9, §10, §11). Everything here is git plumbing: refs under
// `refs/rejected/`, `refs/dashboard/`, `refs/retired/`, and JSON notes on
// `refs/notes/dashboard-*`. No transition validation lives here — that's the
// player's job (steps 5-6); this module only records outcomes and reads them
// back.

use crate::config::{self, ClaimKind, Reason, TaskKind};
use crate::git::Git;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --------------------------------------------------------------------------
// Actor / Transition (spec §4, §9)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Director,
    Worker,
    Player,
    /// A task's task agent (spec §17): posts and sends back on the
    /// director's behalf, from its own clone.
    TaskAgent,
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
    /// A spike's `conclude:` marker (spec §14).
    Conclude,
    /// The player's merge of `main` into a task branch while landing an
    /// accept (spec §11).
    Sync,
    /// The tip of a branch whose accept merge failed the suite (the
    /// player's sync, or a director's resolution): back to the worker
    /// (`in_revision`), without spending a review round.
    SyncRevision,
    /// A director's resolution of a recorded merge conflict (spec §11).
    Resolve,
    /// A worker's `contest:` marker: a test it must pass is wrong (spec §18).
    Contest,
    /// The reply to a contest (task agent, or director): back to the worker.
    ContestReply,
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
            Transition::Conclude => "conclude",
            Transition::Sync => "sync",
            Transition::SyncRevision => "sync_revision",
            Transition::Resolve => "resolve",
            Transition::Contest => "contest",
            Transition::ContestReply => "contest_reply",
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
pub fn record_import(
    hub: &Git,
    commit: &str,
    actor: Actor,
    transition: Transition,
    slug: Option<&str>,
) -> Result<ImportNote, String> {
    let note = ImportNote {
        actor,
        transition,
        slug: slug.map(|s| s.to_string()),
        at: now_unix(),
    };
    let json = serde_json::to_string(&note).map_err(|e| e.to_string())?;
    hub.notes_add(IMPORTS_NOTES_REF, commit, &json)
        .map_err(|e| e.to_string())?;
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
    hub.for_each_ref(&format!("refs/rejected/{scope}/*"))
        .into_iter()
        .filter_map(|(refname, _)| refname.rsplit('/').next()?.parse::<u32>().ok())
        .collect()
}

/// Stores `commit` under the next free `refs/rejected/<scope>/<n>` and notes
/// the reason. `scope` is the task slug, or `_main` for a director commit to
/// `main` that isn't tied to a task (never counted as a strike — see
/// `state::derive`, which only counts rejections whose scope is a task slug).
pub fn record_rejection(
    hub: &Git,
    scope: &str,
    commit: &str,
    reason: Reason,
    actor: Actor,
    message: &str,
) -> Result<RejectionRecord, String> {
    let n = scope_rejection_ns(hub, scope)
        .into_iter()
        .max()
        .unwrap_or(0)
        + 1;
    let refname = format!("refs/rejected/{scope}/{n}");
    hub.update_ref(&refname, commit)
        .map_err(|e| e.to_string())?;

    let at = now_unix();
    let note = RejectionNote {
        scope: scope.to_string(),
        n,
        reason,
        actor,
        message: message.to_string(),
        at,
    };
    let json = serde_json::to_string(&note).map_err(|e| e.to_string())?;
    hub.notes_add(REJECTIONS_NOTES_REF, commit, &json)
        .map_err(|e| e.to_string())?;

    Ok(RejectionRecord {
        scope: scope.to_string(),
        n,
        reason,
        actor,
        message: message.to_string(),
        at,
        commit: commit.to_string(),
    })
}

/// True when `commit` is stored under any `refs/rejected/<scope>/<n>` — the
/// player never re-evaluates a rejected tip (spec §5).
pub fn is_rejected(hub: &Git, commit: &str) -> bool {
    hub.for_each_ref("refs/rejected/*/*")
        .iter()
        .any(|(_, sha)| sha == commit)
}

/// Every recorded rejection, sorted by time then `n`.
pub fn rejections(hub: &Git) -> Vec<RejectionRecord> {
    let mut out: Vec<RejectionRecord> = hub
        .for_each_ref("refs/rejected/*/*")
        .into_iter()
        .filter_map(|(_, sha)| {
            let text = hub.notes_show(REJECTIONS_NOTES_REF, &sha)?;
            let note: RejectionNote = serde_json::from_str(&text).ok()?;
            Some(RejectionRecord {
                scope: note.scope,
                n: note.n,
                reason: note.reason,
                actor: note.actor,
                message: note.message,
                at: note.at,
                commit: sha,
            })
        })
        .collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then(a.n.cmp(&b.n)));
    out
}

// --------------------------------------------------------------------------
// Flake ledger: refs/flakes/<scope>/<n> (feature request: retry_failed)
// --------------------------------------------------------------------------

const FLAKES_NOTES_REF: &str = "refs/notes/dashboard-flakes";

/// What the player saw when a check passed only after a retry. The note on a
/// commit is a JSON array of these (one per scope and check).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlakeRecord {
    pub scope: String,
    pub check: String,
    pub retries: u32,
    pub retry_failed: u32,
    /// Output tail of each failing attempt before the passing one.
    pub outputs: Vec<String>,
    pub at: u64,
    #[serde(default)]
    pub commit: String,
}

fn read_flake_note(hub: &Git, commit: &str) -> Vec<FlakeRecord> {
    hub.notes_show(FLAKES_NOTES_REF, commit)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Records that `result` (a cached check result at `commit`) passed only on a
/// retry, under `scope` (a task slug, or `_main`). A no-op for a result that
/// didn't, and for a (scope, check, commit) already recorded — the player
/// re-reads cached suites, so this is called repeatedly.
pub fn record_flake(
    hub: &Git,
    scope: &str,
    commit: &str,
    result: &crate::checks::CheckResult,
) -> Result<(), String> {
    if !result.flaky() {
        return Ok(());
    }
    let mut entries = read_flake_note(hub, commit);
    if entries
        .iter()
        .any(|e| e.scope == scope && e.check == result.name)
    {
        return Ok(());
    }
    entries.push(FlakeRecord {
        scope: scope.to_string(),
        check: result.name.clone(),
        retries: result.retries,
        retry_failed: result.retry_failed,
        outputs: result.retry_outputs.clone(),
        at: now_unix(),
        commit: String::new(),
    });
    let json = serde_json::to_string(&entries).map_err(|e| e.to_string())?;
    hub.notes_add(FLAKES_NOTES_REF, commit, &json)
        .map_err(|e| e.to_string())?;

    let existing = hub.for_each_ref(&format!("refs/flakes/{scope}/*"));
    if !existing.iter().any(|(_, sha)| sha == commit) {
        let n = existing
            .iter()
            .filter_map(|(r, _)| r.rsplit('/').next()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        hub.update_ref(&format!("refs/flakes/{scope}/{n}"), commit)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Every recorded flake, oldest first.
pub fn flakes(hub: &Git) -> Vec<FlakeRecord> {
    let mut out = Vec::new();
    for (refname, sha) in hub.for_each_ref("refs/flakes/*/*") {
        let scope = refname
            .trim_start_matches("refs/flakes/")
            .rsplit_once('/')
            .map(|(s, _)| s.to_string())
            .unwrap_or_default();
        out.extend(
            read_flake_note(hub, &sha)
                .into_iter()
                .filter(|e| e.scope == scope)
                .map(|e| FlakeRecord {
                    commit: sha.clone(),
                    ..e
                }),
        );
    }
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.check.cmp(&b.check)));
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
pub fn record_escalation(
    hub: &Git,
    slug: &str,
    commit: &str,
    cause: &str,
) -> Result<EscalationRecord, String> {
    hub.update_ref(&format!("refs/dashboard/escalated/{slug}"), commit)
        .map_err(|e| e.to_string())?;
    let record = EscalationRecord {
        slug: slug.to_string(),
        cause: cause.to_string(),
        at: now_unix(),
    };
    let json = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    hub.notes_add(ESCALATIONS_NOTES_REF, commit, &json)
        .map_err(|e| e.to_string())?;
    Ok(record)
}

/// The escalation record for `slug`, if any.
pub fn escalation(hub: &Git, slug: &str) -> Option<EscalationRecord> {
    let sha = hub.rev_parse(&format!("refs/dashboard/escalated/{slug}"))?;
    let text = hub.notes_show(ESCALATIONS_NOTES_REF, &sha)?;
    serde_json::from_str(&text).ok()
}

// --------------------------------------------------------------------------
// Merge conflicts: refs/notes/dashboard-conflicts (spec §11)
// --------------------------------------------------------------------------

const CONFLICTS_NOTES_REF: &str = "refs/notes/dashboard-conflicts";

/// An accept the player couldn't land because merging `main` into the task
/// conflicts. Noted on the task tip it was found at (the `accept:` marker,
/// or a resolution that hit a newer conflict): `main` is the exact commit a
/// resolution must merge, `paths` the only paths it may resolve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub main: String,
    pub paths: Vec<String>,
    pub at: u64,
}

pub fn record_conflict(
    hub: &Git,
    tip: &str,
    main: &str,
    paths: &[String],
) -> Result<ConflictRecord, String> {
    let record = ConflictRecord {
        main: main.to_string(),
        paths: paths.to_vec(),
        at: now_unix(),
    };
    let json = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    hub.notes_add(CONFLICTS_NOTES_REF, tip, &json)
        .map_err(|e| e.to_string())?;
    Ok(record)
}

/// The conflict noted on `tip`, if any: the task is `conflicted` while its
/// branch tip carries one.
pub fn conflict(hub: &Git, tip: &str) -> Option<ConflictRecord> {
    serde_json::from_str(&hub.notes_show(CONFLICTS_NOTES_REF, tip)?).ok()
}

// --------------------------------------------------------------------------
// Task agents (spec §17): refs/dashboard/agent/<slug>,
// refs/dashboard/agent-asked/<slug>, refs/notes/dashboard-agent-reports
// --------------------------------------------------------------------------

const AGENT_REPORTS_NOTES_REF: &str = "refs/notes/dashboard-agent-reports";

/// Records that `slug`'s task agent was launched, at the task tip `commit`.
pub fn set_agent_launched(hub: &Git, slug: &str, commit: &str) -> Result<(), String> {
    hub.update_ref(&format!("refs/dashboard/agent/{slug}"), commit)
        .map_err(|e| e.to_string())
}

/// Whether `slug`'s task agent was ever launched (it is launched at most once;
/// `dashboard relaunch --agent` restarts it by hand).
pub fn agent_launched(hub: &Git, slug: &str) -> bool {
    hub.ref_exists(&format!("refs/dashboard/agent/{slug}"))
}

/// Records that the task agent was asked to review the tested tip `commit`.
pub fn set_agent_asked(hub: &Git, slug: &str, commit: &str) -> Result<(), String> {
    hub.update_ref(&format!("refs/dashboard/agent-asked/{slug}"), commit)
        .map_err(|e| e.to_string())
}

/// The tested tip the task agent was last asked to review.
pub fn agent_asked(hub: &Git, slug: &str) -> Option<String> {
    hub.rev_parse(&format!("refs/dashboard/agent-asked/{slug}"))
}

/// One report a task agent handed the player (its fixed return block),
/// noted on the task tip it was read at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReport {
    /// `posted`, `accept`, `sent-back`, `unsure`, or `answer` for a reply to
    /// `dashboard ask`; `unknown` when the block has no recognized verdict.
    pub verdict: String,
    pub text: String,
    #[serde(default)]
    pub at: u64,
}

/// Appends `report` to the reports noted on `tip`.
pub fn record_agent_report(
    hub: &Git,
    tip: &str,
    mut report: AgentReport,
) -> Result<AgentReport, String> {
    report.at = now_unix();
    let mut all = agent_reports(hub, tip);
    all.push(report.clone());
    let json = serde_json::to_string(&all).map_err(|e| e.to_string())?;
    hub.notes_add(AGENT_REPORTS_NOTES_REF, tip, &json)
        .map_err(|e| e.to_string())?;
    Ok(report)
}

/// Every report noted on `commit`, oldest first.
pub fn agent_reports(hub: &Git, commit: &str) -> Vec<AgentReport> {
    hub.notes_show(AGENT_REPORTS_NOTES_REF, commit)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

// --------------------------------------------------------------------------
// Pins: refs/dashboard/pins/<slug> (spec §8.3)
// --------------------------------------------------------------------------

/// Records that `slug`'s pins are taken from `commit`.
pub fn set_pins(hub: &Git, slug: &str, commit: &str) -> Result<(), String> {
    hub.update_ref(&format!("refs/dashboard/pins/{slug}"), commit)
        .map_err(|e| e.to_string())
}

/// Whether `slug` has ever had pins recorded — the player's signal that a
/// task was already launched (spec §9's launch row runs at most once).
pub fn has_pins(hub: &Git, slug: &str) -> bool {
    hub.ref_exists(&format!("refs/dashboard/pins/{slug}"))
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
/// `tests.toml`, every suite fragment under `tests.d/`, every `test_paths`
/// entry of the tests.toml *at that commit*, and everything under
/// `tasks/<slug>/`. A spike pins only
/// `tasks/<slug>/` (spec §14): it never merges, so the project's tests are
/// the worker's to change while exploring.
pub fn pinned_files(hub: &Git, slug: &str) -> Option<BTreeMap<String, String>> {
    let commit = hub.rev_parse(&format!("refs/dashboard/pins/{slug}"))?;
    let mut map = BTreeMap::new();

    if !is_spike_at(hub, &commit, slug) {
        add_pinned_path(hub, &commit, "tests.toml", &mut map);
        add_pinned_path(hub, &commit, &format!("{}/", config::SUITE_DIR), &mut map);
        if let Some(text) = hub.show_file(&commit, "tests.toml") {
            if let Ok(cfg) = config::tests_config_from_str(&text) {
                for test_path in &cfg.test_paths {
                    add_pinned_path(hub, &commit, test_path, &mut map);
                }
            }
        }
    }
    add_pinned_path(hub, &commit, &format!("tasks/{slug}/"), &mut map);

    // A flaky-test-fix unpins exactly its `fix_paths` (plain files): the
    // worker replaces the flaky test there. Everything else stays pinned.
    if let Some(cfg) =
        task_config_at(hub, &commit, slug).filter(|c| c.kind == TaskKind::FlakyTestFix)
    {
        for p in &cfg.fix_paths {
            map.remove(p);
        }
    }

    Some(map)
}

/// `tasks/<slug>/task.toml` at `commit`, if it exists and parses.
pub fn task_config_at(hub: &Git, commit: &str, slug: &str) -> Option<config::TaskConfig> {
    hub.show_file(commit, &format!("tasks/{slug}/task.toml"))
        .and_then(|text| config::task_config_from_str(&text).ok())
}

/// Whether `tasks/<slug>/task.toml` at `commit` declares `kind = "spike"`.
pub fn is_spike_at(hub: &Git, commit: &str, slug: &str) -> bool {
    hub.show_file(commit, &format!("tasks/{slug}/task.toml"))
        .and_then(|text| config::task_config_from_str(&text).ok())
        .is_some_and(|cfg| cfg.kind == TaskKind::Spike)
}

/// Every pinned path that `candidate_commit` leaves missing or changed. A
/// slug with no recorded pins has nothing to violate. New files at
/// `candidate_commit` that aren't in the pinned set are never violations,
/// except a new suite fragment (`tests.d/*.toml`) on a non-spike task: the
/// suite is the director's, so adding a check to it is as off-limits as
/// editing `tests.toml`.
pub fn check_pins(hub: &Git, slug: &str, candidate_commit: &str) -> Vec<String> {
    let pinned = match pinned_files(hub, slug) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let mut violations: Vec<String> = pinned
        .iter()
        .filter_map(
            |(path, expected)| match hub.blob_hash(candidate_commit, path) {
                Some(actual) if actual == *expected => None,
                _ => Some(path.clone()),
            },
        )
        .collect();
    let pins_commit = hub
        .rev_parse(&format!("refs/dashboard/pins/{slug}"))
        .unwrap_or_default();
    if !is_spike_at(hub, &pins_commit, slug) {
        let added = hub
            .ls_tree_files(candidate_commit, &format!("{}/", config::SUITE_DIR))
            .into_iter()
            .filter(|p| config::is_suite_fragment(p) && !pinned.contains_key(p));
        violations.extend(added);
    }
    violations
}

// --------------------------------------------------------------------------
// Spike base commit: refs/dashboard/base/<slug> (spec §14)
// --------------------------------------------------------------------------

/// Records the commit a spike's claims are measured against: the tip at
/// launch, which is `main`'s code plus the director's post. Set once and
/// never moved, unlike pins, so every round compares against the same code.
pub fn set_base(hub: &Git, slug: &str, commit: &str) -> Result<(), String> {
    hub.update_ref(&format!("refs/dashboard/base/{slug}"), commit)
        .map_err(|e| e.to_string())
}

pub fn base_commit(hub: &Git, slug: &str) -> Option<String> {
    hub.rev_parse(&format!("refs/dashboard/base/{slug}"))
}

// --------------------------------------------------------------------------
// Explorations: refs/notes/dashboard-explorations (spec §14)
// --------------------------------------------------------------------------

const EXPLORATIONS_NOTES_REF: &str = "refs/notes/dashboard-explorations";

/// What the player recorded about one claim when it validated a finish.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimNote {
    pub kind: ClaimKind,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<serde_json::Value>,
}

/// The player's verified record of one spike round: attached to every
/// validated finish commit, and (with `concluded` set) to the director's
/// `conclude:` commit. The free-form findings stay in the tree; this is the
/// copy `status` reads so it never parses markdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationNote {
    pub slug: String,
    pub round: u32,
    pub question: String,
    pub answer: String,
    #[serde(default)]
    pub claims: BTreeMap<String, ClaimNote>,
    #[serde(default)]
    pub concluded: bool,
    pub at: u64,
}

pub fn record_exploration(
    hub: &Git,
    commit: &str,
    mut note: ExplorationNote,
) -> Result<ExplorationNote, String> {
    note.at = now_unix();
    let json = serde_json::to_string(&note).map_err(|e| e.to_string())?;
    hub.notes_add(EXPLORATIONS_NOTES_REF, commit, &json)
        .map_err(|e| e.to_string())?;
    Ok(note)
}

/// The exploration note attached directly to `commit`, if any.
pub fn exploration(hub: &Git, commit: &str) -> Option<ExplorationNote> {
    let text = hub.notes_show(EXPLORATIONS_NOTES_REF, commit)?;
    serde_json::from_str(&text).ok()
}

/// The newest exploration note on `tip`'s first-parent history: a running
/// spike's latest validated round, or (at a concluded spike's tip) its
/// conclusion.
pub fn latest_exploration(hub: &Git, tip: &str) -> Option<ExplorationNote> {
    hub.rev_list(&["--first-parent", tip])
        .into_iter()
        .find_map(|c| exploration(hub, &c))
}

/// The newest round of `slug`'s spike at `commit`: the highest `<n>` among
/// `tasks/<slug>/questions/<n>.toml` (0 before the post adds the first).
pub fn spike_round(hub: &Git, commit: &str, slug: &str) -> u32 {
    question_rounds(hub, commit, slug)
        .into_iter()
        .max()
        .unwrap_or(0)
}

/// Every `<n>` with a `tasks/<slug>/questions/<n>.toml` at `commit`.
pub fn question_rounds(hub: &Git, commit: &str, slug: &str) -> Vec<u32> {
    let prefix = format!("tasks/{slug}/questions/");
    hub.ls_tree_files(commit, &prefix)
        .iter()
        .filter_map(|f| {
            f.strip_prefix(&prefix)?
                .strip_suffix(".toml")?
                .parse::<u32>()
                .ok()
        })
        .collect()
}

// --------------------------------------------------------------------------
// Explored: refs/explored/<slug> (spec §14)
// --------------------------------------------------------------------------

/// Every slug currently explored (a concluded spike).
pub fn explored_slugs(hub: &Git) -> Vec<String> {
    hub.for_each_ref("refs/explored/*")
        .into_iter()
        .filter_map(|(refname, _)| {
            refname
                .strip_prefix("refs/explored/")
                .map(|s| s.to_string())
        })
        .collect()
}

pub fn is_explored(hub: &Git, slug: &str) -> bool {
    hub.ref_exists(&format!("refs/explored/{slug}"))
}

// --------------------------------------------------------------------------
// Retired: refs/retired/<slug> (spec §8.1, §9)
// --------------------------------------------------------------------------

/// Every slug currently retired.
pub fn retired_slugs(hub: &Git) -> Vec<String> {
    hub.for_each_ref("refs/retired/*")
        .into_iter()
        .filter_map(|(refname, _)| refname.strip_prefix("refs/retired/").map(|s| s.to_string()))
        .collect()
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
        assert_eq!(
            task_trailer("Merge task/foo\n\nTask: foo\n"),
            Some("foo".to_string())
        );
        assert_eq!(task_trailer("no trailer here"), None);
        assert_eq!(
            task_trailer("Task:    padded-slug  \n"),
            Some("padded-slug".to_string())
        );
        assert_eq!(task_trailer("Task: \n"), None);
    }
}
