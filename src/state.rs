// Derived roadmap/task status (spec.md §10, §12): a pure function of the
// hub's git state (branches, notes, rejections, pins, merges) plus whether a
// task's worker tmux session is alive. Nothing here mutates the hub — the
// player (steps 5-6) does the writing; this module and src/records.rs only
// read it back.

use crate::checks::{self, SuiteResult};
use crate::config::{self, Reason, RoadmapConfig, TaskConfig, TaskKind};
use crate::git::Git;
use crate::records::{self, FlakeRecord, ImpurityRecord, RejectionRecord, Transition};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

// --------------------------------------------------------------------------
// Status (spec §12)
// --------------------------------------------------------------------------

/// `Serialize`/`Deserialize` (snake_case, matching `as_str`) so the player can
/// persist a slug's last-seen status across restarts (`src/events.rs`,
/// `player-state.json`) without re-deriving history it already reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Orphaned,
    Ready,
    /// A `ready` task the director released with a `stage:` marker: its task
    /// agent is launched (a `ready` task never starts on its own); `unstage:`
    /// puts it back, until the agent exists.
    Staged,
    Posted,
    InProgress,
    Tested,
    InRevision,
    /// An accept the player couldn't land: merging `main` conflicts, and the
    /// director must resolve that specific conflict (spec §11).
    Conflicted,
    /// The worker contested a test it must pass (spec §18): the task
    /// agent (or the director) must reply.
    Contested,
    Escalated,
    Accepted,
    /// A concluded spike (spec §14): never merged, its branch kept under
    /// `refs/explored/<slug>`.
    Explored,
    Retired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Orphaned => "orphaned",
            Status::Ready => "ready",
            Status::Staged => "staged",
            Status::Posted => "posted",
            Status::InProgress => "in_progress",
            Status::Tested => "tested",
            Status::InRevision => "in_revision",
            Status::Conflicted => "conflicted",
            Status::Contested => "contested",
            Status::Escalated => "escalated",
            Status::Accepted => "accepted",
            Status::Explored => "explored",
            Status::Retired => "retired",
        }
    }
}

// --------------------------------------------------------------------------
// TaskState / HubState
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TaskState {
    pub slug: String,
    pub config: TaskConfig,
    pub status: Status,
    pub tip: String,
    /// Only non-zero counters (spec: "Strikes count the rejection records
    /// whose scope == slug, per reason").
    pub strikes: BTreeMap<Reason, u32>,
    pub review_round: u32,
    /// Contests answered so far: `tasks/<slug>/contests/c*.md` at the tip.
    pub contest_round: u32,
    /// The worker's reason, set only when `status == Contested`.
    pub contest: Option<String>,
    /// The cause text, set only when `status == Escalated`.
    pub escalation: Option<String>,
    /// The post-mortem body, set only when `status == Retired`.
    pub post_mortem: Option<String>,
    /// Cached check results at `tip`, read-only (never runs anything).
    pub suite: Option<SuiteResult>,
    /// Whether this task's worker tmux session is alive right now — the
    /// same `live_session` call `derive` uses to distinguish `posted` from
    /// `in_progress`, but recorded for every task so the renderer can show
    /// it in the in-flight block even once the task has moved past `posted`
    /// (the session persists across send-backs, spec §9).
    pub alive: bool,
    /// `dashboard relaunch`es of the worker since its last progress
    /// (`records::relaunch_count`), against `[limits] max_relaunches`.
    pub relaunches: u32,
    /// Only for `kind = "spike"` tasks.
    pub spike: Option<SpikeState>,
    /// How many flake-ledger entries this task has.
    pub flaky: u32,
    /// Impurity-ledger entries under this task's scope (spec §20).
    pub impure: u32,
    /// The conflict to resolve, set only when `status == Conflicted`.
    pub conflict: Option<records::ConflictRecord>,
    /// The task's task agent (spec §17), once one was launched.
    pub agent: Option<AgentState>,
}

