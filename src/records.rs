// Provenance and enforcement records the player writes to the hub, plus the
// read side used by both the player and the renderer (spec.md §4, §5, §8.3,
// §9, §10, §11). Everything here is git plumbing: refs under
// `refs/rejected/`, `refs/dashboard/`, `refs/retired/`, and JSON notes on
// `refs/notes/dashboard-*`. No transition validation lives here — that's the
// player's job (steps 5-6); this module only records outcomes and reads them
// back.

use crate::config::{self, ClaimKind, Reason, TaskKind};
use crate::git::Git;
use crate::raise::Raise;
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
    /// A director's `stage:` marker: hold a `ready` task so its task agent
    /// isn't launched.
    Stage,
    /// A director's `unstage:` marker: release a `staged` task.
    Unstage,
    /// A `tested` task's tests the director keeps out of the suite, removed
    /// before accept (spec §6.5): the task stays `tested`.
    Trim,
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
            Transition::Stage => "stage",
            Transition::Unstage => "unstage",
            Transition::Trim => "trim",
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
// Impurity ledger: refs/impure/<scope>/<n> (spec §20)
// --------------------------------------------------------------------------

const IMPURITIES_NOTES_REF: &str = "refs/notes/dashboard-impurities";

/// A check the player found impure: it left files in its worktree, or the
/// sandbox stopped it. The note on a commit is a JSON array of these (one per
/// scope and check), like the flake ledger's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpurityRecord {
    pub scope: String,
    pub check: String,
    /// `CheckResult::impure_evidence`: the files left, or the sandbox lines.
    pub evidence: Vec<String>,
    pub at: u64,
    #[serde(default)]
    pub commit: String,
}

