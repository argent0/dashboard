// Director-relevant events (spec.md's operator requirement: "the director
// must not poll" — see CLAUDE.md and the plan's `dashboard wait` decision).
// The player appends events to `home.events()` (JSON lines) as things happen;
// `dashboard wait` is the director's blocking listener, reading forward from
// `home.director_cursor()`. Nothing here validates anything — that's the
// player's (src/player.rs) job; this module only records and reads events
// back, plus the heartbeat file `dashboard wait` uses to notice a dead
// player.

use crate::paths::Home;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --------------------------------------------------------------------------
// EventKind / Event
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    DirectorRejected,
    Tested,
    Escalated,
    /// An escalated task back in flight: the operator cleared its
    /// relaunch-limit escalation (`dashboard relaunch --reset`).
    Resumed,
    /// An accept the player couldn't land: merging `main` conflicts.
    Conflict,
    /// An accept whose merge with `main` failed the suite: back to the worker.
    MergeFailed,
    /// A worker contested a test and task agents are off: the director replies.
    Contested,
    /// `main`'s suite has impure checks under the current sandbox (spec §20).
    MainImpure,
    /// `main`'s suite is over its `[budget]`: a check killed for its
    /// timeout, or the total past `suite_fail`.
    MainOverBudget,
    /// `main`'s suite fails at its tip: work failing only on those checks
    /// is held until it passes.
    MainFailing,
    /// A finish or accept held, not judged, because it fails only on checks
    /// that are already impure or over budget on `main`.
    Held,
    /// A test-speedup finish passed the ratchet: what it removed and
    /// rewired, for the director to weigh at `accept`.
    SpeedupFinish,
    /// A suite took longer than `[budget] suite_warn`; informational.
    SuiteSlow,
    Ready,
    Accepted,
    Explored,
    Retired,
    SessionDied,
    /// A live worker ended its turn while it still owes a finish (or hit an
    /// API error): only the director or operator can get it going again.
    SessionIdle,
    /// A task agent was launched to post (or review) a task (spec §17).
    AgentLaunched,
    /// A task agent's report: its verdict on a post, review or question.
    AgentReport,
    /// A task agent's session died before its task finished.
    AgentDied,
    /// A live task agent ended its turn while it still owes a post, review
    /// or contest reply, without a report (or hit an API error).
    AgentIdle,
    PlayerError,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::DirectorRejected => "director_rejected",
            EventKind::Tested => "tested",
            EventKind::Escalated => "escalated",
            EventKind::Resumed => "resumed",
            EventKind::Conflict => "conflict",
            EventKind::MergeFailed => "merge_failed",
            EventKind::Contested => "contested",
            EventKind::MainImpure => "main_impure",
            EventKind::MainOverBudget => "main_over_budget",
            EventKind::MainFailing => "main_failing",
            EventKind::Held => "held",
            EventKind::SpeedupFinish => "speedup_finish",
            EventKind::SuiteSlow => "suite_slow",
            EventKind::Ready => "ready",
            EventKind::Accepted => "accepted",
            EventKind::Explored => "explored",
            EventKind::Retired => "retired",
            EventKind::SessionDied => "session_died",
            EventKind::SessionIdle => "session_idle",
            EventKind::AgentLaunched => "agent_launched",
            EventKind::AgentReport => "agent_report",
            EventKind::AgentDied => "agent_died",
            EventKind::AgentIdle => "agent_idle",
            EventKind::PlayerError => "player_error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub at: u64,
    pub kind: EventKind,
    pub slug: Option<String>,
    pub message: String,
}

impl Event {
    /// A one- or two-line human form for `dashboard wait`: `#<seq> <kind>
    /// <slug> — <message>`, plus a per-kind actionable hint (appended inline
    /// when it's short, on its own line when the message itself already
    /// spans more than one line — e.g. a block's explanation, or a
    /// multi-line rejection message).
    pub fn human(&self) -> String {
        let slug = self.slug.as_deref().unwrap_or("_main");
        let header = format!(
            "#{} {}  {} — {}",
            self.seq,
            self.kind.as_str(),
            slug,
            self.message
        );
        match self.hint() {
            Some(hint) if header.contains('\n') => format!("{header}\n{hint}"),
            Some(hint) => format!("{header}; {hint}"),
            None => header,
        }
    }