/// A task agent's runtime state and its latest report on the task's tip.
#[derive(Debug, Clone)]
pub struct AgentState {
    pub alive: bool,
    pub report: Option<records::AgentReport>,
    /// Its relaunches since its last progress, counted apart from the worker's.
    pub relaunches: u32,
}

/// A spike's progress, read from the hub (spec §14).
#[derive(Debug, Clone)]
pub struct SpikeState {
    /// Questions posted so far (the current round); 0 before the post.
    pub round: u32,
    /// The player-verified answer of the latest validated round (or, once
    /// concluded, the final one), straight from the exploration note.
    pub answer: Option<String>,
    pub question: Option<String>,
    pub concluded: bool,
}

#[derive(Debug, Clone)]
pub struct HubState {
    pub roadmap: RoadmapConfig,
    pub main_tip: String,
    pub tasks: Vec<TaskState>,
    pub rejections: Vec<RejectionRecord>,
    /// Checks that passed only on a retry (the flake ledger).
    pub flakes: Vec<FlakeRecord>,
    /// Checks the player found impure (the impurity ledger, spec §20).
    pub impurities: Vec<ImpurityRecord>,
    /// Checks cached as impure on `main`'s tip under main's own `[sandbox]`
    /// policy (spec §20): (check, what it left in the worktree). Until a
    /// task fixes them, work failing only on these is held.
    pub main_impure: Vec<(String, Vec<String>)>,
    /// Why `main`'s cached suite run is over its `[budget]` (a check killed
    /// for its timeout, or the total past `suite_fail`), one line each;
    /// empty when it isn't. Work failing only there is held too.
    pub main_over_budget: Vec<String>,
    /// Every budget raise the director committed (architect-v3.md), oldest first.
    pub budget_raises: Vec<crate::raise::Raise>,
    /// Tasks whose finish is held on those checks (`refs/dashboard/held/*`).
    pub held: Vec<String>,
    /// `dashboard.project` from the hub's git config — used to name tmux
    /// sessions for display, and as the header's fallback title.
    pub project: String,
}

// --------------------------------------------------------------------------
// derive
// --------------------------------------------------------------------------

