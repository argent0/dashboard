// The original tasks.yaml renderer. Kept working unchanged while the
// director/player/subagent model (spec.md) is built alongside it; step 8 of
// the migration plan replaces this module and its call site in main.rs.
//
// Statuses: pending, in_progress (first pass), in_revision (sent back by a
// coordinator review for corrections), built, tested, blocked. Optional
// per-task `reviews:` list records each coordinator assessment as
// "r<N> <sent-back|accepted|escalated> <date> <notes>". The REVIEW_CAP-th
// sent-back round is flagged as the final one: if that revision still isn't
// right, the coordinator escalates to a human instead of sending it back again.

use crate::ui::{c, bar, center, style, truncate};
use serde::Deserialize;
use std::collections::HashMap;

// --------------------------------------------------------------------------
// tasks.yaml schema
// --------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct Roadmap {
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) phases: Vec<Phase>,
    #[serde(default)]
    pub(crate) tasks: Vec<Task>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Phase {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    goal: Option<String>,
}

fn default_status() -> String {
    "pending".to_string()
}

#[derive(Debug, Deserialize)]
pub(crate) struct Task {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default = "default_status")]
    status: String,
    #[serde(default)]
    phase: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    milestone: bool,
    #[serde(default)]
    reviews: Vec<String>,
    #[serde(default)]
    blocked_reason: Option<String>,
}

// --------------------------------------------------------------------------
// Status styling
// --------------------------------------------------------------------------

struct StatusStyle {
    icon: &'static str,
    color: &'static str,
    label: &'static str,
}

fn status_style(status: &str) -> StatusStyle {
    match status {
        "tested" => StatusStyle { icon: "✓", color: c::GREEN, label: "tested" },
        "built" => StatusStyle { icon: "◆", color: c::CYAN, label: "built" },
        "in_progress" => StatusStyle { icon: "◐", color: c::YELLOW, label: "in_progress" },
        "in_revision" => StatusStyle { icon: "↻", color: c::MAGENTA, label: "in_revision" },
        "blocked" => StatusStyle { icon: "✗", color: c::RED, label: "blocked" },
        _ => StatusStyle { icon: "○", color: c::GRAY, label: "pending" },
    }
}

const STATUS_ORDER: [&str; 6] = ["tested", "built", "in_progress", "in_revision", "blocked", "pending"];

// --------------------------------------------------------------------------
// Rendering
// --------------------------------------------------------------------------

// A `reviews:` entry is the coordinator's assessment of one pass over a task:
// "r<N> <sent-back|accepted|escalated> <date> <free text>". Sent-back rounds
// are capped: after the REVIEW_CAP-th, the next step is a human, not a round.
const REVIEW_CAP: usize = 2;

/// Matches `^r(\d+)\s+(sent-back|accepted|escalated)\b`, returning the verdict.
fn review_verdict(s: &str) -> Option<&'static str> {
    let rest = s.strip_prefix('r')?;
    let digit_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if digit_end == 0 {
        return None;
    }
    let after_digits = &rest[digit_end..];
    let ws_len = after_digits.chars().take_while(|c| c.is_whitespace()).count();
    if ws_len == 0 {
        return None;
    }
    let after_ws = &after_digits[ws_len..];
    for verdict in ["sent-back", "accepted", "escalated"] {
        if let Some(after) = after_ws.strip_prefix(verdict) {
            let boundary_ok = match after.chars().next() {
                None => true,
                Some(ch) => !(ch.is_alphanumeric() || ch == '_'),
            };
            if boundary_ok {
                return Some(match verdict {
                    "sent-back" => "sent-back",
                    "accepted" => "accepted",
                    _ => "escalated",
                });
            }
        }
    }
    None
}