    fn hint(&self) -> Option<String> {
        const RELAUNCH_CAP: &str = " (relaunches are capped at [limits] max_relaunches since its last progress; one more escalates the task)";
        let slug = self.slug.as_deref().unwrap_or("<slug>");
        match self.kind {
            EventKind::Tested => Some(format!(
                "review it: git fetch hub && git log hub/task/{slug}, then send back, or `dashboard accept {slug} --why \"...\"` (a spike: `dashboard conclude {slug} --why \"...\"`)"
            )),
            EventKind::Escalated if self.message.starts_with("relaunch limit (") => {
                let flag = if self.message.contains("the task agent was relaunched") {
                    " --agent"
                } else {
                    ""
                };
                Some(format!(
                    "take it to the operator: once they've fixed what keeps killing it (agent_cmd, login, a dialog, a usage limit), dashboard relaunch {slug}{flag} --reset resumes the task; else `dashboard retire {slug} --why \"<post-mortem>\"` and replan"
                ))
            }
            EventKind::Escalated => {
                Some(format!("discuss with the operator, then `dashboard retire {slug} --why \"<post-mortem>\"` and replan"))
            }
            EventKind::Resumed => None, // informational: back in flight
            EventKind::Conflict => Some(format!(
                "git fetch hub && git checkout task/{slug} && git merge --ff-only hub/task/{slug}, then git merge <that main commit>, resolve only those paths, commit"
            )),
            EventKind::MergeFailed => Some(format!(
                "the worker fixes it; wait for tested, then review and `dashboard accept {slug}` again"
            )),
            EventKind::Contested => Some(format!(
                "reply on task/{slug} in one commit adding tasks/{slug}/contests/c<N>.md (plus fixed tests if it's right), or `dashboard retire {slug} --why \"...\"`"
            )),
            EventKind::MainImpure => Some(
                "plan a task that makes these checks leave their worktree clean; until it lands, work failing only on them is held".to_string(),
            ),
            EventKind::MainOverBudget => Some(
                "`dashboard draft speedup` in the director clone plans the `test-speedup` naming the slow checks; review and stage it, or raise [budget] in roadmap.toml instead; until it lands, work failing only on them (or slow only because main is) is held".to_string(),
            ),
            EventKind::MainFailing => Some(
                "fix main (a director commit, or a task on the failing checks' modules); until it passes, work failing only on those checks is held".to_string(),
            ),
            EventKind::Held => Some(
                "nothing to do on this task: it's re-judged once main's failing, impure or over-budget checks are fixed".to_string(),
            ),
            EventKind::SpeedupFinish => Some(
                "read what it removed before accepting: removing tests is allowed, but it's your call at `accept`".to_string(),
            ),
            EventKind::SuiteSlow => None, // informational
            EventKind::Ready => Some(format!(
                "with task agents on, start it with `dashboard stage {slug}`; else post it: git fetch hub && git merge (or checkout) hub/task/{slug}"
            )),
            EventKind::DirectorRejected => {
                Some("fetch hub and fix with a new commit / reset".to_string())
            }
            EventKind::Accepted => {
                Some("on main: git fetch hub; git merge --ff-only hub/main".to_string())
            }
            EventKind::Explored => None, // informational: the spike is concluded
            EventKind::Retired => None,  // informational
            EventKind::SessionDied => Some(format!(
                "diagnose from the exit status and last screen before relaunching: a usage/rate limit or API error: wait for the reset, then dashboard relaunch {slug}; auth failure, command not found, a trust/permission dialog or a crash at startup: take it to the operator (a relaunch dies the same way); a one-off crash mid-work: dashboard relaunch {slug}{RELAUNCH_CAP}"
            )),
            EventKind::AgentLaunched => None, // informational: the task agent posts or reviews it
            EventKind::AgentReport => Some(format!(
                "accept: drop the checks its suite line marks drop with `dashboard trim {slug} <check>…`, then `dashboard accept {slug}` (its why becomes the body); unsure: `dashboard ask {slug} \"...\"` or take it to the operator; posted, sent-back, replied, answer: nothing to do"
            )),
            EventKind::AgentDied => Some(format!(
                "diagnose from the exit status and last screen before relaunching: a usage/rate limit or API error: wait for the reset, then dashboard relaunch {slug} --agent; auth failure, command not found, a trust/permission dialog or a crash at startup: take it to the operator (a relaunch dies the same way); a one-off crash mid-work: dashboard relaunch {slug} --agent{RELAUNCH_CAP}"
            )),
            EventKind::SessionIdle => Some(format!(
                "diagnose from the last screen, don't just relaunch: a usage/rate limit or API error: wait for the reset, then the operator nudges it or you kill and relaunch it; a question or confusion: nothing you commit reaches a worker mid-work, so pass it to its task agent (dashboard ask {slug} \"...\") if the brief or tests are wrong, and to the operator to answer in the session; a dialog or auth failure: the operator; an unexplained hang: tmux kill-session -t <that session>, then dashboard relaunch {slug}{RELAUNCH_CAP}"
            )),
            EventKind::AgentIdle => Some(format!(
                "diagnose from the last screen, don't just relaunch: a usage/rate limit or API error: wait for the reset, then nudge it (dashboard ask {slug} \"...\"); a question or confusion: answer it with dashboard ask {slug} \"...\"; a dialog or auth failure: the operator; an unexplained hang: tmux kill-session -t <that session>, then dashboard relaunch {slug} --agent{RELAUNCH_CAP}"
            )),
            EventKind::PlayerError => None, // the message is the whole story
        }
    }
}