/// Computes every task's status from the hub. `live_session(slug)` reports
/// whether that task's worker tmux session is currently alive — the one
/// piece of status that isn't pure git state (spec §12: `in_progress` is
/// "runtime, not git").
pub fn derive(hub: &Git, live_session: &dyn Fn(&str) -> bool) -> Result<HubState, String> {
    let main_tip = hub
        .rev_parse("refs/heads/main")
        .ok_or_else(|| "hub has no main".to_string())?;
    let roadmap_text = hub
        .show_file(&main_tip, "roadmap.toml")
        .ok_or_else(|| "roadmap.toml not found at main's tip".to_string())?;
    let roadmap = config::roadmap_config_from_str(&roadmap_text)?;

    let accepted = records::accepted_tasks(hub, &main_tip);
    let retired: Vec<String> = records::retired_slugs(hub);
    let rejections = records::rejections(hub);
    let flakes = records::flakes(hub);
    let impurities = records::impurities(hub);
    let isolation = crate::sandbox::Sandbox::from_config(&roadmap.sandbox)
        .map(|s| s.fingerprint())
        .unwrap_or_default();
    let main_impure = checks::cached(hub, &main_tip)
        .into_iter()
        .filter(|r| r.impurity().is_some() && r.isolation == isolation)
        .map(|r| {
            let evidence = r.impure_evidence();
            (r.name, evidence)
        })
        .collect();
    let main_over_budget = {
        let results: Vec<checks::CheckResult> = checks::cached(hub, &main_tip)
            .into_iter()
            .filter(|r| r.isolation == isolation && r.timeout == roadmap.budget.check_timeout)
            .collect();
        let suite = checks::SuiteResult {
            pass: true,
            checks: results,
            error: None,
        };
        suite
            .budget_overrun(&roadmap.budget)
            .map(|o| {
                let mut lines: Vec<String> = o
                    .timed_out
                    .iter()
                    .map(|n| format!("{n}: killed past check_timeout ({}s)", roadmap.budget.check_timeout))
                    .collect();
                if o.total_over {
                    lines.push(format!(
                        "suite: {} against suite_fail ({}s)",
                        checks::fmt_secs(suite.millis()),
                        roadmap.budget.suite_fail
                    ));
                }
                lines
            })
            .unwrap_or_default()
    };
    let project = hub.config_get("dashboard.project").unwrap_or_default();

    // Every slug we know about: a live task branch, a retirement, or an
    // accepted merge (covering a hand-built or unusual hub where the branch
    // ref itself no longer exists after acceptance).
    let mut slugs: BTreeSet<String> = BTreeSet::new();
    for (refname, _) in hub.for_each_ref("refs/heads/task/*") {
        if let Some(slug) = refname.strip_prefix("refs/heads/task/") {
            slugs.insert(slug.to_string());
        }
    }
    slugs.extend(retired.iter().cloned());
    slugs.extend(records::explored_slugs(hub));
    slugs.extend(accepted.keys().cloned());

    let mut tasks = Vec::with_capacity(slugs.len());
    for slug in &slugs {
        tasks.push(derive_task(
            hub,
            &roadmap,
            slug,
            &accepted,
            &retired,
            &rejections,
            &flakes,
            &impurities,
            live_session,
        )?);
    }

    // Sequential (the default): nothing is `ready` while another task is
    // in flight.
    if roadmap.player.runs == config::Runs::Sequential
        && tasks.iter().any(|t| {
            matches!(
                t.status,
                Status::Staged
                    | Status::Posted
                    | Status::InProgress
                    | Status::Tested
                    | Status::InRevision
                    | Status::Conflicted
                    | Status::Contested
                    | Status::Escalated
            )
        })
    {
        for t in tasks.iter_mut().filter(|t| t.status == Status::Ready) {
            t.status = Status::Pending;
        }
    }

    // Task list order: the order of phases in roadmap.toml, then slug.
    let phase_order: Vec<&str> = roadmap.phases.iter().map(|p| p.id.as_str()).collect();
    tasks.sort_by(|a, b| {
        let a_idx = phase_order
            .iter()
            .position(|p| *p == a.config.phase)
            .unwrap_or(usize::MAX);
        let b_idx = phase_order
            .iter()
            .position(|p| *p == b.config.phase)
            .unwrap_or(usize::MAX);
        a_idx.cmp(&b_idx).then_with(|| a.slug.cmp(&b.slug))
    });

    Ok(HubState {
        roadmap,
        main_tip,
        tasks,
        rejections,
        flakes,
        impurities,
        main_impure,
        main_over_budget,
        budget_raises: records::budget_raises(hub),
        held: records::held_slugs(hub),
        project,
    })
}

/// The tip to read `slug`'s state from: the retirement, else the live task
/// branch, else (branch already gone) the accepted merge on main. `task.toml`
/// is reachable from every one of these, since a retire marker is empty and
/// an accepted merge lands `tasks/<slug>/` on main (spec §8.2).
fn task_tip(
    hub: &Git,
    slug: &str,
    accepted: &BTreeMap<String, String>,
    retired: &[String],
) -> Option<String> {
    if retired.iter().any(|s| s == slug) {
        return hub.rev_parse(&format!("refs/retired/{slug}"));
    }
    if let Some(tip) = hub.rev_parse(&format!("refs/explored/{slug}")) {
        return Some(tip);
    }
    if let Some(tip) = hub.rev_parse(&format!("refs/heads/task/{slug}")) {
        return Some(tip);
    }
    accepted.get(slug).cloned()
}

