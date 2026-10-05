// The hub-derived renderer (spec.md §12) — replaces the tasks.yaml renderer
// (src/legacy.rs, removed in this step) now that every task's status is
// computed from the hub (src/state.rs) instead of hand-edited YAML. Keeps
// legacy::render's visual language: the boxed header, bars, icons, and
// ui.rs's color/truncation helpers.

use crate::checks::SuiteResult;
use crate::config::{Reason, TaskKind};
use crate::events::Event;
use crate::records::{Actor, RejectionRecord};
use crate::state::{HubState, Status, TaskState};
use crate::ui::{bar, c, center, style, truncate};
use std::collections::HashSet;

// --------------------------------------------------------------------------
// Status styling (spec §12's icon table)
// --------------------------------------------------------------------------

struct StatusStyle {
    icon: &'static str,
    codes: &'static [&'static str],
}

fn status_style(status: Status) -> StatusStyle {
    match status {
        Status::Accepted => StatusStyle { icon: "✓", codes: &[c::GREEN] },
        Status::Explored => StatusStyle { icon: "◇", codes: &[c::BLUE] },
        Status::Tested => StatusStyle { icon: "◆", codes: &[c::CYAN] },
        Status::InProgress => StatusStyle { icon: "◐", codes: &[c::YELLOW] },
        Status::Posted => StatusStyle { icon: "▶", codes: &[c::YELLOW] },
        Status::InRevision => StatusStyle { icon: "↻", codes: &[c::MAGENTA] },
        Status::Conflicted => StatusStyle { icon: "⇄", codes: &[c::RED] },
        Status::Escalated => StatusStyle { icon: "✗", codes: &[c::RED] },
        Status::Ready => StatusStyle { icon: "●", codes: &[c::CYAN] },
        Status::Pending => StatusStyle { icon: "○", codes: &[c::GRAY] },
        Status::Orphaned => StatusStyle { icon: "⊘", codes: &[c::RED] },
        Status::Retired => StatusStyle { icon: "✝", codes: &[c::GRAY, c::DIM] },
    }
}

/// Live statuses, in the order the overall summary line and per-phase rows
/// display them. `Retired` is deliberately excluded — it's counted (and
/// shown) separately, never part of the live denominator.
const LIVE_STATUS_ORDER: [Status; 10] =
    [Status::Accepted, Status::Tested, Status::InProgress, Status::Posted, Status::InRevision, Status::Conflicted, Status::Escalated, Status::Ready, Status::Pending, Status::Orphaned];

fn actor_label(actor: Actor) -> &'static str {
    match actor {
        Actor::Director => "director",
        Actor::Subagent => "subagent",
        Actor::Player => "player",
        Actor::TaskAgent => "task agent",
    }
}

/// The first line of `s`, with a trailing `…` added only when a later line
/// was actually cut off.
fn first_line_truncated(s: &str) -> String {
    match s.split_once('\n') {
        Some((first, rest)) if !rest.trim().is_empty() => format!("{first}…"),
        Some((first, _)) => first.to_string(),
        None => s.to_string(),
    }
}

fn none_line(color: bool) -> String {
    style("  (none)", &[c::DIM], color)
}

// --------------------------------------------------------------------------
// render
// --------------------------------------------------------------------------