fn read_impurity_note(hub: &Git, commit: &str) -> Vec<ImpurityRecord> {
    hub.notes_show(IMPURITIES_NOTES_REF, commit)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Records that `result` (a check result at `commit`) is impure, under
/// `scope` (a task slug, or `_main`). A no-op for a pure result, and for a
/// (scope, check, commit) already recorded: cached results are re-read
/// every tick.
pub fn record_impurity(
    hub: &Git,
    scope: &str,
    commit: &str,
    result: &crate::checks::CheckResult,
) -> Result<(), String> {
    if result.impurity().is_none() {
        return Ok(());
    }
    let mut entries = read_impurity_note(hub, commit);
    if entries
        .iter()
        .any(|e| e.scope == scope && e.check == result.name)
    {
        return Ok(());
    }
    entries.push(ImpurityRecord {
        scope: scope.to_string(),
        check: result.name.clone(),
        evidence: result.impure_evidence(),
        at: now_unix(),
        commit: String::new(),
    });
    let json = serde_json::to_string(&entries).map_err(|e| e.to_string())?;
    hub.notes_add(IMPURITIES_NOTES_REF, commit, &json)
        .map_err(|e| e.to_string())?;

    let existing = hub.for_each_ref(&format!("refs/impure/{scope}/*"));
    if !existing.iter().any(|(_, sha)| sha == commit) {
        let n = existing
            .iter()
            .filter_map(|(r, _)| r.rsplit('/').next()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        hub.update_ref(&format!("refs/impure/{scope}/{n}"), commit)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Every recorded impurity, oldest first.
pub fn impurities(hub: &Git) -> Vec<ImpurityRecord> {
    let mut out = Vec::new();
    for (refname, sha) in hub.for_each_ref("refs/impure/*/*") {
        let scope = refname
            .trim_start_matches("refs/impure/")
            .rsplit_once('/')
            .map(|(s, _)| s.to_string())
            .unwrap_or_default();
        out.extend(
            read_impurity_note(hub, &sha)
                .into_iter()
                .filter(|e| e.scope == scope)
                .map(|e| ImpurityRecord {
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
    /// Set on a `[limits] max_relaunches` escalation: the session whose
    /// relaunches ran out, `"worker"` or `"agent"`. Only such an escalation
    /// can be cleared (`clear_escalation`, `dashboard relaunch --reset`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relaunch_limit: Option<String>,
}

impl EscalationRecord {
    /// The session a relaunch-limit escalation is for (`Some(true)`: the
    /// task agent), or `None` for any other cause. A record written before
    /// `relaunch_limit` existed is read off its cause text.
    pub fn relaunch_session(&self) -> Option<bool> {
        match self.relaunch_limit.as_deref() {
            Some(s) => Some(s == session_name(true)),
            None if self.cause.starts_with("relaunch limit (") => {
                Some(self.cause.contains("the task agent was relaunched"))
            }
            None => None,
        }
    }
}

/// The name a relaunch-limit record gives a session.
pub fn session_name(agent: bool) -> &'static str {
    if agent { "agent" } else { "worker" }
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
    write_escalation(hub, slug, commit, cause, None)
}

/// `record_escalation` for a session past `[limits] max_relaunches`: the
/// one kind of escalation `clear_escalation` may undo.
pub fn record_relaunch_escalation(
    hub: &Git,
    slug: &str,
    commit: &str,
    cause: &str,
    agent: bool,
) -> Result<EscalationRecord, String> {
    write_escalation(hub, slug, commit, cause, Some(agent))
}

fn write_escalation(
    hub: &Git,
    slug: &str,
    commit: &str,
    cause: &str,
    relaunch_agent: Option<bool>,
) -> Result<EscalationRecord, String> {
    hub.update_ref(&format!("refs/dashboard/escalated/{slug}"), commit)
        .map_err(|e| e.to_string())?;
    let record = EscalationRecord {
        slug: slug.to_string(),
        cause: cause.to_string(),
        at: now_unix(),
        relaunch_limit: relaunch_agent.map(|a| session_name(a).to_string()),
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
// Cleared escalations: refs/dashboard/escalation-cleared/<slug>/<n>
// --------------------------------------------------------------------------

const CLEARED_NOTES_REF: &str = "refs/notes/dashboard-escalations-cleared";

/// An escalation the operator cleared (`dashboard relaunch --reset`), kept
/// as evidence: the escalation ref moves to
/// `refs/dashboard/escalation-cleared/<slug>/<n>`, and this record goes in a
/// list note on the commit it pointed at. A list, and its own notes ref,
/// because the same tip may escalate again and overwrite its
/// `dashboard-escalations` note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearedEscalation {
    pub slug: String,
    /// Which `refs/dashboard/escalation-cleared/<slug>/<n>` this is.
    pub n: u32,
    pub cause: String,
    /// The session whose relaunch limit it was, `"worker"` or `"agent"`.
    pub session: String,
    #[serde(default)]
    pub escalated_at: u64,
    #[serde(default)]
    pub at: u64,
}

/// Clears `slug`'s escalation, which must be a relaunch-limit one for the
/// worker (or, with `agent`, the task agent): the escalation ref moves to
/// `refs/dashboard/escalation-cleared/<slug>/<n>` in one transaction with
/// old-value checks (so an escalation the player records meanwhile is never
/// lost), then a `ClearedEscalation` is noted on its commit. The caller
/// checks it's also the task's *current* cause (`state::escalation_cause`
/// ranks strikes and `block:` above a record).
pub fn clear_escalation(hub: &Git, slug: &str, agent: bool) -> Result<ClearedEscalation, String> {
    let escalated = format!("refs/dashboard/escalated/{slug}");
    let sha = hub
        .rev_parse(&escalated)
        .ok_or_else(|| format!("task/{slug} has no escalation record"))?;
    let rec = escalation(hub, slug)
        .ok_or_else(|| format!("task/{slug}'s escalation record is unreadable"))?;
    if rec.relaunch_session() != Some(agent) {
        return Err(format!(
            "task/{slug} is escalated for {}, not the {}'s relaunch limit",
            rec.cause,
            session_name(agent)
        ));
    }
    let n = cleared_refs(hub, slug)
        .iter()
        .map(|(n, _)| *n)
        .max()
        .unwrap_or(0)
        + 1;
    hub.move_ref_cas(
        &escalated,
        &format!("refs/dashboard/escalation-cleared/{slug}/{n}"),
        &sha,
    )
    .map_err(|e| format!("task/{slug}'s escalation changed meanwhile: {e}"))?;
    let cleared = ClearedEscalation {
        slug: slug.to_string(),
        n,
        cause: rec.cause,
        session: session_name(agent).to_string(),
        escalated_at: rec.at,
        at: now_unix(),
    };
    hub.notes_modify(CLEARED_NOTES_REF, &sha, |old| {
        let mut all: Vec<ClearedEscalation> = old
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        all.push(cleared.clone());
        serde_json::to_string(&all).unwrap_or_default()
    })
    .map_err(|e| e.to_string())?;
    Ok(cleared)
}

fn cleared_refs(hub: &Git, slug: &str) -> Vec<(u32, String)> {
    let prefix = format!("refs/dashboard/escalation-cleared/{slug}/");
    hub.for_each_ref(&format!("{prefix}*"))
        .into_iter()
        .filter_map(|(r, sha)| Some((r.strip_prefix(&prefix)?.parse().ok()?, sha)))
        .collect()
}

/// Every escalation of `slug` the operator cleared, oldest first.
pub fn cleared_escalations(hub: &Git, slug: &str) -> Vec<ClearedEscalation> {
    let mut out: Vec<ClearedEscalation> = cleared_refs(hub, slug)
        .into_iter()
        .filter_map(|(n, sha)| {
            let all: Vec<ClearedEscalation> =
                serde_json::from_str(&hub.notes_show(CLEARED_NOTES_REF, &sha)?).ok()?;
            all.into_iter().find(|c| c.slug == slug && c.n == n)
        })
        .collect();
    out.sort_by_key(|c| c.n);
    out
}

// --------------------------------------------------------------------------
// Relaunches: refs/notes/dashboard-relaunches
// --------------------------------------------------------------------------

const RELAUNCHES_NOTES_REF: &str = "refs/notes/dashboard-relaunches";

/// One `dashboard relaunch`, noted on the task tip it happened at: a commit
/// on the task branch is progress, so a moved tip starts both counts over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaunchRecord {
    /// The task agent's session (`--agent`), else the worker's.
    #[serde(default)]
    pub agent: bool,
    /// Task agent reports noted on the tip at the time: a report collected
    /// since is progress too, so only records matching the current count
    /// count toward the task agent's limit.
    #[serde(default)]
    pub reports: usize,
    /// Not a relaunch: the operator cleared this session's relaunch-limit
    /// escalation (`dashboard relaunch --reset`), so its count starts over
    /// from here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reset: bool,
    #[serde(default)]
    pub at: u64,
}

fn relaunch_records(hub: &Git, tip: &str) -> Vec<RelaunchRecord> {
    hub.notes_show(RELAUNCHES_NOTES_REF, tip)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Relaunches of the worker (or, with `agent`, the task agent) since that
/// session's last progress: noted on `tip` after its last reset and, for
/// the task agent, with no report collected since.
pub fn relaunch_count(hub: &Git, tip: &str, agent: bool) -> u32 {
    let reports = agent_reports(hub, tip).len();
    let records = relaunch_records(hub, tip);
    let start = records
        .iter()
        .rposition(|r| r.reset && r.agent == agent)
        .map_or(0, |i| i + 1);
    records[start..]
        .iter()
        .filter(|r| !r.reset && r.agent == agent && (!agent || r.reports == reports))
        .count() as u32
}

/// Notes one relaunch on `tip`. `dashboard relaunch` runs beside the
/// player, so the append is a `notes_modify` (a re-read on a race).
pub fn record_relaunch(hub: &Git, tip: &str, agent: bool) -> Result<(), String> {
    append_relaunch_record(hub, tip, agent, false)
}

/// Notes a reset on `tip`: the session's relaunches before it stop counting.
pub fn record_relaunch_reset(hub: &Git, tip: &str, agent: bool) -> Result<(), String> {
    append_relaunch_record(hub, tip, agent, true)
}

fn append_relaunch_record(hub: &Git, tip: &str, agent: bool, reset: bool) -> Result<(), String> {
    let record = RelaunchRecord {
        agent,
        reports: agent_reports(hub, tip).len(),
        reset,
        at: now_unix(),
    };
    hub.notes_modify(RELAUNCHES_NOTES_REF, tip, |old| {
        let mut all: Vec<RelaunchRecord> = old
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        all.push(record.clone());
        serde_json::to_string(&all).unwrap_or_default()
    })
    .map_err(|e| e.to_string())
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
// test-speedup finishes: refs/notes/dashboard-speedups
// --------------------------------------------------------------------------

const SPEEDUPS_NOTES_REF: &str = "refs/notes/dashboard-speedups";

/// Notes what a test-speedup finish `commit` removed or rewired (spec
/// §15.1), for the review the task agent and director make of it.
pub fn record_speedup(hub: &Git, commit: &str, text: &str) -> Result<(), String> {
    hub.notes_add(SPEEDUPS_NOTES_REF, commit, text)
        .map_err(|e| e.to_string())
}

/// The note `record_speedup` left on `commit`, if any.
pub fn speedup_notes(hub: &Git, commit: &str) -> Option<String> {
    hub.notes_show(SPEEDUPS_NOTES_REF, commit)
}

// --------------------------------------------------------------------------
// Budget raises (architect-v3.md): refs/notes/dashboard-budget-raises
// --------------------------------------------------------------------------

const RAISES_NOTES_REF: &str = "refs/notes/dashboard-budget-raises";

/// Notes the budget raises the imported `main` commit made.
pub fn record_budget_raises(hub: &Git, commit: &str, raises: &[Raise]) -> Result<(), String> {
    if raises.is_empty() {
        return Ok(());
    }
    let text = serde_json::to_string(raises).map_err(|e| e.to_string())?;
    hub.notes_add(RAISES_NOTES_REF, commit, &text)
        .map_err(|e| e.to_string())
}

/// Every recorded budget raise, oldest first.
pub fn budget_raises(hub: &Git) -> Vec<Raise> {
    let mut out: Vec<Raise> = hub
        .notes_list(RAISES_NOTES_REF)
        .iter()
        .filter_map(|c| hub.notes_show(RAISES_NOTES_REF, c))
        .filter_map(|t| serde_json::from_str::<Vec<Raise>>(&t).ok())
        .flatten()
        .collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.budget.cmp(&b.budget)));
    out
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

/// One report a task agent filed with `dashboard report` (`report::take`
/// renders its block), noted on the task tip it was collected at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReport {
    /// One of `report::Verdict`, checked against the step it closes.
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
/// the Lean package (`architecture/`, the suite files included), every
/// `test_paths` entry of the suite *at that commit*, and everything under
/// `tasks/<slug>/`. A spike pins only
/// `tasks/<slug>/` (spec §14): it never merges, so the project's tests are
/// the worker's to change while exploring.
pub fn pinned_files(hub: &Git, slug: &str) -> Option<BTreeMap<String, String>> {
    let commit = hub.rev_parse(&format!("refs/dashboard/pins/{slug}"))?;
    let mut map = BTreeMap::new();

    // A spike pins no suite; a test-speedup unpins it (the whole test side
    // is its to change): the player's scope check (`test_speedup_scope`)
    // fences it in instead.
    let speedup = is_test_speedup_at(hub, &commit, slug);
    let worker_file = config::worker_tests_file(slug);
    if !is_spike_at(hub, &commit, slug) && !speedup {
        if let Ok(suite) = suite_at(hub, &commit, None) {
            for test_path in &suite.test_paths {
                add_pinned_path(hub, &commit, test_path, &mut map);
            }
            // Every check's own files, but those of the worker's checks.
            for c in suite.checks.iter().filter(|c| c.source.as_deref() != Some(worker_file.as_str())) {
                for f in &c.files {
                    add_pinned_path(hub, &commit, f, &mut map);
                }
            }
        }
    }
    add_pinned_path(hub, &commit, &format!("tasks/{slug}/"), &mut map);

    // The architecture (architect-v3.md): the whole Lean package with its
    // suite files and every API's paths are the
    // director's and task agent's, never a worker's. A spike never merges;
    // a test-speedup owns the suite files.
    if !is_spike_at(hub, &commit, slug) {
        add_pinned_path(hub, &commit, &format!("{}/", config::ARCH_DIR), &mut map);
        if speedup {
            map.retain(|p, _| !config::is_tests_path(p));
        }
        // The worker's own checks are its to write (spec §8.3).
        map.remove(&worker_file);
        if let Some(arch) = arch_at(hub, &commit) {
            for api in arch.api_paths() {
                add_pinned_path(hub, &commit, &api, &mut map);
            }
        }
    }

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

/// The architecture at `commit`, if there is one and it parses: the Lean
/// package's export cached by the player (`arch_source_at`; nothing is run
/// here).
pub fn arch_at(hub: &Git, commit: &str) -> Option<config::ArchConfig> {
    arch_source_at(hub, commit, None).ok().flatten().map(|s| s.cfg)
}

/// The notes ref caching a Lean package's export by its tree id.
pub const ARCH_NOTES: &str = "refs/notes/dashboard-arch";

/// The architecture at `commit` as the raise judge sees it: `Ok(None)` when
/// the commit has none, `Err` when it has one that doesn't load.
pub fn arch_source_at(
    hub: &Git,
    commit: &str,
    exporter: Option<&crate::lean::Exporter>,
) -> Result<Option<crate::raise::Source>, String> {
    Ok(exported_at(hub, commit, exporter)?.map(|e| crate::raise::Source::from_export(&e)))
}

/// The Lean package's export at `commit`: `Ok(None)` when the commit has
/// no `architecture/Architecture.lean`. The export is noted on the
/// package's tree id; on a miss `exporter` runs it (with none, only the
/// note is consulted).
pub fn exported_at(
    hub: &Git,
    commit: &str,
    exporter: Option<&crate::lean::Exporter>,
) -> Result<Option<crate::lean::Exported>, String> {
    if hub.show_file(commit, config::ARCH_LEAN_MANIFEST).is_none() {
        return Ok(None);
    }
    let tree = hub
        .rev_parse(&format!("{commit}:{}", config::ARCH_DIR))
        .ok_or_else(|| format!("{}: no tree at {commit}", config::ARCH_DIR))?;
    if let Some(note) = hub.notes_show(ARCH_NOTES, &tree) {
        return crate::lean::parse(&note).map(Some);
    }
    let Some(exporter) = exporter else {
        return Err(format!("{}: not exported yet (the player exports it on import)", config::ARCH_DIR));
    };
    let e = exporter.export_tree(hub, &tree)?;
    let _ = hub.notes_add(ARCH_NOTES, &tree, &(e.json.to_string() + "\n"));
    Ok(Some(e))
}

/// The notes ref caching run sets by `architecture/` tree id: a JSON map
/// from the sorted, comma-joined module list to the check names.
pub const RUNSETS_NOTES: &str = "refs/notes/dashboard-runsets";

/// The run set of `modules` at `commit` (spec §6.6): Lean's `impacted`,
/// noted on the package's tree; on a miss `exporter` computes it.
pub fn impacted_at(
    hub: &Git,
    commit: &str,
    modules: &[String],
    exporter: Option<&crate::lean::Exporter>,
) -> Result<Vec<String>, String> {
    let tree = hub
        .rev_parse(&format!("{commit}:{}", config::ARCH_DIR))
        .ok_or_else(|| format!("{}: no tree at {commit}", config::ARCH_DIR))?;
    let mut sorted = modules.to_vec();
    sorted.sort();
    let key = sorted.join(",");
    let mut map: BTreeMap<String, Vec<String>> =
        hub.notes_show(RUNSETS_NOTES, &tree).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if let Some(names) = map.get(&key) {
        return Ok(names.clone());
    }
    let Some(exporter) = exporter else {
        return Err(format!("{}: run set of [{key}] not computed yet", config::ARCH_DIR));
    };
    let names = exporter.impacted_tree(hub, &tree, &sorted)?;
    map.insert(key, names.clone());
    let _ = hub.notes_add(RUNSETS_NOTES, &tree, &(serde_json::to_string(&map).unwrap_or_default() + "\n"));
    Ok(names)
}

/// The suite at `commit` (spec §6.2): the checks of `architecture/Tests.lean`
/// and `architecture/Tests/*.lean`, through the export.
pub fn suite_at(hub: &Git, commit: &str, exporter: Option<&crate::lean::Exporter>) -> Result<config::Suite, String> {
    if hub.show_file(commit, config::TESTS_LEAN).is_none() {
        return Err(format!("no {} at {commit}: the suite lives in the Lean package", config::TESTS_LEAN));
    }
    exported_at(hub, commit, exporter)?
        .map(|e| e.suite)
        .ok_or_else(|| format!("no {} at {commit}", config::ARCH_LEAN_MANIFEST))
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

/// Whether `tasks/<slug>/task.toml` at `commit` declares `kind = "test-speedup"`.
pub fn is_test_speedup_at(hub: &Git, commit: &str, slug: &str) -> bool {
    task_config_at(hub, commit, slug).is_some_and(|cfg| cfg.kind == TaskKind::TestSpeedup)
}

/// Every pinned path that `candidate_commit` leaves missing or changed. A
/// slug with no recorded pins has nothing to violate. A new file is never a
/// pins violation: whether the worker may add it is its scope's call
/// (`scope_violations`, spec §8.3).
pub fn check_pins(hub: &Git, slug: &str, candidate_commit: &str) -> Vec<String> {
    let pinned = match pinned_files(hub, slug) {
        Some(m) => m,
        None => return Vec::new(),
    };
    pinned
        .iter()
        .filter_map(
            |(path, expected)| match hub.blob_hash(candidate_commit, path) {
                Some(actual) if actual == *expected => None,
                _ => Some(path.clone()),
            },
        )
        .collect()
}

// --------------------------------------------------------------------------
// Worker scope (spec §8.3)
// --------------------------------------------------------------------------

/// What a task's worker may write and what judges it, read at `tip`.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub modules: Vec<String>,
    /// Paths (files and directory prefixes) the worker may write: each
    /// module's and root's paths, `shared`, its worker file and the `files`
    /// of the checks that file declares.
    pub paths: Vec<String>,
    pub worker_file: String,
    /// Why each of `paths` is there, in step: a module or root name,
    /// `shared`, `your checks` or `files of your checks`.
    pub why: Vec<String>,
    /// The task's run set (check names).
    pub run_set: Vec<String>,
}

/// The scope of `slug`'s worker at `tip`.
pub fn scope_at(hub: &Git, slug: &str, tip: &str, exporter: Option<&crate::lean::Exporter>) -> Result<Scope, String> {
    let cfg = task_config_at(hub, tip, slug).ok_or_else(|| format!("tasks/{slug}/task.toml missing or invalid at {tip}"))?;
    let e = exported_at(hub, tip, exporter)?.ok_or_else(|| format!("no {} at {tip}", config::ARCH_LEAN_MANIFEST))?;
    let worker_file = config::worker_tests_file(slug);
    let mut paths: Vec<String> = Vec::new();
    let mut why: Vec<String> = Vec::new();
    let mut add = |p: &String, w: &str| {
        if !paths.contains(p) {
            paths.push(p.clone());
            why.push(w.to_string());
        }
    };
    for m in &cfg.modules {
        if let Some((_, ps)) = e.arch.node(m) {
            ps.iter().for_each(|p| add(p, m));
        }
    }
    e.arch.shared.iter().for_each(|p| add(p, "shared"));
    add(&worker_file, "your checks");
    for c in e.suite.from_file(&worker_file) {
        c.files.iter().for_each(|p| add(p, "files of your checks"));
    }
    let run_set = if cfg.modules.is_empty() {
        e.suite.checks.iter().map(|c| c.name.clone()).collect()
    } else {
        impacted_at(hub, tip, &cfg.modules, exporter)?
    };
    Ok(Scope { modules: cfg.modules, paths, worker_file, why, run_set })
}

/// What a worker's working tree holds (spec §21), read at `tip`.
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    pub scope: Scope,
    /// Read-only paths with why each is there, in order and deduplicated
    /// against `scope.paths`: `architecture`, `tasks/<slug>`, `api <name>`
    /// (paths and impl files of every API a node in `modules` generates,
    /// consumes or refs), `files of <check>` for each run-set check,
    /// `test_paths`, `.gitignore`, and the agent config (`.claude/`,
    /// `CLAUDE.md`) the worker's agent reads.
    pub readonly: Vec<(String, String)>,
}

/// The working tree of `slug`'s worker at `tip`: its scope plus what it
/// reads (spec §21).
pub fn workspace_at(hub: &Git, slug: &str, tip: &str, exporter: Option<&crate::lean::Exporter>) -> Result<Workspace, String> {
    let scope = scope_at(hub, slug, tip, exporter)?;
    let e = exported_at(hub, tip, exporter)?.ok_or_else(|| format!("no {} at {tip}", config::ARCH_LEAN_MANIFEST))?;
    let mut readonly: Vec<(String, String)> = Vec::new();
    let mut add = |p: &String, w: String| {
        if !scope.paths.contains(p) && !readonly.iter().any(|(q, _)| q == p) {
            readonly.push((p.clone(), w));
        }
    };
    add(&format!("{}/", config::ARCH_DIR), "architecture".into());
    add(&format!("tasks/{slug}/"), format!("tasks/{slug}"));
    for n in &scope.modules {
        for a in crate::arch::view_apis(&e.arch, &e.idls, n) {
            a.paths.iter().chain(&a.impls).for_each(|p| add(p, format!("api {}", a.name)));
        }
    }
    for c in e.suite.checks.iter().filter(|c| scope.run_set.contains(&c.name)) {
        c.files.iter().for_each(|p| add(p, format!("files of {}", c.name)));
    }
    e.suite.test_paths.iter().for_each(|p| add(p, "test_paths".into()));
    add(&".gitignore".to_string(), ".gitignore".into());
    for p in [".claude/", "CLAUDE.md"] {
        add(&p.to_string(), "agent config".into());
    }
    Ok(Workspace { scope, readonly })
}

impl Workspace {
    /// The sparse-checkout patterns (non-cone) of the tree: each path
    /// anchored with a leading `/` and its trailing `/` trimmed, so it
    /// matches the file or the directory with everything under it;
    /// `.gitignore` stays bare and matches at every depth.
    pub fn patterns(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in self.scope.paths.iter().chain(self.readonly.iter().map(|(p, _)| p)) {
            let pat = if p == ".gitignore" { p.clone() } else { sparse_pattern(p) };
            if !out.contains(&pat) {
                out.push(pat);
            }
        }
        out
    }
}

/// Opens the `files` of the checks `slug`'s worker declared, read from
/// `snapshot` of its working tree (spec §21): each one no sparse pattern of
/// `worktree` covers yet is added, so the worker's next `git add -A`
/// stages it. Returns the paths opened; a full tree needs none.
pub fn widen(
    hub: &Git,
    worktree: &Git,
    slug: &str,
    snapshot: &str,
    exporter: Option<&crate::lean::Exporter>,
) -> Result<Vec<String>, String> {
    if !worktree.is_sparse() {
        return Ok(Vec::new());
    }
    let Some(e) = exported_at(hub, snapshot, exporter)? else { return Ok(Vec::new()) };
    let open = worktree.sparse_list();
    let covered = |path: &str| {
        let pat = sparse_pattern(path);
        open.iter().any(|o| pat == *o || pat.starts_with(&format!("{o}/")))
    };
    let mut paths: Vec<String> = Vec::new();
    for c in e.suite.from_file(&config::worker_tests_file(slug)) {
        for f in c.files.iter().filter(|f| !covered(f)) {
            if !paths.contains(f) {
                paths.push(f.clone());
            }
        }
    }
    if !paths.is_empty() {
        let patterns: Vec<String> = paths.iter().map(|p| sparse_pattern(p)).collect();
        worktree.sparse_add(&patterns).map_err(|e| e.to_string())?;
    }
    Ok(paths)
}

/// `path` as an anchored non-cone sparse pattern, glob characters escaped.
pub fn sparse_pattern(path: &str) -> String {
    let mut out = String::from("/");
    for ch in path.trim_start_matches('/').trim_end_matches('/').chars() {
        if matches!(ch, '*' | '?' | '[' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Where `slug`'s task agent (or the director's send-back and contest
/// reply) may write, read at `tip` (spec §17): its own `Tests/<slug>.lean`,
/// `tasks/<slug>/`, the `files` of the checks in the task's run set, the
/// APIs whose generator and consumer are both in its modules (their Lean
/// file and paths), `test_paths` and the roadmap's `[player] lock_files`.
/// Paths and directory prefixes.
pub fn agent_scope_at(hub: &Git, slug: &str, tip: &str, exporter: Option<&crate::lean::Exporter>) -> Vec<String> {
    let mut out = vec![config::tests_file(slug), format!("tasks/{slug}/")];
    let modules = task_config_at(hub, tip, slug).map(|c| c.modules).unwrap_or_default();
    if let Ok(Some(e)) = exported_at(hub, tip, exporter) {
        let names: Vec<String> = if modules.is_empty() {
            e.suite.checks.iter().map(|c| c.name.clone()).collect()
        } else {
            impacted_at(hub, tip, &modules, exporter).unwrap_or_default()
        };
        let names: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
        out.extend(e.suite.files_of(&names));
        for a in e.arch.apis.iter().filter(|a| modules.contains(&a.generator) && modules.contains(&a.consumer)) {
            out.push(crate::lean::api_file(&a.name));
            out.extend(a.paths.iter().cloned());
        }
        out.extend(e.suite.test_paths.iter().cloned());
    }
    if let Some(r) = hub.show_file(tip, "roadmap.toml").and_then(|t| config::roadmap_config_from_str(&t).ok()) {
        out.extend(r.player.lock_files);
    }
    out
}

/// What `candidate` (a worker's finish on `slug`) does beyond its scope at
/// `tip`: a check of its worker file that covers `all` or a node outside
/// `modules`, then every changed path outside the scope (widened by the
/// `files` of the candidate's own worker checks). Empty: within scope.
pub fn scope_violations(
    hub: &Git,
    slug: &str,
    tip: &str,
    candidate: &str,
    exporter: Option<&crate::lean::Exporter>,
) -> Vec<String> {
    let scope = match scope_at(hub, slug, tip, exporter) {
        Ok(s) => s,
        Err(e) => return vec![e],
    };
    let cand = match suite_at(hub, candidate, exporter) {
        Ok(s) => s,
        Err(e) => return vec![e],
    };
    let mut out = Vec::new();
    let mut paths = scope.paths.clone();
    for c in cand.from_file(&scope.worker_file) {
        if c.all {
            out.push(format!("check {} covers all, outside modules", c.name));
        }
        for n in c.covers.iter().filter(|n| !scope.modules.contains(n)) {
            out.push(format!("check {} covers {n}, outside modules", c.name));
        }
        paths.extend(c.files.iter().cloned());
    }
    out.extend(
        hub.diff_names(tip, candidate)
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| !config::path_under_test_paths(p, &paths)),
    );
    out
}

// --------------------------------------------------------------------------
// Held finishes: refs/dashboard/held/<slug>, refs/dashboard/released/<slug>
// (spec §20)
// --------------------------------------------------------------------------

/// Marks `commit`, a worker's finish failing only on checks impure on
/// `main`, as held: neither imported nor rejected.
pub fn set_held(hub: &Git, slug: &str, commit: &str) -> Result<(), String> {
    hub.update_ref(&format!("refs/dashboard/held/{slug}"), commit)
        .map_err(|e| e.to_string())
}

pub fn held_commit(hub: &Git, slug: &str) -> Option<String> {
    hub.rev_parse(&format!("refs/dashboard/held/{slug}"))
}

pub fn held_slugs(hub: &Git) -> Vec<String> {
    hub.for_each_ref("refs/dashboard/held/*")
        .into_iter()
        .filter_map(|(r, _)| r.strip_prefix("refs/dashboard/held/").map(str::to_string))
        .collect()
}

/// Releases `slug`'s hold. With `superseded`, the held commit is left
/// behind by a player merge of `main` into the branch: the worker rebases
/// it, and until then that exact commit is ignored rather than rejected
/// for diverging (`released_commit`).
pub fn release_held(hub: &Git, slug: &str, superseded: bool) -> Result<(), String> {
    if let Some(commit) = held_commit(hub, slug) {
        if superseded {
            hub.update_ref(&format!("refs/dashboard/released/{slug}"), &commit)
                .map_err(|e| e.to_string())?;
        }
        hub.delete_ref(&format!("refs/dashboard/held/{slug}"))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn released_commit(hub: &Git, slug: &str) -> Option<String> {
    hub.rev_parse(&format!("refs/dashboard/released/{slug}"))
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

    #[test]
    fn patterns_are_anchored_non_cone() {
        let scope = Scope {
            paths: vec!["a".into(), "shared.txt".into(), "architecture/Tests/s.worker.lean".into()],
            ..Scope::default()
        };
        let readonly = ["architecture/", "tasks/s/", "stories", ".gitignore", "a"]
            .iter()
            .map(|p| (p.to_string(), "why".to_string()))
            .collect();
        let w = Workspace { scope, readonly };
        assert_eq!(
            w.patterns(),
            ["/a", "/shared.txt", "/architecture/Tests/s.worker.lean", "/architecture", "/tasks/s", "/stories", ".gitignore"]
        );
        assert_eq!(sparse_pattern("x/[a]*?\\b/"), "/x/\\[a]\\*\\?\\\\b");
    }
}