/// Whether the director's latest `stage:`/`unstage:` on this pending-family
/// branch is a `stage:`. The tip's own note can't say: the player merges
/// `main` into the branch (`merge_unblock`) and a `replan` may follow, each
/// becoming the tip, so walk the branch's own (first-parent) history past
/// those to the last marker; none (the plan) means not staged.
fn staged_at(hub: &Git, tip: &str) -> bool {
    for c in hub.rev_list(&["--first-parent", tip]) {
        match records::import_note(hub, &c).map(|n| n.transition) {
            Some(Transition::MergeUnblock | Transition::Replan) => continue,
            Some(Transition::Stage) => return true,
            _ => return false,
        }
    }
    false
}

fn derive_task(
    hub: &Git,
    roadmap: &RoadmapConfig,
    slug: &str,
    accepted: &BTreeMap<String, String>,
    retired: &[String],
    rejections: &[RejectionRecord],
    flakes: &[FlakeRecord],
    impurities: &[ImpurityRecord],
    live_session: &dyn Fn(&str) -> bool,
) -> Result<TaskState, String> {
    let is_retired = retired.iter().any(|s| s == slug);
    let is_accepted = accepted.contains_key(slug);
    let is_explored = records::is_explored(hub, slug);
    let alive = live_session(slug);
    let agent = records::agent_launched(hub, slug).then(|| AgentState {
        alive: live_session(&agent_key(slug)),
        report: None,
        relaunches: 0,
    });

    let tip = task_tip(hub, slug, accepted, retired)
        .ok_or_else(|| format!("no branch, retirement, or merge found for task {slug}"))?;

    let task_toml_path = format!("tasks/{slug}/task.toml");
    let config_text = hub
        .show_file(&tip, &task_toml_path)
        .ok_or_else(|| format!("{task_toml_path} not found at {tip}"))?;
    let task_config =
        config::task_config_from_str(&config_text).map_err(|e| format!("{task_toml_path}: {e}"))?;

    // ---- strikes (only rejections scoped to this slug; `_main` never counts) ----
    let mut strikes: BTreeMap<Reason, u32> = BTreeMap::new();
    for r in rejections.iter().filter(|r| r.scope == slug) {
        *strikes.entry(r.reason).or_insert(0) += 1;
    }

    // ---- review round: tasks/<slug>/reviews/r*.md files at the tip; for a
    // spike, the send-backs so far are the questions after the first ----
    let is_spike = task_config.kind == TaskKind::Spike;
    let spike = if is_spike {
        let round = records::spike_round(hub, &tip, slug);
        let note = records::latest_exploration(hub, &tip);
        Some(SpikeState {
            round,
            answer: note.as_ref().map(|n| n.answer.clone()),
            question: note.as_ref().map(|n| n.question.clone()),
            concluded: note.as_ref().is_some_and(|n| n.concluded),
        })
    } else {
        None
    };
    let review_round = match &spike {
        Some(sp) => sp.round.saturating_sub(1),
        None => hub
            .ls_tree_files(&tip, &format!("tasks/{slug}/reviews/"))
            .iter()
            .filter(|f| {
                let base = f.rsplit('/').next().unwrap_or("");
                base.starts_with('r') && base.ends_with(".md")
            })
            .count() as u32,
    };

    let contest_round = contest_files(hub, &tip, slug).len() as u32;
    let flaky = flakes.iter().filter(|f| f.scope == slug).count() as u32;
    let impure = impurities.iter().filter(|i| i.scope == slug).count() as u32;
    let suite = cached_suite_summary(hub, &tip);
    let note = records::import_note(hub, &tip);

    // ---- dependency resolution, needed only for the pending family, but
    // cheap enough to always compute ----
    let any_dep_retired = task_config
        .depends_on
        .iter()
        .any(|d| retired.iter().any(|r| r == d));
    let all_deps_accepted = task_config
        .depends_on
        .iter()
        .all(|d| accepted.contains_key(d));

    let mut escalation: Option<String> = None;
    let mut post_mortem: Option<String> = None;
    let mut conflict: Option<records::ConflictRecord> = None;
    let mut contest: Option<String> = None;

    let status = if is_retired {
        post_mortem = hub.commit_body(&tip);
        Status::Retired
    } else if is_accepted {
        Status::Accepted
    } else if is_explored {
        Status::Explored
    } else if let Some(cause) = escalation_cause(hub, roadmap, slug, &strikes, note.as_ref(), &tip)
    {
        escalation = Some(cause);
        Status::Escalated
    } else if let Some(c) = records::conflict(hub, &tip) {
        conflict = Some(c);
        Status::Conflicted
    } else {
        match note.as_ref().map(|n| n.transition) {
            Some(Transition::Finish | Transition::Trim) => Status::Tested,
            Some(Transition::SendBack | Transition::SyncRevision | Transition::ContestReply) => {
                Status::InRevision
            }
            Some(Transition::Contest) => {
                contest = Some(marker_text(hub, &tip, "contest:"));
                Status::Contested
            }
            Some(Transition::Post) => {
                if alive {
                    Status::InProgress
                } else {
                    Status::Posted
                }
            }
            // `plan`, `replan`, `merge_unblock`, `stage`, `unstage`, or no
            // note at all — the pending family. Any other transition found here (accept,
            // retire, docs, merge_accept) is a state the player should
            // never leave a task branch's tip in once its merge/move has
            // completed; treating it as pending-family is a safe fallback
            // rather than a panic. See the final report's ambiguity list.
            _ => {
                if any_dep_retired {
                    Status::Orphaned
                } else if all_deps_accepted {
                    if staged_at(hub, &tip) {
                        Status::Staged
                    } else {
                        Status::Ready
                    }
                } else {
                    Status::Pending
                }
            }
        }
    };

    let agent = agent.map(|a| AgentState {
        report: records::agent_reports(hub, &tip).pop(),
        relaunches: records::relaunch_count(hub, &tip, true),
        ..a
    });
    let relaunches = records::relaunch_count(hub, &tip, false);
    Ok(TaskState {
        slug: slug.to_string(),
        config: task_config,
        status,
        tip,
        strikes,
        review_round,
        contest_round,
        contest,
        escalation,
        post_mortem,
        suite,
        alive,
        relaunches,
        spike,
        flaky,
        impure,
        conflict,
        agent,
    })
}