// --------------------------------------------------------------------------
// events.jsonl: append / read_all
// --------------------------------------------------------------------------

/// Appends a new event with `seq` = the last recorded seq + 1 (starting at
/// 1), as a single write in append mode. `read_all` (which tolerates
/// malformed lines) is used to find the last seq, so a corrupt earlier line
/// never wedges numbering — it's simply skipped, same as any other reader.
pub fn append(
    home: &Home,
    kind: EventKind,
    slug: Option<&str>,
    message: &str,
) -> Result<Event, String> {
    let seq = read_all(home).last().map(|e| e.seq).unwrap_or(0) + 1;
    let event = Event {
        seq,
        at: now_unix(),
        kind,
        slug: slug.map(|s| s.to_string()),
        message: message.to_string(),
    };
    let json = serde_json::to_string(&event).map_err(|e| e.to_string())?;

    let path = home.events();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(format!("{json}\n").as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(event)
}

/// Every event on file, in order, skipping any line that doesn't parse as an
/// `Event` (a torn write, or hand edits). Returns an empty list if the file
/// doesn't exist yet.
pub fn read_all(home: &Home) -> Vec<Event> {
    let text = match fs::read_to_string(home.events()) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Event>(line).ok())
        .collect()
}

// --------------------------------------------------------------------------
// director.cursor
// --------------------------------------------------------------------------

/// The director's last-read seq, defaulting to 0 (nothing read yet) when the
/// cursor file is missing or unparsable.
pub fn read_cursor(home: &Home) -> u64 {
    fs::read_to_string(home.director_cursor())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

pub fn write_cursor(home: &Home, seq: u64) -> Result<(), String> {
    let path = home.director_cursor();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&path, seq.to_string()).map_err(|e| format!("{}: {e}", path.display()))
}

// --------------------------------------------------------------------------
// player.heartbeat
// --------------------------------------------------------------------------