pub fn render(state: &HubState, events: &[Event], color: bool, width: usize) -> String {
    let mut out: Vec<String> = Vec::new();

    // Spikes never merge and nothing depends on them (spec §14), so they stay
    // out of the progress counts, phases, milestones and ready list; they get
    // their own section, and still show up wherever a task is in flight or
    // escalated.
    let is_spike = |t: &&TaskState| t.config.kind == TaskKind::Spike;
    let live: Vec<&TaskState> = state.tasks.iter().filter(|t| t.status != Status::Retired && !is_spike(t)).collect();
    let spikes: Vec<&TaskState> = state.tasks.iter().filter(|t| t.status != Status::Retired && is_spike(t)).collect();
    let retired: Vec<&TaskState> = state.tasks.iter().filter(|t| t.status == Status::Retired).collect();

    // ---- header ----
    let name = state.roadmap.title.clone().unwrap_or_else(|| if state.project.is_empty() { "PROJECT".to_string() } else { state.project.clone() });
    let title = format!(" {name} — Roadmap ");
    let inner_w = (width.saturating_sub(2)).max(title.chars().count());
    out.push(style(&format!("╭{}╮", "─".repeat(inner_w)), &[c::MAGENTA], color));
    out.push(format!(
        "{}{}{}",
        style("│", &[c::MAGENTA], color),
        style(&center(&title, inner_w), &[c::BOLD, c::WHITE], color),
        style("│", &[c::MAGENTA], color)
    ));
    out.push(style(&format!("╰{}╯", "─".repeat(inner_w)), &[c::MAGENTA], color));
    out.push(String::new());

    // ---- overall ----
    let live_total = live.len();
    let accepted = live.iter().filter(|t| t.status == Status::Accepted).count();
    let frac = if live_total > 0 { accepted as f64 / live_total as f64 } else { 0.0 };
    let pct = (frac * 100.0).round() as i64;
    let bar_w = 40.min(width.saturating_sub(30));
    out.push(format!("{}  {}  {}/{} accepted ({}%)", style("Overall", &[c::BOLD], color), bar(frac, bar_w, color), accepted, live_total, pct));

    let mut bits: Vec<String> = LIVE_STATUS_ORDER
        .iter()
        .map(|&s| {
            let n = live.iter().filter(|t| t.status == s).count();
            let ss = status_style(s);
            style(&format!("{} {} {}", ss.icon, n, s.as_str()), ss.codes, color)
        })
        .collect();
    let retired_style = status_style(Status::Retired);
    bits.push(style(&format!("{} {} retired", retired_style.icon, retired.len()), retired_style.codes, color));
    out.push(bits.join("  "));
    out.push(String::new());

    // ---- phases ----
    let phase_ids: HashSet<&str> = state.roadmap.phases.iter().map(|p| p.id.as_str()).collect();
    let hidden_ids: HashSet<&str> = state.roadmap.phases.iter().filter(|p| p.hidden).map(|p| p.id.as_str()).collect();
    for phase in state.roadmap.phases.iter().filter(|p| !p.hidden) {
        let tasks: Vec<&TaskState> = live.iter().filter(|t| t.config.phase == phase.id).copied().collect();
        let header = phase.title.clone().unwrap_or_else(|| phase.id.clone());
        render_phase(&mut out, &header, phase.goal.as_deref(), &tasks, color, width);
    }
    let no_phase: Vec<&TaskState> = live.iter().filter(|t| !phase_ids.contains(t.config.phase.as_str())).copied().collect();
    if !no_phase.is_empty() {
        render_phase(&mut out, "(no phase)", None, &no_phase, color, width);
    }
    if !hidden_ids.is_empty() {
        let n = live.iter().filter(|t| hidden_ids.contains(t.config.phase.as_str())).count();
        let plural = |k: usize| if k == 1 { "" } else { "s" };
        let k = hidden_ids.len();
        out.push(style(&format!("({k} hidden phase{}, {n} task{})", plural(k), plural(n)), &[c::DIM], color));
        out.push(String::new());
    }

    // ---- milestones ----
    let milestones: Vec<&TaskState> = live.iter().filter(|t| t.config.milestone && !hidden_ids.contains(t.config.phase.as_str())).copied().collect();
    out.push(style("⚑ Milestones", &[c::BOLD, c::MAGENTA], color));
    if milestones.is_empty() {
        out.push(none_line(color));
    } else {
        for t in &milestones {
            let ss = status_style(t.status);
            out.push(format!("  {} {} {}", ss.icon, style(&format!("{:<11}", t.status.as_str()), ss.codes, color), truncate(&t.config.title, width.saturating_sub(20))));
        }
    }
    out.push(String::new());

    // ---- ready ("actionable now") ----
    let ready: Vec<&TaskState> = live.iter().filter(|t| t.status == Status::Ready).copied().collect();
    out.push(style(&format!("▶ Ready ({})", ready.len()), &[c::BOLD, c::CYAN], color));
    if ready.is_empty() {
        out.push(none_line(color));
    } else {
        for t in &ready {
            let agent = match &t.agent {
                Some(a) if a.alive => style("  (task agent posting)", &[c::DIM], color),
                Some(_) => style("  (task agent dead)", &[c::RED], color),
                None => String::new(),
            };
            out.push(format!("  {}  {}{agent}", style(&t.slug, &[c::CYAN], color), truncate(&t.config.title, width.saturating_sub(t.slug.chars().count()).saturating_sub(6))));
        }
    }
    out.push(String::new());

    // ---- spikes ----
    if !spikes.is_empty() {
        out.push(style(&format!("◇ Spikes ({})", spikes.len()), &[c::BOLD, c::BLUE], color));
        for t in &spikes {
            let ss = status_style(t.status);
            let detail = spike_detail(t);
            out.push(format!(
                "  {} {} {:<24} {}",
                ss.icon,
                style(&format!("{:<11}", t.status.as_str()), ss.codes, color),
                style(&t.slug, &[c::DIM], color),
                truncate(&detail, width.saturating_sub(42).max(10))
            ));
        }
        out.push(String::new());
    }

    // ---- in flight ----
    let in_flight: Vec<&TaskState> = live.iter().chain(spikes.iter()).filter(|t| matches!(t.status, Status::Posted | Status::InProgress | Status::Tested | Status::InRevision | Status::Conflicted)).copied().collect();
    out.push(style(&if in_flight.len() > 1 { format!("In flight ({})", in_flight.len()) } else { "In flight".to_string() }, &[c::BOLD], color));
    if in_flight.is_empty() {
        out.push(none_line(color));
    }
    for (i, t) in in_flight.iter().enumerate() {
        if i > 0 {
            out.push(String::new());
        }
        render_in_flight(&mut out, state, t, color, width);
    }
    out.push(String::new());

    // ---- escalated ----
    let escalated: Vec<&TaskState> = live.iter().chain(spikes.iter()).filter(|t| t.status == Status::Escalated).copied().collect();
    out.push(style(&format!("✗ Escalated ({})", escalated.len()), &[c::BOLD, c::RED], color));
    if escalated.is_empty() {
        out.push(none_line(color));
    } else {
        for t in &escalated {
            let cause = t.escalation.as_deref().unwrap_or("(no cause recorded)");
            let cause = truncate(&first_line_truncated(cause), width.saturating_sub(t.slug.chars().count()).saturating_sub(4).max(10));
            out.push(format!("  {}  {}", style(&t.slug, &[c::RED], color), cause));
        }
    }
    out.push(String::new());

    // ---- orphaned ----
    let orphaned: Vec<&TaskState> = live.iter().filter(|t| t.status == Status::Orphaned).copied().collect();
    out.push(style(&format!("⊘ Orphaned ({})", orphaned.len()), &[c::BOLD, c::RED], color));
    if orphaned.is_empty() {
        out.push(none_line(color));
    } else {
        for t in &orphaned {
            let dead: Vec<&str> = t
                .config
                .depends_on
                .iter()
                .filter(|d| state.tasks.iter().any(|other| other.slug == **d && other.status == Status::Retired))
                .map(|d| d.as_str())
                .collect();
            let deps = if dead.is_empty() { "a dependency was retired".to_string() } else { format!("retired dep: {}", dead.join(", ")) };
            out.push(format!("  {}  {} — needs a replan", style(&t.slug, &[c::RED], color), style(&deps, &[c::DIM], color)));
        }
    }
    out.push(String::new());

    // ---- recent rejections ----
    out.push(style("Recent rejections", &[c::BOLD], color));
    let recent: Vec<&RejectionRecord> = state.rejections.iter().rev().take(8).collect();
    if recent.is_empty() {
        out.push(none_line(color));
    } else {
        for r in &recent {
            let msg = first_line_truncated(&r.message);
            out.push(format!("  #{} {} {} {} — {}", r.n, r.scope, r.reason.as_str(), actor_label(r.actor), truncate(&msg, width.saturating_sub(20))));
        }
    }
    out.push(String::new());

    // ---- flaky checks (the flake ledger) ----
    if !state.flakes.is_empty() {
        out.push(style(&format!("Flaky checks ({})", state.flakes.len()), &[c::BOLD, c::YELLOW], color));
        for f in state.flakes.iter().rev().take(5) {
            let seen = f.outputs.iter().find_map(|o| o.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("(no output)");
            out.push(format!("  {} {} passed on retry {}/{} — {}", f.scope, f.check, f.retries, f.retry_failed, truncate(seen.trim(), width.saturating_sub(40).max(10))));
        }
        out.push(String::new());
    }

    // ---- retired ----
    out.push(style(&format!("{} Retired ({})", retired_style.icon, retired.len()), retired_style.codes, color));
    if retired.is_empty() {
        out.push(none_line(color));
    } else {
        for t in &retired {
            let mortem = t.post_mortem.as_deref().unwrap_or("(no post-mortem recorded)");
            out.push(format!("  {}  {}", style(&t.slug, retired_style.codes, color), truncate(&first_line_truncated(mortem), width.saturating_sub(t.slug.chars().count()).saturating_sub(4).max(10))));
        }
    }

    // ---- recent events (optional, dim) ----
    if !events.is_empty() {
        out.push(String::new());
        out.push(style("Recent events", &[c::BOLD, c::DIM], color));
        for e in events.iter().rev().take(5) {
            let slug = e.slug.as_deref().unwrap_or("_main");
            let line = format!("  #{} {}  {} — {}", e.seq, e.kind.as_str(), slug, first_line_truncated(&e.message));
            out.push(style(&truncate(&line, width), &[c::DIM], color));
        }
    }

    out.join("\n")
}

// --------------------------------------------------------------------------
// Per-phase block
// --------------------------------------------------------------------------

fn render_phase(out: &mut Vec<String>, header: &str, goal: Option<&str>, tasks: &[&TaskState], color: bool, width: usize) {
    let total = tasks.len();
    let accepted = tasks.iter().filter(|t| t.status == Status::Accepted).count();
    let frac = if total > 0 { accepted as f64 / total as f64 } else { 0.0 };
    out.push(format!("{}  {}  {}/{}", style(header, &[c::BOLD], color), bar(frac, 20, color), accepted, total));
    if let Some(goal) = goal {
        if !goal.is_empty() {
            out.push(format!("  {}", style(&truncate(goal, width.saturating_sub(2)), &[c::DIM], color)));
        }
    }
    for t in tasks {
        let ss = status_style(t.status);
        let mark = if t.config.milestone { " ⚑" } else { "  " };
        let slug = format!("{:<24}", t.slug);
        let status_label = format!("{:<11}", t.status.as_str());
        let title_w = width.saturating_sub(46).max(10);
        let flaky = if t.flaky > 0 { style(&format!("  flaky: {}", t.flaky), &[c::YELLOW], color) } else { String::new() };
        out.push(format!("  {} {} {}{} {}{}", ss.icon, style(&status_label, ss.codes, color), style(&slug, &[c::DIM], color), mark, truncate(&t.config.title, title_w), flaky));
    }
    out.push(String::new());
}

// --------------------------------------------------------------------------
// In-flight block
// --------------------------------------------------------------------------

/// One line about a spike: its title with the question round and, once a
/// round has been validated, the answer the player recorded for it.
fn spike_detail(t: &TaskState) -> String {
    let Some(sp) = &t.spike else { return t.config.title.clone() };
    let progress = match (&sp.answer, sp.concluded) {
        (Some(a), true) => format!("answer: {a} (round {})", sp.round),
        (Some(a), false) => format!("round {}, latest answer: {a}", sp.round),
        (None, _) if sp.round > 0 => format!("round {}", sp.round),
        (None, _) => "not posted".to_string(),
    };
    format!("{progress} — {}", t.config.title)
}

fn render_in_flight(out: &mut Vec<String>, state: &HubState, t: &TaskState, color: bool, width: usize) {
    let ss = status_style(t.status);
    out.push(format!("  {} {}  {}", ss.icon, style(t.status.as_str(), ss.codes, color), style(&t.slug, &[c::BOLD], color)));
    out.push(format!("  {}", truncate(&t.config.title, width.saturating_sub(2))));

    if let Some(conflict) = &t.conflict {
        let line = format!("  conflicts with main {}: {}", &conflict.main[..conflict.main.len().min(12)], conflict.paths.join(", "));
        out.push(style(&truncate(&line, width), &[c::RED], color));
    }

    if let Some(sp) = &t.spike {
        out.push(format!("  spike round {}{}", sp.round, sp.answer.as_ref().map(|a| format!(", latest answer: {a}")).unwrap_or_default()));
    }

    let max_rounds = state.roadmap.limits.max_review_rounds;
    let round_codes: &[&str] = if t.review_round >= max_rounds { &[c::RED] } else { &[] };
    out.push(format!("  round {}", style(&format!("r{}/{}", t.review_round, max_rounds), round_codes, color)));

    let strike_bits: Vec<String> = Reason::ALL
        .iter()
        .filter_map(|&r| {
            let n = t.strikes.get(&r).copied().unwrap_or(0);
            if n == 0 {
                return None;
            }
            let limit = state.roadmap.limits.strikes.limit(r);
            let codes: &[&str] = if n >= limit { &[c::RED] } else { &[] };
            Some(style(&format!("{} {}/{}", r.as_str(), n, limit), codes, color))
        })
        .collect();
    if !strike_bits.is_empty() {
        out.push(format!("  strikes: {}", strike_bits.join(", ")));
    }

    if let Some(suite) = &t.suite {
        if t.spike.is_some() {
            // A spike's cached results are its claims (prefixed `claim:`),
            // not a project suite.
            let claims: Vec<_> = suite.checks.iter().filter(|c| c.name.starts_with("claim:")).cloned().collect();
            if !claims.is_empty() {
                let claims = SuiteResult { pass: claims.iter().all(|c| c.pass), checks: claims, error: None };
                out.push(format!("  claims: {}", format_suite(&claims)));
            }
        } else {
            out.push(format!("  suite: {}", format_suite(suite)));
        }
    }

    let session = crate::state::tmux_session_name(&state.project, &t.slug);
    let alive = if t.alive { style("alive", &[c::GREEN], color) } else { style("dead", &[c::RED], color) };
    out.push(format!("  session: {session} ({alive})"));

    if let Some(agent) = &t.agent {
        let session = crate::state::tmux_session_name(&state.project, &crate::state::agent_key(&t.slug));
        let alive = if agent.alive { style("alive", &[c::GREEN], color) } else { style("dead", &[c::RED], color) };
        let verdict = agent.report.as_ref().map(|r| format!(", reported: {}", r.verdict)).unwrap_or_default();
        out.push(format!("  task agent: {session} ({alive}){verdict}"));
    }
}

fn format_suite(suite: &SuiteResult) -> String {
    if suite.checks.is_empty() {
        return match &suite.error {
            Some(e) => format!("error: {e}"),
            None => "(no cached result)".to_string(),
        };
    }
    suite
        .checks
        .iter()
        .map(|c| {
            let passed = c.exits.iter().filter(|&&e| e == 0).count();
            let retry = match (c.retries, c.pass) {
                (0, _) => String::new(),
                (n, true) => format!(", passed on retry {n}/{}", c.retry_failed),
                (n, false) => format!(", retried {n}/{}", c.retry_failed),
            };
            format!("{}: {} ({}/{}{retry})", c.name, if c.pass { "pass" } else { "fail" }, passed, c.runs)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::CheckResult;
    use crate::config::{Limits, Phase, RoadmapConfig, StrikeLimits, TaskConfig};
    use crate::events::EventKind;
    use std::collections::BTreeMap;

    fn roadmap() -> RoadmapConfig {
        RoadmapConfig {
            title: Some("Widget".to_string()),
            phases: vec![Phase { id: "p1".to_string(), title: Some("Phase One".to_string()), goal: Some("ship the widget".to_string()), hidden: false }],
            docs_paths: vec![],
            limits: Limits { max_review_rounds: 2, strikes: StrikeLimits::default() },
            player: Default::default(),
        }
    }

    fn task(slug: &str, phase: &str, status: Status) -> TaskState {
        TaskState {
            slug: slug.to_string(),
            config: TaskConfig { title: format!("Title for {slug}"), phase: phase.to_string(), milestone: false, depends_on: vec![], check: None, kind: TaskKind::Task, ..Default::default() },
            status,
            tip: "deadbeef".to_string(),
            strikes: BTreeMap::new(),
            review_round: 0,
            escalation: None,
            post_mortem: None,
            suite: None,
            alive: false,
            spike: None,
            flaky: 0,
            conflict: None,
            agent: None,
        }
    }

    fn hub_state(tasks: Vec<TaskState>) -> HubState {
        HubState { roadmap: roadmap(), main_tip: "deadbeef".to_string(), tasks, rejections: vec![], flakes: vec![], project: "widget".to_string() }
    }

    fn render_plain(state: &HubState) -> String {
        render(state, &[], false, 100)
    }

    #[test]
    fn header_uses_roadmap_title() {
        let out = render_plain(&hub_state(vec![]));
        assert!(out.contains("Widget — Roadmap"), "{out}");
    }

    #[test]
    fn header_falls_back_to_project_without_a_title() {
        let mut state = hub_state(vec![]);
        state.roadmap.title = None;
        let out = render_plain(&state);
        assert!(out.contains("widget — Roadmap"), "{out}");
    }

    #[test]
    fn retired_tasks_are_excluded_from_the_progress_denominator() {
        let tasks = vec![task("a", "p1", Status::Accepted), task("b", "p1", Status::Pending), task("dead", "p1", Status::Retired)];
        let out = render_plain(&hub_state(tasks));
        // 1 accepted out of 2 live tasks (the retired one doesn't count).
        assert!(out.contains("1/2 accepted (50%)"), "{out}");
        assert!(out.contains("✝ 1 retired"), "{out}");
    }

    #[test]
    fn phases_list_tasks_and_no_phase_group_catches_the_rest() {
        let tasks = vec![task("in-phase", "p1", Status::Ready), task("stray", "other-phase", Status::Ready)];
        let out = render_plain(&hub_state(tasks));
        assert!(out.contains("Phase One"), "{out}");
        assert!(out.contains("ship the widget"), "{out}");
        assert!(out.contains("in-phase"), "{out}");
        assert!(out.contains("(no phase)"), "{out}");
        assert!(out.contains("stray"), "{out}");
    }

    #[test]
    fn hidden_phases_are_collapsed_but_still_counted() {
        let mut old = task("old-task", "p0", Status::Accepted);
        old.config.milestone = true;
        let tasks = vec![old, task("new-task", "p1", Status::Ready)];
        let mut state = hub_state(tasks);
        state.roadmap.phases.insert(0, Phase { id: "p0".to_string(), title: Some("Phase Zero".to_string()), goal: None, hidden: true });
        let out = render_plain(&state);
        assert!(!out.contains("Phase Zero"), "{out}");
        assert!(!out.contains("old-task"), "{out}");
        assert!(!out.contains("(no phase)"), "{out}");
        assert!(out.contains("(1 hidden phase, 1 task)"), "{out}");
        assert!(out.contains("1/2 accepted (50%)"), "{out}");
        assert!(out.contains("new-task"), "{out}");
    }

    #[test]
    fn retired_tasks_are_not_listed_under_phases() {
        let tasks = vec![task("dead", "p1", Status::Retired)];
        let mut state = hub_state(tasks);
        state.tasks[0].post_mortem = Some("didn't work out\nmore detail".to_string());
        let out = render_plain(&state);
        // "dead" only appears in the Retired section, never as a phase row.
        let phase_block = out.split("⚑ Milestones").next().unwrap();
        assert!(!phase_block.contains("dead"), "{phase_block}");
        assert!(out.contains("didn't work out…"), "{out}");
    }

    #[test]
    fn milestones_section_lists_milestone_tasks() {
        let mut t = task("m1", "p1", Status::Ready);
        t.config.milestone = true;
        let out = render_plain(&hub_state(vec![t]));
        assert!(out.contains("⚑ Milestones"), "{out}");
        assert!(out.contains("m1") || out.contains("Title for m1"), "{out}");
    }

    #[test]
    fn milestones_section_shows_none_when_empty() {
        let out = render_plain(&hub_state(vec![task("x", "p1", Status::Ready)]));
        let section = out.split("⚑ Milestones").nth(1).unwrap();
        let section = section.split("▶ Ready").next().unwrap();
        assert!(section.contains("(none)"), "{section}");
    }

    #[test]
    fn ready_section_lists_ready_tasks_or_none() {
        let out_empty = render_plain(&hub_state(vec![task("x", "p1", Status::Pending)]));
        assert!(out_empty.contains("▶ Ready (0)"), "{out_empty}");
        assert!(out_empty.contains("(none)"), "{out_empty}");

        let out = render_plain(&hub_state(vec![task("go", "p1", Status::Ready)]));
        assert!(out.contains("▶ Ready (1)"), "{out}");
        assert!(out.contains("go"), "{out}");
    }

    #[test]
    fn in_flight_shows_status_round_strikes_suite_and_session() {
        let mut t = task("wip", "p1", Status::Tested);
        t.review_round = 1;
        t.strikes.insert(Reason::Suite, 1);
        t.strikes.insert(Reason::Pins, 0); // zero strikes never show
        t.suite = Some(SuiteResult {
            pass: true,
            checks: vec![CheckResult { name: "unit".to_string(), cmd: "true".to_string(), runs: 1, min_pass: 1, max_pass: None, exits: vec![0], pass: true, output_tail: None, ..Default::default() }],
            error: None,
        });
        t.alive = true;
        let out = render_plain(&hub_state(vec![t]));
        assert!(out.contains("In flight"), "{out}");
        assert!(out.contains("tested"), "{out}");
        assert!(out.contains("wip"), "{out}");
        assert!(out.contains("r1/2"), "{out}");
        assert!(out.contains("suite 1/2"), "{out}");
        assert!(!out.contains("pins 0/2"), "{out}");
        assert!(out.contains("unit: pass (1/1)"), "{out}");
        assert!(out.contains("dash-widget-wip"), "{out}");
        assert!(out.contains("alive"), "{out}");
    }

    #[test]
    fn in_flight_and_ready_show_the_task_agent() {
        let mut t = task("a", "p1", Status::Tested);
        t.agent = Some(crate::state::AgentState { alive: true, report: Some(crate::records::AgentReport { verdict: "accept".to_string(), text: String::new(), at: 0 }) });
        let mut r = task("b", "p1", Status::Ready);
        r.agent = Some(crate::state::AgentState { alive: true, report: None });
        let out = render_plain(&hub_state(vec![t, r]));
        assert!(out.contains("task agent: dash-widget-a_agent (alive), reported: accept"), "{out}");
        assert!(out.contains("(task agent posting)"), "{out}");
    }

    #[test]
    fn in_flight_lists_every_parallel_task() {
        let out = render_plain(&hub_state(vec![task("one", "p1", Status::InProgress), task("two", "p1", Status::Tested), task("done", "p1", Status::Accepted)]));
        let section = out.split("In flight (2)").nth(1).expect("count in header");
        let section = section.split("✗ Escalated").next().unwrap();
        assert!(section.contains("one") && section.contains("two"), "{section}");
        assert!(!section.contains("done"), "{section}");
    }

    #[test]
    fn in_flight_shows_none_when_nothing_is_in_flight() {
        let out = render_plain(&hub_state(vec![task("done", "p1", Status::Accepted)]));
        let section = out.split("In flight").nth(1).unwrap();
        let section = section.split("✗ Escalated").next().unwrap();
        assert!(section.contains("(none)"), "{section}");
    }

    #[test]
    fn escalated_section_truncates_multiline_cause() {
        let mut t = task("boom", "p1", Status::Escalated);
        t.escalation = Some("strike limit: suite 2/2\nmore detail nobody needs here".to_string());
        let out = render_plain(&hub_state(vec![t]));
        assert!(out.contains("✗ Escalated (1)"), "{out}");
        assert!(out.contains("strike limit: suite 2/2…"), "{out}");
        assert!(!out.contains("more detail nobody needs"), "{out}");
    }

    #[test]
    fn orphaned_section_names_the_retired_dependency() {
        let mut orphan = task("orphan", "p1", Status::Orphaned);
        orphan.config.depends_on = vec!["dead-dep".to_string()];
        let dead_dep = task("dead-dep", "p1", Status::Retired);
        let out = render_plain(&hub_state(vec![orphan, dead_dep]));
        assert!(out.contains("⊘ Orphaned (1)"), "{out}");
        assert!(out.contains("orphan"), "{out}");
        assert!(out.contains("retired dep: dead-dep"), "{out}");
        assert!(out.contains("needs a replan"), "{out}");
    }

    #[test]
    fn recent_rejections_show_newest_first_capped_at_eight() {
        let mut state = hub_state(vec![]);
        for i in 1..=10u32 {
            state.rejections.push(RejectionRecord { scope: "x".to_string(), n: i, reason: Reason::Suite, actor: Actor::Subagent, message: format!("attempt {i}"), at: i as u64, commit: format!("sha{i}") });
        }
        let out = render_plain(&state);
        assert!(out.contains("attempt 10"), "{out}");
        assert!(!out.contains("attempt 2\n") && !out.ends_with("attempt 2"), "should have dropped the two oldest: {out}");
        // Newest (10) appears before an older one (9) in the rendered text.
        assert!(out.find("attempt 10").unwrap() < out.find("attempt 9").unwrap(), "{out}");
    }

    #[test]
    fn retired_section_shows_post_mortem_first_line() {
        let mut t = task("dead", "p1", Status::Retired);
        t.post_mortem = Some("the approach was wrong\nlesson learned".to_string());
        let out = render_plain(&hub_state(vec![t]));
        assert!(out.contains("✝ Retired (1)"), "{out}");
        assert!(out.contains("dead"), "{out}");
        assert!(out.contains("the approach was wrong…"), "{out}");
        assert!(!out.contains("lesson learned"), "{out}");
    }

    #[test]
    fn recent_events_are_shown_newest_first_and_capped_at_five() {
        let state = hub_state(vec![]);
        let events: Vec<Event> = (1..=7u64).map(|seq| Event { seq, at: seq, kind: EventKind::Tested, slug: Some("x".to_string()), message: format!("msg {seq}") }).collect();
        let out = render(&state, &events, false, 100);
        assert!(out.contains("Recent events"), "{out}");
        assert!(out.contains("msg 7"), "{out}");
        assert!(!out.contains("msg 1"), "should only keep the last 5: {out}");
        assert!(out.find("msg 7").unwrap() < out.find("msg 3").unwrap(), "{out}");
    }

    #[test]
    fn no_events_omits_the_section() {
        let out = render_plain(&hub_state(vec![]));
        assert!(!out.contains("Recent events"), "{out}");
    }
}