pub fn render(data: &Roadmap, color: bool, width: usize) -> String {
    let phases = &data.phases;
    let tasks = &data.tasks;
    let by_id: HashMap<&str, &Task> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();

    let mut out: Vec<String> = Vec::new();

    let sent_back_rounds = |t: &Task| -> usize { t.reviews.iter().filter(|r| review_verdict(r) == Some("sent-back")).count() };

    let last_verdict = |t: &Task| -> Option<&'static str> { t.reviews.iter().rev().find_map(|r| review_verdict(r)) };

    let cap_hit = |t: &Task| -> bool { sent_back_rounds(t) >= REVIEW_CAP && last_verdict(t) == Some("sent-back") };

    let actionable = |t: &Task| -> bool {
        if t.status != "pending" {
            return false;
        }
        t.depends_on.iter().all(|d| by_id.get(d.as_str()).map(|dep| dep.status == "tested").unwrap_or(false))
    };

    let unmet_deps = |t: &Task| -> Vec<String> {
        t.depends_on.iter().filter(|d| by_id.get(d.as_str()).map(|dep| dep.status != "tested").unwrap_or(true)).cloned().collect()
    };

    // ---- header ----
    let project = data.title.as_deref().unwrap_or("PROJECT");
    let title = format!(" {project} — Implementation Roadmap ");
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

    // ---- overall progress ----
    let total = tasks.len();
    let mut counts: HashMap<&str, usize> = STATUS_ORDER.iter().map(|s| (*s, 0)).collect();
    for t in tasks {
        let key = STATUS_ORDER.iter().find(|&&o| o == t.status).copied().unwrap_or("pending");
        *counts.entry(key).or_insert(0) += 1;
    }
    let tested = *counts.get("tested").unwrap_or(&0);
    let frac = if total > 0 { tested as f64 / total as f64 } else { 0.0 };
    let pct = (frac * 100.0).round() as i64;

    let bar_w = 40.min(width.saturating_sub(30));
    let summary_bits = STATUS_ORDER
        .iter()
        .map(|s| {
            let ss = status_style(s);
            style(&format!("{} {} {}", ss.icon, counts.get(s).unwrap_or(&0), ss.label), &[ss.color], color)
        })
        .collect::<Vec<_>>()
        .join("  ");
    out.push(format!("{}  {}  {}/{} tested ({}%)", style("Overall", &[c::BOLD], color), bar(frac, bar_w, color), tested, total, pct));
    out.push(summary_bits);
    out.push(String::new());

    // ---- phases ----
    for ph in phases {
        let ph_tasks: Vec<&Task> = tasks.iter().filter(|t| t.phase == ph.id).collect();
        let ph_total = ph_tasks.len();
        let ph_tested = ph_tasks.iter().filter(|t| t.status == "tested").count();
        let ph_frac = if ph_total > 0 { ph_tested as f64 / ph_total as f64 } else { 0.0 };
        let header = ph.title.clone().unwrap_or_else(|| ph.id.clone());
        out.push(format!("{}  {}  {}/{}", style(&header, &[c::BOLD], color), bar(ph_frac, 20, color), ph_tested, ph_total));
        let goal = ph.goal.as_deref().unwrap_or("");
        if !goal.is_empty() {
            out.push(format!("  {}", style(&truncate(goal, width.saturating_sub(2)), &[c::DIM], color)));
        }
        for t in &ph_tasks {
            let ss = status_style(&t.status);
            let mark = if t.milestone { " ⚑" } else { "  " };
            let tid = format!("{:<24}", t.id);
            let tstatus = format!("{:<11}", t.status);
            let n_rev = t.reviews.len();
            let badge = if n_rev > 0 {
                format!("{} ", style(&format!("r{n_rev}"), &[if cap_hit(t) { c::RED } else { c::MAGENTA }], color))
            } else {
                String::new()
            };
            let badge_w = if n_rev > 0 { format!("r{n_rev} ").chars().count() } else { 0 };
            let title_w = (width.saturating_sub(46).saturating_sub(badge_w)).max(10);
            let row = format!("  {} {} {}{} {}", ss.icon, style(&tstatus, &[ss.color], color), style(&tid, &[c::DIM], color), mark, badge + &truncate(&t.title, title_w));
            out.push(row);
        }
        out.push(String::new());
    }

    // ---- milestones ----
    let milestones: Vec<&Task> = tasks.iter().filter(|t| t.milestone).collect();
    if !milestones.is_empty() {
        out.push(style("⚑ Milestones", &[c::BOLD, c::MAGENTA], color));
        for t in &milestones {
            let ss = status_style(&t.status);
            out.push(format!("  {} {} {}", ss.icon, style(&format!("{:<11}", t.status), &[ss.color], color), truncate(&t.title, width.saturating_sub(20))));
        }
        out.push(String::new());
    }

    // ---- actionable now ("next", computed) ----
    let next_tasks: Vec<&Task> = tasks.iter().filter(|t| actionable(t)).collect();
    out.push(style(&format!("▶ Actionable now ({})", next_tasks.len()), &[c::BOLD, c::CYAN], color));
    if !next_tasks.is_empty() {
        for t in &next_tasks {
            out.push(format!("  {}  {}", style(&t.id, &[c::CYAN], color), truncate(&t.title, width.saturating_sub(t.id.chars().count()).saturating_sub(6))));
        }
    } else {
        out.push(style("  (none — everything actionable is in progress, or nothing is unblocked)", &[c::DIM], color));
    }
    out.push(String::new());

    // ---- in revision (sent back by the coordinator's review) ----
    let revising: Vec<&Task> = tasks.iter().filter(|t| t.status == "in_revision").collect();
    out.push(style(&format!("↻ In revision ({})", revising.len()), &[c::BOLD, c::MAGENTA], color));
    if !revising.is_empty() {
        for t in &revising {
            let last = t.reviews.last().cloned().unwrap_or_else(|| "no review recorded".to_string());
            let warn = if cap_hit(t) {
                style(&format!("  ⚠ final round ({REVIEW_CAP}/{REVIEW_CAP}) — escalate if still not right"), &[c::RED], color)
            } else {
                String::new()
            };
            let last_w = (width.saturating_sub(t.id.chars().count()).saturating_sub(8)).max(20);
            out.push(format!("  {}{}{}", style(&t.id, &[c::MAGENTA], color), warn, style(&format!("  — {}", truncate(&last, last_w)), &[c::DIM], color)));
        }
    } else {
        out.push(style("  (none)", &[c::DIM], color));
    }
    out.push(String::new());

    // ---- blocked ----
    let blocked: Vec<&Task> = tasks.iter().filter(|t| t.status == "blocked").collect();
    out.push(style(&format!("⛔ Blocked ({})", blocked.len()), &[c::BOLD, c::RED], color));
    if !blocked.is_empty() {
        for t in &blocked {
            let reason = match t.blocked_reason.as_deref() {
                Some(r) if !r.is_empty() => r.to_string(),
                _ => {
                    let deps = unmet_deps(t);
                    if deps.is_empty() { "no reason recorded".to_string() } else { format!("waiting on: {}", deps.join(", ")) }
                }
            };
            out.push(format!("  {}  {}{}", style(&t.id, &[c::RED], color), truncate(&t.title, 40), style(&format!("  — {reason}"), &[c::DIM], color)));
        }
    } else {
        out.push(style("  (none)", &[c::DIM], color));
    }

    out.join("\n")
}