/// Overwrites the heartbeat file with the current unix time. Called by the
/// player at the end of every tick.
pub fn write_heartbeat(home: &Home) -> Result<(), String> {
    let path = home.heartbeat();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&path, now_unix().to_string()).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_heartbeat(home: &Home) -> Option<u64> {
    fs::read_to_string(home.heartbeat())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// True when the heartbeat is missing or older than 30s — `dashboard wait`'s
/// signal that the player doesn't seem to be running.
pub fn heartbeat_stale(home: &Home) -> bool {
    match read_heartbeat(home) {
        Some(at) => now_unix().saturating_sub(at) > 30,
        None => true,
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn home() -> (TempDir, Home) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        (dir, Home { root })
    }

    #[test]
    fn append_numbers_sequentially_starting_at_one() {
        let (_dir, home) = home();
        let e1 = append(&home, EventKind::Tested, Some("a"), "one").unwrap();
        let e2 = append(&home, EventKind::Accepted, Some("a"), "two").unwrap();
        let e3 = append(&home, EventKind::PlayerError, None, "three").unwrap();
        assert_eq!((e1.seq, e2.seq, e3.seq), (1, 2, 3));
        assert!(e1.at > 0);

        let all = read_all(&home);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].message, "one");
        assert_eq!(all[2].slug, None);
    }

    #[test]
    fn read_all_returns_empty_when_the_file_is_missing() {
        let (_dir, home) = home();
        assert!(read_all(&home).is_empty());
    }

    #[test]
    fn read_all_skips_malformed_lines() {
        let (_dir, home) = home();
        append(&home, EventKind::Tested, Some("a"), "good one").unwrap();
        // Hand-inject a torn/garbage line between two good ones.
        let mut file = OpenOptions::new().append(true).open(home.events()).unwrap();
        file.write_all(b"not json at all\n").unwrap();
        drop(file);
        let e3 = append(&home, EventKind::Retired, Some("a"), "good two").unwrap();
        assert_eq!(
            e3.seq, 2,
            "seq numbering skips the malformed line, continuing from the last valid event"
        );

        let all = read_all(&home);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].message, "good one");
        assert_eq!(all[1].message, "good two");
    }

    #[test]
    fn cursor_defaults_to_zero_and_round_trips() {
        let (_dir, home) = home();
        assert_eq!(read_cursor(&home), 0);
        write_cursor(&home, 5).unwrap();
        assert_eq!(read_cursor(&home), 5);
    }

    #[test]
    fn heartbeat_missing_is_stale_then_fresh_after_write() {
        let (_dir, home) = home();
        assert!(heartbeat_stale(&home));
        write_heartbeat(&home).unwrap();
        assert!(!heartbeat_stale(&home));
    }

    #[test]
    fn human_form_includes_seq_kind_slug_message_and_hint() {
        let e = Event {
            seq: 12,
            at: 1,
            kind: EventKind::Tested,
            slug: Some("add-login".to_string()),
            message: "finish accepted".to_string(),
        };
        let human = e.human();
        assert!(
            human.starts_with("#12 tested  add-login — finish accepted"),
            "{human}"
        );
        assert!(human.contains("git fetch hub"), "{human}");

        let retired = Event {
            seq: 1,
            at: 1,
            kind: EventKind::Retired,
            slug: Some("dead".to_string()),
            message: "didn't pan out".to_string(),
        };
        assert_eq!(retired.human(), "#1 retired  dead — didn't pan out");
    }

    #[test]
    fn session_health_hints_diagnose_first_and_never_say_tmux_attach() {
        for kind in [
            EventKind::SessionDied,
            EventKind::AgentDied,
            EventKind::SessionIdle,
            EventKind::AgentIdle,
        ] {
            let e = Event {
                seq: 1,
                at: 1,
                kind,
                slug: Some("s".to_string()),
                message: "task/s's session died\nlast screen:\n  boom".to_string(),
            };
            let hint = e.hint().expect("a session-health event has a hint");
            assert!(hint.starts_with("diagnose from"), "{hint}");
            assert!(hint.contains("last screen"), "{hint}");
            assert!(hint.contains("usage/rate limit"), "{hint}");
            assert!(hint.contains("operator"), "{hint}");
            assert!(hint.contains("dashboard relaunch s"), "{hint}");
            assert!(hint.contains("max_relaunches"), "{hint}");
            assert!(!hint.contains("tmux attach"), "{hint}");
            assert!(e.human().ends_with(&format!("\n{hint}")), "{}", e.human());
        }
    }

    #[test]
    fn only_a_relaunch_limit_escalation_hints_at_reset() {
        let escalated = |message: &str| Event {
            seq: 1,
            at: 1,
            kind: EventKind::Escalated,
            slug: Some("s".to_string()),
            message: message.to_string(),
        };
        let hint = escalated("relaunch limit (2) reached: the task agent was relaunched 2 times since its last progress and died again").hint().unwrap();
        assert!(
            hint.contains("dashboard relaunch s --agent --reset"),
            "{hint}"
        );
        let hint = escalated("relaunch limit (2) reached: the worker was relaunched 2 times since its last progress and died again").hint().unwrap();
        assert!(hint.contains("dashboard relaunch s --reset"), "{hint}");
        let hint = escalated("strike limit: suite 2/2").hint().unwrap();
        assert!(!hint.contains("--reset"), "{hint}");
        assert!(hint.contains("retire"), "{hint}");
    }
}