/// Whether `cfg`'s post and send-backs belong to a task agent (spec §17):
/// task agents are on (`[player] task_agent_cmd`), and it isn't a spike,
/// whose question the director writes with the operator.
pub fn task_agent_managed(roadmap: &RoadmapConfig, cfg: &TaskConfig) -> bool {
    roadmap.player.task_agents_enabled() && cfg.kind != TaskKind::Spike
}

/// Escalation causes that aren't already covered by retired/accepted: a
/// strike counter at its limit, a `block:` marker at the tip, or a standalone
/// escalation record. Checked in that order; spec §10 doesn't rank them
/// against each other when more than one applies.
fn escalation_cause(
    hub: &Git,
    roadmap: &RoadmapConfig,
    slug: &str,
    strikes: &BTreeMap<Reason, u32>,
    note: Option<&records::ImportNote>,
    tip: &str,
) -> Option<String> {
    for reason in Reason::ALL {
        let limit = roadmap.limits.strikes.limit(reason);
        let count = strikes.get(&reason).copied().unwrap_or(0);
        if count >= limit {
            return Some(format!(
                "strike limit: {} {}/{}",
                reason.as_str(),
                count,
                limit
            ));
        }
    }

    if matches!(note, Some(n) if n.transition == Transition::Block) {
        return Some(format!("blocked: {}", marker_text(hub, tip, "block:")));
    }

    if let Some(rec) = records::escalation(hub, slug) {
        return Some(rec.cause);
    }

    None
}

/// A marker commit's explanation: its subject without `prefix`, then its body.
fn marker_text(hub: &Git, commit: &str, prefix: &str) -> String {
    let subject = hub.commit_subject(commit).unwrap_or_default();
    let subject = subject
        .strip_prefix(prefix)
        .map(str::trim_start)
        .unwrap_or(&subject)
        .to_string();
    let body = hub.commit_body(commit).unwrap_or_default();
    let body = body.trim();
    if body.is_empty() {
        subject
    } else {
        format!("{subject}\n{body}")
    }
}

/// The contest replies on a task at `commit`: `tasks/<slug>/contests/c<N>.md`.
pub fn contest_files(hub: &Git, commit: &str, slug: &str) -> Vec<String> {
    hub.ls_tree_files(commit, &format!("tasks/{slug}/contests/"))
        .into_iter()
        .filter(|f| {
            let base = f.rsplit('/').next().unwrap_or("");
            base.strip_prefix('c')
                .and_then(|r| r.strip_suffix(".md"))
                .is_some_and(|n| n.parse::<u32>().is_ok())
        })
        .collect()
}

/// Reads every cached check result at `commit` (never runs anything) and
/// summarizes it the way `checks::SuiteResult` does, so the renderer can
/// treat a live suite run and this cached view the same way.
fn cached_suite_summary(hub: &Git, commit: &str) -> Option<SuiteResult> {
    let results = checks::cached(hub, commit);
    if results.is_empty() {
        return None;
    }
    let pass = results.iter().all(|c| c.pass);
    Some(SuiteResult {
        pass,
        checks: results,
        error: None,
    })
}

// --------------------------------------------------------------------------
// tmux (spec §9's launch/relay mechanism; used by `derive`'s `live_session`
// and by the player, steps 5-6)
// --------------------------------------------------------------------------

/// `dash-<project>-<slug>`, with `.` and `:` (tmux session-name separators)
/// replaced by `-`.
pub fn tmux_session_name(project: &str, slug: &str) -> String {
    fn sanitize(s: &str) -> String {
        s.chars()
            .map(|c| if c == '.' || c == ':' { '-' } else { c })
            .collect()
    }
    format!("dash-{}-{}", sanitize(project), sanitize(slug))
}

/// The `Sessions` key of `slug`'s task agent (spec §17), next to the
/// worker's own key `slug`. `_` never appears in a slug, so it can't
/// collide with another task's worker session.
pub fn agent_key(slug: &str) -> String {
    format!("{slug}_agent")
}

/// Whether the agent in tmux session `name` is currently alive: the
/// session exists *and* its pane's command is still running. Sessions are
/// launched with `remain-on-exit`, so a crashed agent's pane lingers, dead,
/// until the player reaps it (`Sessions::reap`); that's dead here already.
/// `false` if tmux itself isn't installed, not just if the session is
/// missing (`display-message` on a missing target prints nothing).
pub fn tmux_alive(name: &str) -> bool {
    Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            &format!("={name}:"),
            "#{pane_dead}",
        ])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
// Full derivation coverage (real git repos, hand-built hub states) lives in
// tests/state.rs; these unit tests only cover the pure helpers.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_session_name_sanitizes_dots_and_colons() {
        assert_eq!(
            tmux_session_name("my.project", "slug:with:colons"),
            "dash-my-project-slug-with-colons"
        );
        assert_eq!(tmux_session_name("plain", "slug"), "dash-plain-slug");
    }

    #[test]
    fn tmux_alive_is_false_for_a_bogus_session() {
        // Doesn't assert anything about a missing `tmux` binary (environment-
        // dependent), only that a session name nobody used isn't alive.
        assert!(!tmux_alive(
            "dashboard-tests-definitely-not-a-real-session-name"
        ));
    }
}
