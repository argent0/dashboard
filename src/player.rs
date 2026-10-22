// The player (spec.md §4-§5, §9, §11): the only writer of the hub. Each tick
// fetches the director's clone, imports and validates its commits onto the
// hub's refs, and runs the player's own merges. Step 5a covered the
// skeleton, history rules, director `main` (docs-only commits) and `task/*`
// branches' plan/replan transitions, and unblock merges. Step 5b filled in
// the rest of the director-side transitions on an existing branch: post,
// send-back, accept and retire, plus the marker-commit dispatch
// (`accept:`/`retire:`, and rejecting any other empty commit as `ownership`
// since only the worker may write `block:`). Every validation is against
// the *cumulative* diff from the hub's current tip to the candidate, exactly
// as in 5a, so a fixing commit on top of a rejected one still has a chance.
// Step 6 (this step) adds the worker side (spec §7, §8.3, §9's launch row):
// launching a worker clone + tmux session right after a task is posted,
// importing and validating its finish/block claims (only the newest commit
// in the fetched range is ever validated — each one is a whole finish claim,
// spec's "Import semantics"), and relaying worker rejections and
// send-backs into its tmux session. Step 7 adds events and `dashboard wait`.

use crate::checks;
use crate::config::{self, Reason, RoadmapConfig, TaskConfig, TaskKind, path_under_test_paths};
use crate::events::{self, EventKind};
use crate::flaky;
use crate::speedup;
use crate::fsm::{self, Move, Verdict};
use crate::git::Git;
use crate::paths::{self, Home};
use crate::prompts::{self, Prompts};
use crate::queue;
use crate::raise;
use crate::records::{self, Actor, RejectionRecord, Transition};
use crate::sandbox::Sandbox;
use crate::spike;
use crate::state::{self, Status};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// --------------------------------------------------------------------------
// Sessions: the runtime tmux side (spec §9's launch/relay), abstracted so
// tests never need a real tmux. Step 6 adds launch and the tmux impl; this
// step only needs `alive` (for `state::derive`) and stubs the rest.
// --------------------------------------------------------------------------

pub trait Sessions {
    fn alive(&self, slug: &str) -> bool;
    /// Starts the worker's session: `agent_cmd` run in `clone_dir` with
    /// `prompt_file`'s contents as its first message (spec §9's launch row).
    fn launch(
        &mut self,
        slug: &str,
        clone_dir: &Path,
        agent_cmd: &str,
        prompt_file: &Path,
    ) -> Result<(), String>;
    fn send(&mut self, slug: &str, message: &str);
    fn kill(&mut self, slug: &str);
    /// The session's visible screen, or `None` when it can't be read (the
    /// session is gone): what pane-staleness idle detection hashes.
    fn capture(&self, slug: &str) -> Option<String>;
    /// Clears away a dead session. A session's pane outlives its command
    /// (tmux's `remain-on-exit`), so a crash's last output survives until
    /// this reads it, with the exit status, and ends the session (freeing
    /// its name for a relaunch). `None` when there's nothing to reap: no
    /// such session, or a live one (never touched).
    fn reap(&mut self, slug: &str) -> Option<Remains>;
}

/// What a dead session left behind (`Sessions::reap`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Remains {
    /// The dead pane's last output, `None` if it couldn't be read.
    pub screen: Option<String>,
    /// The command's exit status, if it exited.
    pub status: Option<i32>,
    /// The signal that killed it, if one did.
    pub signal: Option<i32>,
}

/// A `Sessions` that records every `launch`/`send`/`kill` call so tests can
/// assert on them instead of touching tmux, with a configurable `alive` set
/// so a test can mark a session dead again (or never alive at all) to drive
/// the relaunch/no-relaunch paths. `launch` inserts the slug into `alive`
/// (unless it's in `stillborn`: a session that dies before any tick sees
/// it); `kill` removes it — a test after that point can remove it directly
/// too, to simulate a session dying on its own. `panes` is what `capture`
/// returns per live session (a live one missing from it shows ""). A dead
/// session whose pane lingers is one in `lingering` (and not in `alive`):
/// what `reap` returns once, recording the slug in `reaped`. A launch or
/// kill clears it, as replacing or killing the tmux session would.
#[derive(Debug, Default)]
pub struct NullSessions {
    pub sent: Vec<(String, String)>,
    pub killed: Vec<String>,
    pub launched: Vec<LaunchCall>,
    pub alive: BTreeSet<String>,
    pub stillborn: BTreeSet<String>,
    pub panes: BTreeMap<String, String>,
    pub lingering: BTreeMap<String, Remains>,
    pub reaped: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LaunchCall {
    pub slug: String,
    pub clone_dir: PathBuf,
    pub agent_cmd: String,
    pub prompt_file: PathBuf,
}

impl Sessions for NullSessions {
    fn alive(&self, slug: &str) -> bool {
        self.alive.contains(slug)
    }

    fn launch(
        &mut self,
        slug: &str,
        clone_dir: &Path,
        agent_cmd: &str,
        prompt_file: &Path,
    ) -> Result<(), String> {
        self.launched.push(LaunchCall {
            slug: slug.to_string(),
            clone_dir: clone_dir.to_path_buf(),
            agent_cmd: agent_cmd.to_string(),
            prompt_file: prompt_file.to_path_buf(),
        });
        self.lingering.remove(slug);
        if !self.stillborn.contains(slug) {
            self.alive.insert(slug.to_string());
        }
        Ok(())
    }

    fn send(&mut self, slug: &str, message: &str) {
        self.sent.push((slug.to_string(), message.to_string()));
    }

    fn kill(&mut self, slug: &str) {
        self.killed.push(slug.to_string());
        self.alive.remove(slug);
        self.lingering.remove(slug);
    }

    fn capture(&self, slug: &str) -> Option<String> {
        self.alive
            .contains(slug)
            .then(|| self.panes.get(slug).cloned().unwrap_or_default())
    }

    fn reap(&mut self, slug: &str) -> Option<Remains> {
        if self.alive.contains(slug) {
            return None;
        }
        let remains = self.lingering.remove(slug)?;
        self.reaped.push(slug.to_string());
        Some(remains)
    }
}

// --------------------------------------------------------------------------
// TmuxSessions: the real backend (spec §9's launch/relay mechanism)
// --------------------------------------------------------------------------

pub struct TmuxSessions {
    project: String,
}

impl TmuxSessions {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
        }
    }
}

impl Sessions for TmuxSessions {
    fn alive(&self, slug: &str) -> bool {
        state::tmux_alive(&state::tmux_session_name(&self.project, slug))
    }

    /// `tmux new-session -d -s <name> -c <clone_dir> sh -c '<agent_cmd>
    /// "$(cat <prompt_file>)"' \; set-option -w -t =<name>: remain-on-exit
    /// on`. `clone_dir` and `agent_cmd` are passed as plain argv elements
    /// (no shell involved for the outer `tmux` call), but `prompt_file` is
    /// interpolated into the inner `sh -c` string, so it's the one path that
    /// needs shell quoting. `remain-on-exit` (in the same command list, so
    /// the server sets it before it can see the command exit) keeps a
    /// crashed agent's pane, dead, for `reap`. A dead session still holding
    /// the name (a relaunch before the player reaped it) is killed first.
    fn launch(
        &mut self,
        slug: &str,
        clone_dir: &Path,
        agent_cmd: &str,
        prompt_file: &Path,
    ) -> Result<(), String> {
        let name = state::tmux_session_name(&self.project, slug);
        let clone_str = clone_dir
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", clone_dir.display()))?;
        let prompt_str = prompt_file
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", prompt_file.display()))?;
        let inner = format!("{agent_cmd} \"$(cat {})\"", shell_quote(prompt_str));
        if !self.alive(slug) {
            self.kill(slug);
        }
        let target = format!("={name}:");
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &name,
                "-c",
                clone_str,
                "sh",
                "-c",
                &inner,
                ";",
                "set-option",
                "-w",
                "-t",
                &target,
                "remain-on-exit",
                "on",
            ])
            .status()
            .map_err(|e| format!("tmux new-session for {name}: {e}"))?;
        if !status.success() {
            return Err(format!(
                "tmux new-session for {name} failed (exit {:?})",
                status.code()
            ));
        }
        Ok(())
    }

    /// `tmux load-buffer` (message on stdin) then `paste-buffer -d -p`
    /// (bracketed paste, so embedded newlines don't submit early) then
    /// `send-keys Enter`. Errors (e.g. the session died) are logged to
    /// stderr and otherwise ignored — a relay to a dead session isn't a
    /// player-fatal error.
    fn send(&mut self, slug: &str, message: &str) {
        let name = state::tmux_session_name(&self.project, slug);
        let buf = format!("dash-{name}");

        let child = Command::new("tmux")
            .args(["load-buffer", "-b", &buf, "-"])
            .stdin(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                eprintln!("dashboard: tmux load-buffer for {name} failed to start: {e}");
                return;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(message.as_bytes()) {
                eprintln!("dashboard: tmux load-buffer for {name}: writing message failed: {e}");
            }
        }
        match child.wait() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!(
                    "dashboard: tmux load-buffer for {name} failed (exit {:?})",
                    status.code()
                );
                return;
            }
            Err(e) => {
                eprintln!("dashboard: tmux load-buffer for {name} failed: {e}");
                return;
            }
        }

        // `paste-buffer -p` and `send-keys` need a *pane* target, not just a
        // session target: `=<name>` alone (exact session match) resolves for
        // `has-session`/`kill-session`, but tmux won't paste into or send
        // keys to it without a window/pane component — `=<name>:` (session's
        // current window's active pane) is the minimal target that resolves.
        let target = format!("={name}:");
        if let Err(e) = Command::new("tmux")
            .args(["paste-buffer", "-d", "-p", "-b", &buf, "-t", &target])
            .status()
        {
            eprintln!("dashboard: tmux paste-buffer for {name} failed: {e}");
            return;
        }
        if let Err(e) = Command::new("tmux")
            .args(["send-keys", "-t", &target, "Enter"])
            .status()
        {
            eprintln!("dashboard: tmux send-keys for {name} failed: {e}");
        }
    }

    /// `tmux kill-session`, ignoring "no such session" (and any other
    /// failure — a task being killed twice, or a session that already died
    /// on its own, are both fine outcomes here).
    fn kill(&mut self, slug: &str) {
        let name = state::tmux_session_name(&self.project, slug);
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &format!("={name}")])
            .output();
    }

    /// `tmux capture-pane -p -t =<name>:` (like `send`, a pane target: a
    /// bare `=<name>` doesn't resolve to a pane here either). Only the
    /// visible screen, no scrollback, no escape sequences.
    fn capture(&self, slug: &str) -> Option<String> {
        let name = state::tmux_session_name(&self.project, slug);
        let out = Command::new("tmux")
            .args(["capture-pane", "-p", "-t", &format!("={name}:")])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `#{pane_dead}` decides (a live pane is left alone), then the dead
    /// pane's whole history (`capture-pane -S -`: its last lines may have
    /// scrolled off the screen when tmux wrote its own "Pane is dead" line,
    /// which is dropped) and `#{pane_dead_status}`/`#{pane_dead_signal}`,
    /// then `kill-session`.
    fn reap(&mut self, slug: &str) -> Option<Remains> {
        let name = state::tmux_session_name(&self.project, slug);
        let target = format!("={name}:");
        let out = Command::new("tmux")
            .args([
                "display-message",
                "-p",
                "-t",
                &target,
                "#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}",
            ])
            .output()
            .ok()?;
        let info = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let mut fields = info.split('|');
        if !out.status.success() || fields.next() != Some("1") {
            return None;
        }
        let status = fields.next().and_then(|f| f.parse().ok());
        let signal = fields.next().and_then(|f| f.parse().ok());
        let screen = Command::new("tmux")
            .args(["capture-pane", "-p", "-S", "-", "-t", &target])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                let text = String::from_utf8_lossy(&o.stdout);
                let mut lines: Vec<&str> = text.lines().map(str::trim_end).collect();
                while lines.last().is_some_and(|l| l.is_empty()) {
                    lines.pop();
                }
                if lines.last().is_some_and(|l| l.starts_with("Pane is dead")) {
                    lines.pop();
                }
                lines.join("\n")
            });
        self.kill(slug);
        Some(Remains {
            screen,
            status,
            signal,
        })
    }
}

/// Session screen tails in events: at most this many non-blank lines…
const SCREEN_LINES: usize = 25;
/// …and this many bytes of them, so events.jsonl doesn't bloat.
const SCREEN_BYTES: usize = 2048;

/// A screen's last `SCREEN_LINES` non-blank lines, trimmed of trailing
/// whitespace and indented under a "last screen:" header, cut from the
/// front to `SCREEN_BYTES`; or a plain "no screen captured" when there's
/// nothing to show, so the director knows it wasn't left out.
pub fn screen_tail(screen: Option<&str>) -> String {
    let lines: Vec<&str> = screen
        .unwrap_or("")
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return "no screen captured".to_string();
    }
    let mut kept: Vec<String> = Vec::new();
    let mut size = 0;
    let mut cut = lines.len() > SCREEN_LINES;
    for line in lines.iter().rev().take(SCREEN_LINES) {
        let line = format!("  {line}");
        if size + line.len() + 1 > SCREEN_BYTES {
            cut = true;
            if kept.is_empty() {
                // One huge line: keep its end.
                let mut start = line.len() - (SCREEN_BYTES - 1);
                while !line.is_char_boundary(start) {
                    start += 1;
                }
                kept.push(format!("  {}", &line[start..]));
            }
            break;
        }
        size += line.len() + 1;
        kept.push(line);
    }
    if cut {
        kept.push("  …".to_string());
    }
    kept.reverse();
    format!("last screen:\n{}", kept.join("\n"))
}

/// Wraps `s` in single quotes for safe interpolation into a `sh -c` string,
/// escaping any single quote in `s` itself (`'` -> `'\''`).
/// The first check of the task's own suite files (`own`) at the trim that
/// the tip lacks verbatim, or `None` when the trim only removed checks.
fn trim_additions(before: &config::Suite, after: &config::Suite, own: &[String]) -> Option<String> {
    after
        .checks
        .iter()
        .filter(|c| c.source.as_ref().is_some_and(|s| own.contains(s)))
        .find(|c| before.check(&c.name) != Some(*c))
        .map(|c| c.name.clone())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// --------------------------------------------------------------------------
// Idle detection: a Claude Code `Stop` hook in every session's worktree
// --------------------------------------------------------------------------

/// Written by the session's hooks into its worktree's git dir (beside
/// `dashboard-prompt.md`) when the agent ends a turn: `stop <unix> <pid>`,
/// or `error <unix> <pid>` when the turn ended on an API error (a usage
/// limit, an overload). Removed when a prompt is submitted (the agent's
/// `UserPromptSubmit` hook) and by the player before every launch or relay,
/// so it only ever exists while the agent sits waiting for input. The whole
/// line identifies one stop.
pub const STOPPED_FILE: &str = "dashboard-stopped";
/// Written by the player beside it when it collects a task agent's report:
/// the agent answered, so its stop is the expected wait for the director.
/// Removed with `STOPPED_FILE`.
const REPORTED_FILE: &str = "dashboard-reported";
/// Where the hooks go: the worktree's own, never-committed Claude Code
/// settings (`hide_from_git`). Another `agent_cmd` just ignores the file.
const SETTINGS_FILE: &str = ".claude/settings.local.json";

/// One stop, as `read_stop` found it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stop {
    /// `STOPPED_FILE`'s line, unique per stop.
    id: String,
    /// The turn ended on an API error rather than by the agent's choice.
    error: bool,
}

fn read_stop(git_dir: &Path) -> Option<Stop> {
    let text = std::fs::read_to_string(git_dir.join(STOPPED_FILE)).ok()?;
    let id = text.trim().to_string();
    (!id.is_empty()).then(|| Stop {
        error: id.starts_with("error"),
        id,
    })
}

/// Re-arms idle detection for a session about to be (re)launched or to get
/// a message: whatever stop came before is answered by it.
fn clear_stop(git_dir: &Path) {
    let _ = std::fs::remove_file(git_dir.join(STOPPED_FILE));
    let _ = std::fs::remove_file(git_dir.join(REPORTED_FILE));
}

/// The stop marker path of the agent working in `clone_dir`, for tests and
/// tools that want to play the hook's part.
pub fn stop_marker(clone_dir: &Path) -> Result<PathBuf, String> {
    Ok(Player::<NullSessions>::git_dir(clone_dir)?.join(STOPPED_FILE))
}

/// Merges the dashboard's `Stop`/`StopFailure`/`UserPromptSubmit` hooks
/// into `clone_dir`'s `.claude/settings.local.json`, keeping whatever else
/// is there (an earlier launch's copies of these hooks are replaced, not
/// duplicated). A file that isn't a JSON object is left alone: an error.
fn install_stop_hook(clone_dir: &Path, git_dir: &Path) -> Result<(), String> {
    use serde_json::{Value, json};
    let marker = shell_quote(path_str(&git_dir.join(STOPPED_FILE))?);
    let file = clone_dir.join(SETTINGS_FILE);
    let mut settings: Value = match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let not_object = || {
        format!(
            "{}: not the JSON object Claude Code expects",
            file.display()
        )
    };
    let hooks = settings
        .as_object_mut()
        .ok_or_else(not_object)?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(not_object)?;
    for (event, command) in [
        ("Stop", format!("echo \"stop $(date +%s) $$\" > {marker}")),
        (
            "StopFailure",
            format!("echo \"error $(date +%s) $$\" > {marker}"),
        ),
        ("UserPromptSubmit", format!("rm -f {marker}")),
    ] {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(not_object)?;
        groups.retain(|g| !g.to_string().contains(STOPPED_FILE));
        groups.push(json!({"hooks": [{"type": "command", "command": command}]}));
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&file, text + "\n").map_err(|e| format!("{}: {e}", file.display()))?;
    hide_from_git(clone_dir)
}

/// Keeps the settings file out of anything the agent could commit: an
/// untracked one goes in the repository's `info/exclude` (shared by every
/// worktree of the workspace), a tracked one is marked `--skip-worktree` in
/// this worktree's index, or `--assume-unchanged` in a sparse one (spec
/// §21), where git clears the skip bit of every file present. Either way
/// `git status` and `git add -A` never see it, and nothing the player
/// validates (commits only) does either.
fn hide_from_git(clone_dir: &Path) -> Result<(), String> {
    let git = Git::new(clone_dir);
    if git.run_ok(&["ls-files", "--error-unmatch", "--", SETTINGS_FILE]) {
        let flag = if git.is_sparse() { "--assume-unchanged" } else { "--skip-worktree" };
        return git
            .run(&["update-index", flag, "--", SETTINGS_FILE])
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    let out = git
        .run(&["rev-parse", "--git-path", "info/exclude"])
        .map_err(|e| e.to_string())?;
    let exclude = clone_dir.join(out);
    let line = format!("/{SETTINGS_FILE}");
    let text = std::fs::read_to_string(&exclude).unwrap_or_default();
    if text.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    std::fs::write(&exclude, format!("{text}{sep}{line}\n"))
        .map_err(|e| format!("{}: {e}", exclude.display()))
}

// --------------------------------------------------------------------------
// TickReport
// --------------------------------------------------------------------------

/// What one `Player::tick` call did. `imported` is `(ref, sha, transition)`
/// for every commit the player advanced a hub ref to (director docs commits
/// on main, plans, replans, and the player's own unblock/accept merges land
/// here too via their `Transition::MergeUnblock`/`MergeAccept` notes, as do
/// worker finish/block commits via `Transition::Finish`/`Block`). `merges`
/// carries a short human line per unblock-merge attempt, success or failure
/// (spec §11) — there's no reason to model it more richly than the CLI's
/// report needs. `escalated` is `(slug, cause)` for a send-back that would
/// exceed `max_review_rounds` (spec §10): the offending commit is
/// deliberately not imported and not recorded as a rejection, so it needs
/// its own place in the report. `launched` is every slug a worker session
/// was started for this tick (spec §9's launch row); `errors` is everything
/// that went wrong outside the reject/relay machinery — a launch attempt
/// whose task check now unexpectedly passes, a leftover clone directory, or
/// a `sessions.launch` failure — none of which is a rejection (there's no
/// commit to reject) but all of which the operator needs to see.
#[derive(Debug, Default)]
pub struct TickReport {
    pub imported: Vec<(String, String, Transition)>,
    pub rejected: Vec<RejectionRecord>,
    pub merges: Vec<String>,
    pub escalated: Vec<(String, String)>,
    /// Accepts stopped at a merge conflict with `main`: (slug, message).
    pub conflicts: Vec<(String, String)>,
    /// Accepts whose merge with `main` failed the suite: (slug, message).
    pub merge_failed: Vec<(String, String)>,
    pub launched: Vec<String>,
    /// Task agents started this tick (spec §17).
    pub agents_launched: Vec<String>,
    /// Reports task agents handed the player this tick: (slug, report).
    pub agent_reports: Vec<(String, records::AgentReport)>,
    /// `main`'s impure checks, when newly found (spec §20): one line each.
    pub main_impure: Vec<String>,
    /// `main`'s suite is over its `[budget]` (a check killed, or the total
    /// past `suite_fail`), when newly found: one line.
    pub main_over_budget: Vec<String>,
    /// `main`'s suite fails, when newly found on a tip: its summary.
    pub main_failing: Vec<String>,
    /// Finishes and accepts held because they fail only on checks already
    /// impure or over budget on `main`: (slug, message).
    pub held: Vec<(String, String)>,
    /// What a test-speedup finish removed or rewired (tests gone, `cmd`s
    /// changed): (slug, message), for the director's review.
    pub speedups: Vec<(String, String)>,
    /// Suites over `[budget] suite_warn`: (slug, message); `_main` for main's.
    pub slow: Vec<(String, String)>,
    pub errors: Vec<String>,
}

impl TickReport {
    pub fn is_empty(&self) -> bool {
        self.imported.is_empty()
            && self.rejected.is_empty()
            && self.merges.is_empty()
            && self.escalated.is_empty()
            && self.conflicts.is_empty()
            && self.merge_failed.is_empty()
            && self.launched.is_empty()
            && self.agents_launched.is_empty()
            && self.agent_reports.is_empty()
            && self.main_impure.is_empty()
            && self.main_over_budget.is_empty()
            && self.main_failing.is_empty()
            && self.held.is_empty()
            && self.speedups.is_empty()
            && self.slow.is_empty()
            && self.errors.is_empty()
    }
}

impl fmt::Display for TickReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return write!(f, "no changes");
        }
        let mut lines: Vec<String> = Vec::new();
        for (r, sha, t) in &self.imported {
            lines.push(format!("imported {} -> {} ({})", short(sha), r, t.as_str()));
        }
        for rej in &self.rejected {
            lines.push(format!(
                "rejected {} [{}]: {}",
                rej.scope,
                rej.reason.as_str(),
                rej.message
            ));
        }
        for (slug, cause) in &self.escalated {
            lines.push(format!("escalated {slug}: {cause}"));
        }
        for (slug, msg) in &self.conflicts {
            lines.push(format!("conflict {slug}: {msg}"));
        }
        for (slug, msg) in &self.merge_failed {
            lines.push(format!("merge failed {slug}: {}", first_line(msg)));
        }
        for slug in &self.launched {
            lines.push(format!("launched {slug}"));
        }
        for slug in &self.agents_launched {
            lines.push(format!("launched task agent {slug}"));
        }
        for (slug, r) in &self.agent_reports {
            lines.push(format!("task agent {slug} reported: {}", r.verdict));
        }
        for line in &self.main_impure {
            lines.push(format!("main impure: {line}"));
        }
        for line in &self.main_over_budget {
            lines.push(format!("main over budget: {line}"));
        }
        for line in &self.main_failing {
            lines.push(format!("main failing: {}", first_line(line)));
        }
        for (slug, msg) in &self.held {
            lines.push(format!("held {slug}: {msg}"));
        }
        for (slug, msg) in &self.speedups {
            lines.push(format!("speedup {slug}: {}", first_line(msg)));
        }
        for (slug, msg) in &self.slow {
            lines.push(format!("slow suite {slug}: {msg}"));
        }
        lines.extend(self.merges.iter().cloned());
        for err in &self.errors {
            lines.push(format!("error: {err}"));
        }
        write!(f, "{}", lines.join("\n"))
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

// --------------------------------------------------------------------------
// Player
// --------------------------------------------------------------------------

pub struct Player<S: Sessions> {
    home: Home,
    hub: Git,
    /// The `architecture/` tree on main whose export was last attempted:
    /// tried once per tree (`ensure_arch`), not every tick.
    arch_tried: Option<String>,
    director: PathBuf,
    project: String,
    runner: checks::Runner,
    sessions: S,
    /// Every slug's status as of the end of the last tick that ran
    /// `emit_events`, persisted to `home.player_state()` so a restart
    /// doesn't re-emit events for statuses the director has already been
    /// told about (step 7).
    last: BTreeMap<String, Status>,
    /// Per session key, the launch id (`launch_id`) whose death was already
    /// reported (or, on a first run, found dead), persisted alongside
    /// `last`. Death is level-triggered: a watched session that's dead
    /// under any other launch id is reported, so one that died before any
    /// tick saw it alive still is, and a relaunch re-arms it.
    deaths_told: BTreeMap<String, String>,
    /// Per session key, the stop (`Stop::id`) already reported idle (or, on
    /// a first run, found), persisted alongside `last`. Dropped once the
    /// marker is gone, so every later stop is new.
    idle_told: BTreeMap<String, String>,
    /// Per session key, its screen as of the last tick it owed work
    /// (`watch_pane`), persisted alongside `last` so a restart neither
    /// resets the idle clock nor repeats the report.
    panes: BTreeMap<String, PaneSeen>,
    /// The time `now` returns instead of the system clock, for tests.
    clock: Option<u64>,
    /// True until the first `emit_events` call completes: with no state file
    /// on disk at `open` time, that first call only initializes `last` (spec:
    /// "The very first run with no state file initializes silently, without
    /// emitting").
    first_run: bool,
    /// The last stale-base commit of each task agent the player told to
    /// rebase (`import_task_agent`), so it says so once, not every tick.
    agent_rebase_told: BTreeMap<String, String>,
    /// The agent commit last told it is held (`fsm::Verdict::Hold`), per slug.
    agent_hold_told: BTreeMap<String, String>,
    /// The fingerprint of the last sandbox policy that passed its
    /// preflight, so a tick re-runs it only when `[sandbox]` changes.
    sandbox_preflighted: Option<String>,
    /// `main`'s impure checks under the current sandbox, by name, as of
    /// this tick's `verify_main`: work failing only on these is held.
    main_impure: BTreeMap<String, String>,
    /// Main's tip and why its suite is over `[budget] suite_warn`, if it is.
    main_slow: Option<(String, String)>,
    /// How `main`'s suite is over `[budget]` (checks killed, total past
    /// `suite_fail`) as of this tick's `verify_main`: work failing only on
    /// these, or only through the total, is held.
    main_over: Option<checks::Overrun>,
    /// The checks failing on `main` as of this tick's `verify_main`: work
    /// failing only on these is held, like on an impure check.
    main_failed: BTreeSet<String>,
    /// `main`'s failing suite (tip, summary), and the tip last reported.
    main_failing: Option<(String, String)>,
    failing_told: Option<String>,
    /// The main tip whose overrun was last reported.
    over_told: Option<String>,
    /// The main tip whose slowness was last reported.
    slow_told: Option<String>,
    /// The main tip `verify_main` last ran the suite on.
    main_verified: String,
    /// What `main`'s suite verdict was last reported for (tip, policy and
    /// impure checks), so it's reported once, not every tick.
    main_told: Option<String>,
    /// Per slug, the (accept candidate, main tip) last held in `land`: the
    /// director hears about it once, and it isn't re-merged and re-run every
    /// tick while `main` stays the same. (Held finishes live in the hub:
    /// `records::held_commit`.)
    held_accepts: BTreeMap<String, (String, String)>,
    /// The (tip, reason) each held launch was last reported for
    /// (`hold_launch`).
    launch_told: BTreeMap<String, String>,
}

impl<S: Sessions> Player<S> {
    /// Opens the player against an already-`dashboard init`-ed home: reads
    /// `dashboard.director` and `dashboard.project` from the hub's config.
    pub fn open(home: Home, sessions: S) -> Result<Self, String> {
        let hub = Git::new(home.hub());
        let director = hub.config_get("dashboard.director").ok_or_else(|| {
            "hub is missing dashboard.director — run `dashboard init`".to_string()
        })?;
        let project = hub
            .config_get("dashboard.project")
            .ok_or_else(|| "hub is missing dashboard.project — run `dashboard init`".to_string())?;
        let runner = checks::Runner::new(&home);
        let (saved, first_run) = load_player_state(&home);
        Ok(Self {
            home,
            hub,
            director: PathBuf::from(director),
            project,
            runner,
            sessions,
            last: saved.status,
            deaths_told: saved.deaths_told,
            idle_told: saved.idle_told,
            panes: saved.panes,
            clock: None,
            first_run,
            agent_rebase_told: BTreeMap::new(),
            agent_hold_told: BTreeMap::new(),
            sandbox_preflighted: None,
            arch_tried: None,
            main_impure: BTreeMap::new(),
            main_slow: None,
            main_over: None,
            main_failed: BTreeSet::new(),
            main_failing: None,
            failing_told: None,
            over_told: None,
            slow_told: None,
            main_verified: String::new(),
            main_told: None,
            held_accepts: BTreeMap::new(),
            launch_told: BTreeMap::new(),
        })
    }

    /// Runs the suite on `main`'s tip under the current sandbox (a cache
    /// hit unless main, the policy or the player's leak check changed) and
    /// remembers which checks are impure there (spec §20). Returns them,
    /// check name to why.
    pub fn verify_main(&mut self) -> Result<&BTreeMap<String, String>, String> {
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or("hub has no main")?;
        let suite = self.runner.run_suite(&main_tip);
        self.note_ledgers("_main", &main_tip, &suite);
        self.main_impure = suite
            .checks
            .iter()
            .filter_map(|c| c.impurity().map(|why| (c.name.clone(), why)))
            .collect();
        self.main_slow = suite
            .slow(&self.runner.budget())
            .map(|msg| (main_tip.clone(), msg));
        self.main_over = suite.budget_overrun(&self.runner.budget());
        self.main_failed = suite.checks.iter().filter(|c| !c.pass).map(|c| c.name.clone()).collect();
        self.main_failing = (!suite.pass).then(|| (main_tip.clone(), suite.summary()));
        self.main_verified = main_tip;
        Ok(&self.main_impure)
    }

    /// Puts `main`'s impure checks in the report once per (tip, policy,
    /// impure set), for the director's `main_impure` event.
    fn report_main(&mut self, report: &mut TickReport) {
        if let Some((tip, msg)) = &self.main_slow {
            if self.slow_told.as_ref() != Some(tip) {
                self.slow_told = Some(tip.clone());
                report.slow.push(("_main".to_string(), msg.clone()));
            }
        }
        match &self.main_failing {
            Some((tip, summary)) if self.failing_told.as_ref() != Some(tip) => {
                self.failing_told = Some(tip.clone());
                report.main_failing.push(summary.clone());
            }
            Some(_) => {}
            None => self.failing_told = None,
        }
        match &self.main_over {
            Some(over) if self.over_told.as_ref() != Some(&self.main_verified) => {
                self.over_told = Some(self.main_verified.clone());
                report.main_over_budget.push(over.why.clone());
            }
            Some(_) => {}
            None => self.over_told = None,
        }
        if self.main_impure.is_empty() {
            self.main_told = None;
            return;
        }
        let names: Vec<&str> = self.main_impure.keys().map(String::as_str).collect();
        let key = format!(
            "{} {} {}",
            self.main_verified,
            self.runner.sandbox().fingerprint(),
            names.join(",")
        );
        if self.main_told.as_ref() == Some(&key) {
            return;
        }
        self.main_told = Some(key);
        report
            .main_impure
            .extend(self.main_impure.values().cloned());
    }

    /// Whether failing check `c` is the same failure `main` already has:
    /// impure there too, killed for its timeout there too, or failing there
    /// (matched by name).
    fn bad_on_main(&self, c: &checks::CheckResult) -> bool {
        (!c.pass && self.main_failed.contains(&c.name))
            || (c.impurity().is_some() && self.main_impure.contains_key(&c.name))
            || (c.timed_out
                && self
                    .main_over
                    .as_ref()
                    .is_some_and(|o| o.timed_out.contains(&c.name)))
    }

    /// A passing suite over `suite_fail` only because `main` is: every check
    /// `main` has is no slower than there (`checks::within_tolerance`), and
    /// the checks `main` lacks fit `suite_fail` on their own, so slow tests
    /// can't hide in main's excess. Totals are never compared. Returns the
    /// text for the `held` event.
    fn held_over_total(&self, suite: &checks::SuiteResult) -> Option<Vec<String>> {
        let over = self.main_over.as_ref().filter(|o| o.total_over)?;
        let budget = self.runner.budget();
        if !suite.pass || suite.over_budget(&budget).is_none() {
            return None;
        }
        let mut added = 0;
        for c in &suite.checks {
            match over.millis.get(&c.name) {
                Some(m) if checks::within_tolerance(c.millis, *m) => {}
                Some(_) => return None,
                None => added += c.millis,
            }
        }
        (added <= budget.suite_fail * 1000).then(|| vec!["suite time".to_string()])
    }

    /// Whether `suite` is held, not judged: it fails only where `main`
    /// does (`held_by_main`), or is slow only because `main` is
    /// (`held_over_total`).
    fn held_reason(&self, suite: &checks::SuiteResult) -> Option<Vec<String>> {
        self.held_by_main(suite).or_else(|| self.held_over_total(suite))
    }

    /// The failing checks' names if the suite fails only on checks that are
    /// already bad on `main` (impure, or killed for their timeout): not the
    /// work's fault.
    fn held_by_main(&self, suite: &checks::SuiteResult) -> Option<Vec<String>> {
        let failing: Vec<&checks::CheckResult> = suite.checks.iter().filter(|c| !c.pass).collect();
        if suite.error.is_some() || failing.is_empty() {
            return None;
        }
        failing
            .iter()
            .all(|c| self.bad_on_main(c))
            .then(|| failing.iter().map(|c| c.name.clone()).collect())
    }

    /// Releases every held finish whose checks are no longer impure on
    /// `main` (spec §20). The branch still carries main's old copy of them,
    /// so the player merges `main` in (a `sync_revision`, re-pinned) and
    /// tells the worker to rebase its finish onto it. A branch that already
    /// has `main` is just un-held: its finish is judged normally.
    fn release_held(&mut self, report: &mut TickReport) -> Result<(), String> {
        for slug in records::held_slugs(&self.hub) {
            let Some(candidate) = records::held_commit(&self.hub, &slug) else {
                continue;
            };
            let tip = self.hub.rev_parse(&format!("refs/heads/task/{slug}"));
            if self.held_reason(&self.run_task_set(&slug, &candidate, tip.as_deref())).is_some() {
                continue;
            }
            let hub_ref = format!("refs/heads/task/{slug}");
            let Some(tip) = self.hub.rev_parse(&hub_ref) else {
                records::release_held(&self.hub, &slug, false)?;
                continue;
            };
            let main_tip = self.main_verified.clone();
            if self.hub.is_ancestor(&main_tip, &tip) {
                records::release_held(&self.hub, &slug, false)?;
                continue;
            }
            let (tree, conflicts) = self
                .hub
                .merge_tree_conflicts(&tip, &main_tip)
                .map_err(|e| e.to_string())?;
            if !conflicts.is_empty() {
                // Judged as it stands from the next tick on; the worker can
                // contest what fails.
                records::release_held(&self.hub, &slug, false)?;
                report.errors.push(format!(
                    "task/{slug}: main is clean again, but merging it in to release the held finish conflicts in {}",
                    conflicts.join(", ")
                ));
                continue;
            }
            let sync = self
                .hub
                .commit_tree(
                    &tree,
                    &[&tip, &main_tip],
                    &format!("Merge main into task/{slug}"),
                )
                .map_err(|e| e.to_string())?;
            self.hub
                .update_ref_cas(&hub_ref, &sync, &tip)
                .map_err(|e| e.to_string())?;
            records::record_import(
                &self.hub,
                &sync,
                Actor::Player,
                Transition::SyncRevision,
                Some(&slug),
            )?;
            self.pin(&slug, &sync, report)?;
            records::release_held(&self.hub, &slug, true)?;
            report
                .imported
                .push((hub_ref, sync.clone(), Transition::SyncRevision));
            report.held.push((
                slug.clone(),
                format!("released: main is clean and merged into task/{slug}; the worker rebases its finish"),
            ));
            let text = self
                .prompts()?
                .render("worker.released", &[("slug", &slug)]);
            self.relay(&slug, &text);
        }
        Ok(())
    }

    /// Reads `[sandbox]` from the hub's `main` and runs every check from
    /// now on under it, after proving the jail holds on this host
    /// (`Sandbox::preflight`) whenever the policy is new. On an error no
    /// check may run: the caller must not tick. Returns the policy's
    /// one-line description.
    pub fn check_sandbox(&mut self) -> Result<String, String> {
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or("hub has no main")?;
        let roadmap = self.roadmap_at(&main_tip)?;
        self.runner.set_budget(roadmap.budget);
        let sandbox = Sandbox::from_config(&roadmap.sandbox)?;
        let fingerprint = sandbox.fingerprint();
        if self.sandbox_preflighted.as_ref() != Some(&fingerprint) {
            self.sandbox_preflighted = None;
            sandbox.preflight(&self.home.root)?;
            self.sandbox_preflighted = Some(fingerprint);
        }
        let description = sandbox.describe();
        self.runner.set_sandbox(sandbox);
        Ok(description)
    }

    /// Read access to the sessions backend, so tests using `NullSessions`
    /// can assert on `sent`/`killed` without the player exposing any way to
    /// mutate it from outside `tick`.
    pub fn sessions(&self) -> &S {
        &self.sessions
    }

    /// Mutable access to the sessions backend. Only meant for tests that
    /// need to simulate a session dying on its own — e.g. removing a slug
    /// from `NullSessions::alive` directly, as opposed to going through
    /// `kill` (see `NullSessions`'s own doc comment).
    pub fn sessions_mut(&mut self) -> &mut S {
        &mut self.sessions
    }

    /// Pins the player's clock (unix seconds) for idle detection, so tests
    /// can move time without sleeping.
    pub fn set_clock(&mut self, unix: u64) {
        self.clock = Some(unix);
    }

    fn now(&self) -> u64 {
        self.clock.unwrap_or_else(unix_now)
    }

    /// One pass: fetch, import director `main`, import director `task/*`
    /// branches, then run the player's own unblock merges (spec §5, §9, §11,
    /// in that order — see CLAUDE.md/spec.md for why the order matters:
    /// `main` can gain new phases/docs before tasks are validated against
    /// it, and a branch just planned this tick can be unblocked in the same
    /// tick if its deps already cleared).
    pub fn tick(&mut self) -> Result<TickReport, String> {
        let mut report = TickReport::default();
        self.fetch()?;
        self.check_sandbox()?;
        // Before importing main (a director commit on an impure main is
        // judged against it) and again after (a cache hit unless main just
        // moved), so a fix that lands now releases held work this tick.
        self.verify_main()?;
        self.import_main(&mut report)?;
        self.ensure_arch(&mut report);
        self.verify_main()?;
        self.report_main(&mut report);
        self.release_held(&mut report)?;
        self.import_task_branches(&mut report)?;
        self.import_task_agents(&mut report)?;
        self.launch_workers(&mut report)?;
        self.import_workers(&mut report)?;
        self.unblock_merges(&mut report)?;
        self.run_task_agents(&mut report)?;
        self.serve_queue()?;
        self.emit_events(&report)?;
        Ok(report)
    }

    /// Whether `[player] runs` is `queued` at main's tip.
    fn queued(&self) -> bool {
        self.hub
            .rev_parse("refs/heads/main")
            .and_then(|tip| self.roadmap_at(&tip).ok())
            .is_some_and(|r| r.player.runs == config::Runs::Queued)
    }

    /// `[player] runs = "queued"`: runs the oldest waiting `dashboard test`
    /// request (one per tick, so runs never overlap) and writes its result
    /// for the agent to read. Never a strike, an event or a hub record.
    fn serve_queue(&mut self) -> Result<(), String> {
        if !self.queued() {
            return Ok(());
        }
        let Some(req) = queue::next(&self.home) else {
            return Ok(());
        };
        let local = format!("refs/dashboard/queue/{}", req.id);
        let fetched = path_str(&req.repo).and_then(|repo| {
            self.hub
                .run(&["fetch", "--quiet", repo, &format!("+{}:{local}", req.ref_name())])
                .map_err(|e| e.to_string())
        });
        let (pass, report) = match fetched
            .ok()
            .and_then(|_| self.hub.rev_parse(&local))
            .filter(|sha| *sha == req.commit)
        {
            Some(sha) => {
                // The task's run set, against the hub tip it builds on.
                let tip = self.hub.rev_parse(&format!("refs/heads/task/{}", req.slug));
                match self.run_set_for(&req.slug, &sha, tip.as_deref()).and_then(|s| s.restrict(&req.checks)) {
                    Ok(set) => {
                        let suite = self.runner.run_set(&sha, &set);
                        (suite.pass, queue::render(Some(&set.why), &suite))
                    }
                    Err(e) => (false, queue::render(None, &checks::SuiteResult::failing(e))),
                }
            }
            None => (
                false,
                "the snapshot could not be fetched from your worktree; run `dashboard test` again\n"
                    .to_string(),
            ),
        };
        let _ = self.hub.delete_ref(&local);
        queue::finish(&self.home, &req, pass, &report)
    }

    // ---- events (spec's "the director must not poll"; step 7) ----

    /// Writes the heartbeat, emits `DirectorRejected`/`PlayerError` straight
    /// from this tick's report, then diffs every task's status against
    /// `self.last` to emit `Tested`/`Escalated`/`Ready`/`Accepted`/
    /// `Retired`, and reports each dead session once per launch
    /// (`SessionDied`/`AgentDied`, `report_death`) and each live one idle
    /// while it owes work once per stop or stale screen (`SessionIdle`/
    /// `AgentIdle`, `report_idle`). The persisted state (and
    /// the "first run" silence) is what makes this idempotent across player
    /// restarts.
    fn emit_events(&mut self, report: &TickReport) -> Result<(), String> {
        events::write_heartbeat(&self.home)?;

        for rej in &report.rejected {
            if rej.actor == Actor::Director {
                let slug = if rej.scope == "_main" {
                    None
                } else {
                    Some(rej.scope.as_str())
                };
                events::append(&self.home, EventKind::DirectorRejected, slug, &rej.message)?;
            }
        }
        for err in &report.errors {
            events::append(&self.home, EventKind::PlayerError, None, err)?;
        }
        // From the report, not a status change: a resolution that runs into
        // a newer conflict stays `conflicted` but still needs the director.
        for (slug, msg) in &report.conflicts {
            events::append(&self.home, EventKind::Conflict, Some(slug), msg)?;
        }
        for (slug, msg) in &report.merge_failed {
            events::append(&self.home, EventKind::MergeFailed, Some(slug), msg)?;
        }
        if !report.main_impure.is_empty() {
            events::append(
                &self.home,
                EventKind::MainImpure,
                None,
                &report.main_impure.join("\n"),
            )?;
        }
        for line in &report.main_over_budget {
            events::append(&self.home, EventKind::MainOverBudget, None, line)?;
        }
        for line in &report.main_failing {
            events::append(&self.home, EventKind::MainFailing, None, line)?;
        }
        for (slug, msg) in &report.held {
            events::append(&self.home, EventKind::Held, Some(slug), msg)?;
        }
        for (slug, msg) in &report.speedups {
            events::append(&self.home, EventKind::SpeedupFinish, Some(slug), msg)?;
        }
        for (slug, msg) in &report.slow {
            let slug = (slug != "_main").then_some(slug.as_str());
            events::append(&self.home, EventKind::SuiteSlow, slug, msg)?;
        }
        for slug in &report.agents_launched {
            let session = state::tmux_session_name(&self.project, &state::agent_key(slug));
            events::append(
                &self.home,
                EventKind::AgentLaunched,
                Some(slug),
                &format!("task agent launched; watch it: tmux attach -t {session}"),
            )?;
        }
        for (slug, r) in &report.agent_reports {
            events::append(&self.home, EventKind::AgentReport, Some(slug), &r.text)?;
        }

        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let mut next: BTreeMap<String, Status> = BTreeMap::new();
        for task in &hub_state.tasks {
            let prev = self.last.get(&task.slug).copied();
            next.insert(task.slug.clone(), task.status);

            // A task agent's liveness is tracked under its own key.
            let finished = matches!(
                task.status,
                Status::Accepted | Status::Retired | Status::Explored
            );
            if let Some(agent) = &task.agent {
                self.report_death(
                    &state::agent_key(&task.slug),
                    agent.alive,
                    !finished,
                    EventKind::AgentDied,
                    &task.slug,
                    &format!("task/{}'s task agent session died", task.slug),
                )?;
            }

            // Pins are taken at launch: without them there's no worker yet.
            let still_in_flight = matches!(
                task.status,
                Status::Posted | Status::InProgress | Status::InRevision | Status::Contested
            );
            self.report_death(
                &task.slug,
                self.sessions.alive(&task.slug),
                still_in_flight && records::has_pins(&self.hub, &task.slug),
                EventKind::SessionDied,
                &task.slug,
                &format!("task/{}'s worker session died", task.slug),
            )?;
            let idle_after = hub_state.roadmap.player.idle_after;
            self.report_idle(task, false, idle_after)?;
            if task.agent.is_some() {
                self.report_idle(task, true, idle_after)?;
            }

            if self.first_run {
                continue;
            }

            let managed = state::task_agent_managed(&hub_state.roadmap, &task.config);
            let left_escalation = prev == Some(Status::Escalated)
                && !matches!(
                    task.status,
                    Status::Escalated | Status::Accepted | Status::Retired | Status::Explored
                );
            if left_escalation {
                // Only `dashboard relaunch --reset` takes a task back out.
                let was = records::cleared_escalations(&self.hub, &task.slug)
                    .pop()
                    .map(|c| format!(" (was: {})", c.cause))
                    .unwrap_or_default();
                events::append(
                    &self.home,
                    EventKind::Resumed,
                    Some(&task.slug),
                    &format!(
                        "escalation cleared by the operator{was}; task/{} is {} again",
                        task.slug,
                        task.status.as_str()
                    ),
                )?;
            } else if prev != Some(task.status) {
                if let Some((kind, message)) = status_event(task, managed) {
                    events::append(&self.home, kind, Some(&task.slug), &message)?;
                }
            }
        }

        self.last = next;
        self.first_run = false;
        save_player_state(
            &self.home,
            &PlayerState {
                status: self.last.clone(),
                deaths_told: self.deaths_told.clone(),
                idle_told: self.idle_told.clone(),
                panes: self.panes.clone(),
            },
        )?;
        Ok(())
    }

    /// Reports session `key` dead once per launch: when it's `watched` (it
    /// was launched and should still be running), dead, and its current
    /// launch id isn't the one already told. Seen alive, it's re-armed. A
    /// first run records what it finds without emitting. A kill by
    /// accept/retire is never reported: the task is no longer watched.
    /// Whatever dead session lingers is reaped (`Sessions::reap`), watched
    /// or not; the report carries its exit status and last screen.
    fn report_death(
        &mut self,
        key: &str,
        alive: bool,
        watched: bool,
        kind: EventKind,
        slug: &str,
        message: &str,
    ) -> Result<(), String> {
        if alive {
            self.deaths_told.remove(key);
            return Ok(());
        }
        let remains = self.sessions.reap(key);
        if !watched {
            return Ok(());
        }
        let launch = launch_id(&self.home, key);
        if self.deaths_told.get(key) == Some(&launch) {
            return Ok(());
        }
        if !self.first_run {
            let remains = remains.unwrap_or_default();
            let exit = match (remains.status, remains.signal) {
                (_, Some(signal)) => format!(" (killed by signal {signal})"),
                (Some(status), None) => format!(" (exit status {status})"),
                (None, None) => String::new(),
            };
            let tail = screen_tail(remains.screen.as_deref());
            events::append(
                &self.home,
                kind,
                Some(slug),
                &format!("{message}{exit}\n{tail}"),
            )?;
        }
        self.deaths_told.insert(key.to_string(), launch);
        Ok(())
    }

    /// Every session launch goes through here: on success it records a
    /// fresh launch id for `key`, which re-arms death reporting for it —
    /// on disk, since `dashboard relaunch` runs in another process.
    fn launch_session(
        &mut self,
        key: &str,
        dir: &Path,
        cmd: &str,
        prompt_file: &Path,
    ) -> Result<(), String> {
        // The prompt file lives in the worktree's git dir, as does the stop
        // marker. A hook that won't install only costs idle detection.
        if let Some(git_dir) = prompt_file.parent() {
            clear_stop(git_dir);
            if let Err(e) = install_stop_hook(dir, git_dir) {
                eprintln!("dashboard: {key}: no idle detection: {e}");
            }
        }
        self.panes.remove(key);
        self.sessions.launch(key, dir, cmd, prompt_file)?;
        record_launch(&self.home, key)
    }

    /// Every message into a session goes through here: the agent resumes
    /// on it, so whatever stop it sat at is answered (`clear_stop`) before
    /// the paste. On disk, since `dashboard ask` runs in its own process.
    fn relay(&mut self, key: &str, text: &str) {
        if let Some(git_dir) = self.session_git_dir(key) {
            clear_stop(&git_dir);
        }
        self.panes.remove(key);
        self.sessions.send(key, text);
    }

    /// The git dir of session `key`'s worktree, if it has one.
    fn session_git_dir(&self, key: &str) -> Option<PathBuf> {
        let dir = match key.strip_suffix("_agent") {
            Some(slug) => paths::agent_clone(&self.director, slug),
            None => paths::worker_clone(&self.director, key),
        };
        if !dir.exists() {
            return None;
        }
        Self::git_dir(&dir).ok()
    }

    /// Whether `task`'s worker (or, with `agent`, its task agent) owes the
    /// hub something right now, i.e. nobody but that agent is holding the
    /// task up, so its sitting idle is worth reporting. The worker: pinned
    /// (launched), `posted`/`in_progress`/`in_revision`, and no finish held
    /// on `main`'s impure checks — a finish, block or contest moves the task
    /// past these. A task agent: launched, the task `staged` (it owes the
    /// post), `tested` (a review) or `contested` (a reply), and no report
    /// collected since its last launch or message (`REPORTED_FILE` in
    /// `git_dir`, its worktree's git dir) — a report hands the turn to the
    /// director. A commit of its own moves the task on; a rejected one is
    /// relayed, which owes the fix. Liveness is the caller's business.
    pub fn owes_work(&self, task: &state::TaskState, agent: bool, git_dir: &Path) -> bool {
        if agent {
            task.agent.is_some()
                && matches!(
                    task.status,
                    Status::Staged | Status::Tested | Status::Contested
                )
                && !git_dir.join(REPORTED_FILE).exists()
        } else {
            matches!(
                task.status,
                Status::Posted | Status::InProgress | Status::InRevision
            ) && records::has_pins(&self.hub, &task.slug)
                && records::held_commit(&self.hub, &task.slug).is_none()
        }
    }

    /// Reports a live session idle while it `owes_work`: the director is
    /// the only one who can get it going again. Two ways, one event kind:
    /// sitting at a stop (its `Stop`/`StopFailure` hook's marker), once per
    /// stop; or, for any agent, a screen unchanged for `idle_after` seconds
    /// (`watch_pane`: hung, or on a dialog), once per unchanged screen. A
    /// stop covers its whole wait: no stale report while the marker is
    /// there, and a stop on a screen already reported stale isn't reported
    /// again. A first run records what it finds without emitting; a stop
    /// that isn't owed anything isn't recorded, so it's reported if the
    /// task comes back to it unprompted.
    fn report_idle(
        &mut self,
        task: &state::TaskState,
        agent: bool,
        idle_after: u64,
    ) -> Result<(), String> {
        let (key, alive) = if agent {
            let alive = task.agent.as_ref().is_some_and(|a| a.alive);
            (state::agent_key(&task.slug), alive)
        } else {
            (task.slug.clone(), self.sessions.alive(&task.slug))
        };
        let Some(git_dir) = alive.then(|| self.session_git_dir(&key)).flatten() else {
            self.idle_told.remove(&key);
            self.panes.remove(&key);
            return Ok(());
        };
        let owes = self.owes_work(task, agent, &git_dir);
        let stale = if owes {
            self.watch_pane(&key, idle_after)
        } else {
            // Its clock starts when it next owes something.
            self.panes.remove(&key);
            None
        };
        let why = match read_stop(&git_dir) {
            Some(stop) => {
                if self.idle_told.get(&key) == Some(&stop.id) || !owes {
                    return Ok(());
                }
                let seen = self.panes.get_mut(&key);
                let told_stale = seen.as_ref().is_some_and(|p| p.told);
                if let Some(seen) = seen {
                    seen.told = true;
                }
                self.idle_told.insert(key.clone(), stop.id);
                if told_stale {
                    return Ok(());
                }
                if stop.error {
                    "stopped on an API error (a usage limit or an overload?) and is waiting for input".to_string()
                } else if agent {
                    format!(
                        "ended its turn without a commit or report while the task is {} and is waiting for input",
                        task.status.as_str()
                    )
                } else {
                    "ended its turn without a finish, block: or contest: commit and is waiting for input".to_string()
                }
            }
            None => {
                self.idle_told.remove(&key);
                let Some(secs) = stale else {
                    return Ok(());
                };
                if let Some(seen) = self.panes.get_mut(&key) {
                    seen.told = true;
                }
                let owed = if agent {
                    format!("while the task is {}", task.status.as_str())
                } else {
                    "while it owes a finish".to_string()
                };
                format!(
                    "has shown no new output for {} {owed} (hung, or waiting at a dialog or permission prompt?)",
                    duration_text(secs)
                )
            }
        };
        if !self.first_run {
            let who = if agent { "task agent" } else { "worker" };
            let session = state::tmux_session_name(&self.project, &key);
            let kind = if agent {
                EventKind::AgentIdle
            } else {
                EventKind::SessionIdle
            };
            let tail = screen_tail(self.sessions.capture(&key).as_deref());
            let message = format!(
                "task/{}'s {who} {why} (tmux session {session})\n{tail}",
                task.slug
            );
            events::append(&self.home, kind, Some(&task.slug), &message)?;
        }
        Ok(())
    }

    /// Captures session `key`'s screen and tracks how long it's been the
    /// same (`panes`): a change, a relay or a (re)launch restarts the
    /// clock. Returns how long it's been unchanged once that's at least
    /// `idle_after` seconds and it wasn't reported yet. A failed capture
    /// (the session just died) is no data: nothing changes. `idle_after`
    /// 0 turns it off.
    fn watch_pane(&mut self, key: &str, idle_after: u64) -> Option<u64> {
        if idle_after == 0 {
            self.panes.remove(key);
            return None;
        }
        let hash = pane_hash(&self.sessions.capture(key)?);
        let launch = launch_id(&self.home, key);
        let now = self.now();
        match self.panes.get(key) {
            Some(seen) if seen.hash == hash && seen.launch == launch => {
                let secs = now.saturating_sub(seen.since);
                (!seen.told && secs >= idle_after).then_some(secs)
            }
            _ => {
                let seen = PaneSeen {
                    hash,
                    launch,
                    since: now,
                    told: false,
                };
                self.panes.insert(key.to_string(), seen);
                None
            }
        }
    }

    // ---- fetch ----

    /// `git fetch --no-tags --prune <director> +refs/heads/main:refs/incoming/director/main
    /// +refs/heads/task/*:refs/incoming/director/task/*`, run in the hub.
    /// `--prune` was checked against exactly these refspecs (a plain local
    /// path source, explicit non-configured refspecs): a branch the director
    /// deletes locally is pruned from `refs/incoming/...` on the next fetch,
    /// so a stale incoming ref never lingers and there is nothing else to
    /// clean up by hand here.
    fn fetch(&self) -> Result<(), String> {
        let director = self
            .director
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", self.director.display()))?;
        self.hub
            .run(&[
                "fetch",
                "--no-tags",
                "--prune",
                director,
                "+refs/heads/main:refs/incoming/director/main",
                "+refs/heads/task/*:refs/incoming/director/task/*",
            ])
            .map(|_| ())
            .map_err(|e| format!("fetching director clone: {e}"))
    }

    // ---- director main (spec §5 director-only paths; scope `_main`) ----

    fn import_main(&mut self, report: &mut TickReport) -> Result<(), String> {
        let incoming = match self.hub.rev_parse("refs/incoming/director/main") {
            Some(sha) => sha,
            None => return Ok(()),
        };
        let hub_ref = "refs/heads/main";
        let mut hub_tip = self
            .hub
            .rev_parse(hub_ref)
            .ok_or_else(|| "hub has no main".to_string())?;

        if self.hub.is_ancestor(&incoming, &hub_tip) {
            return Ok(()); // director is behind (or exactly caught up); nothing to do
        }
        if !self.hub.is_ancestor(&hub_tip, &incoming) {
            if !records::is_rejected(&self.hub, &incoming) {
                let msg = "history: main diverged from hub; run `git reset --hard hub/main` and redo your commit".to_string();
                let rec = records::record_rejection(
                    &self.hub,
                    "_main",
                    &incoming,
                    Reason::History,
                    Actor::Director,
                    &msg,
                )?;
                report.rejected.push(rec);
            }
            return Ok(());
        }

        let commits = self
            .hub
            .rev_list(&[&format!("{hub_tip}..{incoming}"), "--reverse"]);
        let mut rejected_ancestors: Vec<String> = Vec::new();
        for c in commits {
            if records::is_rejected(&self.hub, &c) {
                continue;
            }
            if self.hub.parents(&c).len() > 1
                || rejected_ancestors
                    .iter()
                    .any(|r| self.hub.is_ancestor(r, &c))
            {
                let msg = format!(
                    "history: {} is a merge commit; only the player creates merges",
                    short(&c)
                );
                let rec = records::record_rejection(
                    &self.hub,
                    "_main",
                    &c,
                    Reason::History,
                    Actor::Director,
                    &msg,
                )?;
                report.rejected.push(rec);
                rejected_ancestors.push(c);
                continue;
            }

            match self.validate_main_commit(&hub_tip, &c) {
                Ok(()) => {
                    self.hub
                        .update_ref_cas(hub_ref, &c, &hub_tip)
                        .map_err(|e| e.to_string())?;
                    records::record_import(&self.hub, &c, Actor::Director, Transition::Docs, None)?;
                    if let Ok(raises) = self.main_raises(&hub_tip, &c) {
                        records::record_budget_raises(&self.hub, &c, &raises)?;
                    }
                    report
                        .imported
                        .push((hub_ref.to_string(), c.clone(), Transition::Docs));
                    hub_tip = c;
                }
                Err((reason, msg)) => {
                    let rec = records::record_rejection(
                        &self.hub,
                        "_main",
                        &c,
                        reason,
                        Actor::Director,
                        &msg,
                    )?;
                    report.rejected.push(rec);
                }
            }
        }
        Ok(())
    }

    /// The budget raises `candidate` makes over `base`, judged by
    /// `raise::judge` (an `ownership` message if it breaks the rules). Loads
    /// (and for a Lean package, exports and caches) the architecture at both.
    fn main_raises(&self, base: &str, candidate: &str) -> Result<Vec<raise::Raise>, String> {
        let Some(new) = self.arch_source(candidate)? else {
            return Ok(Vec::new());
        };
        let old = self.arch_source(base).ok().flatten();
        let files: Vec<String> = self.hub.diff_names(base, candidate).into_iter().map(|(_, p)| p).collect();
        raise::judge(
            old.as_ref(),
            &new,
            &self.hub.commit_subject(candidate).unwrap_or_default(),
            &self.hub.commit_body(candidate).unwrap_or_default(),
            &files,
            self.now(),
        )
    }

    /// Exports main's Lean architecture into the hub's cache once per tree,
    /// so pins and the task agent's path allowance read it even when this
    /// player never imported main (a hub `init` made from one).
    fn ensure_arch(&mut self, report: &mut TickReport) {
        let Some(main) = self.hub.rev_parse("refs/heads/main") else { return };
        let Some(tree) = self.hub.rev_parse(&format!("{main}:{}", config::ARCH_DIR)) else { return };
        if self.arch_tried.as_deref() == Some(tree.as_str()) {
            return;
        }
        self.arch_tried = Some(tree);
        if let Err(e) = self.arch_source(&main) {
            report.errors.push(format!("architecture on main: {e}"));
        }
    }

    /// The architecture at `commit` (`records::arch_source_at`), exporting a
    /// Lean package in `<home>/arch/` (jailed like a check) and caching the
    /// result on its tree the first time.
    pub fn arch_source(&self, commit: &str) -> Result<Option<raise::Source>, String> {
        records::arch_source_at(&self.hub, commit, Some(self.runner.exporter()))
    }

    /// The suite at `commit` (`records::suite_at`), exported on a miss.
    fn suite_at(&self, commit: &str) -> Result<config::Suite, String> {
        records::suite_at(&self.hub, commit, Some(self.runner.exporter()))
    }

    /// The run set of `slug` at `commit` (spec §6.6): the modules of its
    /// plan (task.toml at the hub tip, pinned since the post), through
    /// `Runner::run_set_for`.
    fn run_set_for(&self, slug: &str, commit: &str, diff_from: Option<&str>) -> Result<checks::RunSet, String> {
        let tip = self.hub.rev_parse(&format!("refs/heads/task/{slug}")).unwrap_or_else(|| commit.to_string());
        // A flaky-test-fix and a test-speedup work on the suite itself: no
        // modules, the whole suite; so does a branch with no plan (a queued
        // run). A planned task always names its modules.
        let modules = match records::task_config_at(&self.hub, &tip, slug) {
            Some(cfg) if cfg.kind == TaskKind::Task && cfg.modules.is_empty() => {
                return Err(format!("task/{slug}: a task with no modules (validation refuses this plan)"));
            }
            Some(cfg) => cfg.modules,
            None => Vec::new(),
        };
        self.runner.run_set_for(commit, &modules, diff_from)
    }

    /// Runs `slug`'s run set at `commit`; a set that can't be built fails.
    fn run_task_set(&self, slug: &str, commit: &str, diff_from: Option<&str>) -> checks::SuiteResult {
        match self.run_set_for(slug, commit, diff_from) {
            Ok(set) => self.runner.run_set(commit, &set),
            Err(e) => checks::SuiteResult::failing(e),
        }
    }

    /// The suite's `test_paths` at `commit`; none when it doesn't load.
    fn test_paths_at(&self, commit: &str) -> Vec<String> {
        self.suite_at(commit).map(|s| s.test_paths).unwrap_or_default()
    }

    /// `base` is "the current hub main" at this point in the cumulative walk
    /// (it advances with every successfully-imported commit): docs_paths is
    /// read from it, per spec.
    fn validate_main_commit(&self, base: &str, candidate: &str) -> Result<(), (Reason, String)> {
        let roadmap = self
            .roadmap_at(base)
            .map_err(|e| (Reason::Ownership, format!("ownership: {e}")))?;
        for (_, path) in self.hub.diff_names(base, candidate) {
            if !path_allowed(&path, &roadmap.docs_paths) {
                return Err((
                    Reason::Ownership,
                    format!(
                        "ownership: main commit touches {path:?}; a main commit changes only roadmap.toml, prompts.kdl, the architecture (architecture/) and docs_paths entries"
                    ),
                ));
            }
        }
        match self.hub.show_file(candidate, "roadmap.toml") {
            Some(text) => {
                let new = config::roadmap_config_from_str(&text)
                    .map_err(|e| (Reason::Ownership, format!("ownership: roadmap.toml: {e}")))?;
                let test_paths = self.test_paths_at(candidate);
                config::check_lock_files(&new.player.lock_files, &test_paths)
                    .map_err(|e| (Reason::Ownership, format!("ownership: roadmap.toml: {e}")))?;
            }
            None => {
                return Err((
                    Reason::Ownership,
                    "ownership: roadmap.toml missing".to_string(),
                ));
            }
        }
        self.arch_source(candidate).map_err(|e| (Reason::Ownership, format!("ownership: {e}")))?;
        self.suite_at(candidate).map_err(|e| (Reason::Ownership, format!("ownership: {e}")))?;
        self.main_raises(base, candidate).map_err(|e| (Reason::Ownership, e))?;
        match self.hub.show_file(candidate, prompts::FILE) {
            Some(text) => {
                Prompts::parse(&text)
                    .map_err(|e| (Reason::Ownership, format!("ownership: {e}")))?;
            }
            None => {
                return Err((
                    Reason::Ownership,
                    format!(
                        "ownership: {} missing: it holds every prompt the player sends (spec §19)",
                        prompts::FILE
                    ),
                ));
            }
        }
        let suite = self.runner.run_suite(candidate);
        self.note_ledgers("_main", candidate, &suite);
        // Failing only where main already fails impurely is main's old
        // problem, not this commit's: it mustn't block the director (who may
        // be here to change `[sandbox]`).
        if !suite.pass && self.held_by_main(&suite).is_none() {
            let reason = Self::suite_failure_reason(&suite);
            return Err((reason, format!("{}: {}", reason.as_str(), suite.summary())));
        }
        Ok(())
    }

    // ---- director task/<slug> branches ----

    fn import_task_branches(&mut self, report: &mut TickReport) -> Result<(), String> {
        let incoming: Vec<(String, String)> = self
            .hub
            .for_each_ref("refs/incoming/director/task/*")
            .into_iter()
            .filter_map(|(refname, sha)| {
                let slug = refname.strip_prefix("refs/incoming/director/task/")?;
                if slug.is_empty() {
                    None
                } else {
                    Some((slug.to_string(), sha))
                }
            })
            .collect();

        for (slug, sha) in self.order_incoming_task_branches(incoming) {
            self.import_task_branch(&slug, &sha, report)?;
        }
        Ok(())
    }

    /// Orders one tick's batch of incoming task branches so a slug whose
    /// plan depends on another slug arriving in the very same fetch is
    /// processed after that dependency, not before. Without this,
    /// `for_each_ref`'s (alphabetical) order can make a perfectly valid
    /// plan look like it depends on an "unknown task" purely because its
    /// dependency's branch happens to sort later — spuriously burning a
    /// `plan` strike (and possibly escalating the task) for a batching
    /// artifact, not a real planning mistake.
    ///
    /// This is a best-effort scheduling hint, not a validation: it peeks at
    /// each candidate's `depends_on` without rejecting anything, so a slug
    /// whose dependency truly doesn't exist (in this batch or the hub), or
    /// that's genuinely part of a cycle, is still rejected exactly as
    /// before by the real validator — just no longer position-dependent.
    fn order_incoming_task_branches(
        &self,
        incoming: Vec<(String, String)>,
    ) -> Vec<(String, String)> {
        let slugs_in_batch: BTreeSet<String> = incoming.iter().map(|(s, _)| s.clone()).collect();
        let deps: BTreeMap<String, BTreeSet<String>> = incoming
            .iter()
            .map(|(slug, sha)| {
                let path = format!("tasks/{slug}/task.toml");
                let within_batch = self
                    .hub
                    .show_file(sha, &path)
                    .and_then(|text| config::task_config_from_str(&text).ok())
                    .map(|cfg| {
                        cfg.depends_on
                            .into_iter()
                            .filter(|d| d != slug && slugs_in_batch.contains(d))
                            .collect()
                    })
                    .unwrap_or_default();
                (slug.clone(), within_batch)
            })
            .collect();

        let mut ordered: Vec<(String, String)> = Vec::with_capacity(incoming.len());
        let mut remaining = incoming;
        while !remaining.is_empty() {
            let placed: BTreeSet<&str> = ordered.iter().map(|(s, _)| s.as_str()).collect();
            let ready = remaining.iter().position(|(slug, _)| {
                deps.get(slug.as_str())
                    .is_none_or(|d| d.iter().all(|dep| placed.contains(dep.as_str())))
            });
            match ready {
                Some(i) => ordered.push(remaining.remove(i)),
                // A dependency cycle within the batch (or an unreadable
                // task.toml): nothing more can be scheduled by dependency.
                // The real validator rejects a genuine cycle regardless of
                // order, so just append the rest in their original order.
                None => {
                    ordered.extend(remaining);
                    break;
                }
            }
        }
        ordered
    }

    fn import_task_branch(
        &mut self,
        slug: &str,
        incoming: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        // A retired slug: the director's local branch is stale (they haven't
        // deleted it in their own clone yet). Ignore it entirely — not even
        // a rejection, since there's nothing to say.
        if records::is_retired(&self.hub, slug) || records::is_explored(&self.hub, slug) {
            return Ok(());
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        let commits = match self.hub.rev_parse(&hub_ref) {
            Some(hub_tip) => {
                if self.hub.is_ancestor(incoming, &hub_tip) {
                    return Ok(()); // behind or caught up
                }
                if !self.hub.is_ancestor(&hub_tip, incoming) {
                    if !records::is_rejected(&self.hub, incoming) {
                        let msg = format!(
                            "history: task/{slug} diverged from hub; run `git reset --hard hub/task/{slug}` and redo your commit"
                        );
                        let rec = records::record_rejection(
                            &self.hub,
                            slug,
                            incoming,
                            Reason::History,
                            Actor::Director,
                            &msg,
                        )?;
                        report.rejected.push(rec);
                    }
                    return Ok(());
                }
                // `--not main`: a conflict resolution brings in `main`'s own commits,
                // which were validated when they landed there.
                self.hub.rev_list(&[
                    &format!("{hub_tip}..{incoming}"),
                    "--reverse",
                    "--not",
                    "refs/heads/main",
                ])
            }
            None => {
                // Not planned yet: the range is main..incoming, and the
                // branch's base (the oldest commit's parent) must already be
                // in the hub, else the director built it on some history the
                // hub never had (spec §5's fast-forward rule, applied at
                // creation time).
                let main_tip = self
                    .hub
                    .rev_parse("refs/heads/main")
                    .ok_or_else(|| "hub has no main".to_string())?;
                if self.hub.is_ancestor(incoming, &main_tip) {
                    return Ok(()); // nothing new relative to main
                }
                let commits = self
                    .hub
                    .rev_list(&[&format!("{main_tip}..{incoming}"), "--reverse"]);
                let base_ok = match commits
                    .first()
                    .and_then(|oldest| self.hub.parents(oldest).into_iter().next())
                {
                    Some(parent) => parent == main_tip || self.hub.is_ancestor(&parent, &main_tip),
                    None => false,
                };
                if !base_ok {
                    if !records::is_rejected(&self.hub, incoming) {
                        let msg = format!(
                            "history: task/{slug} diverged from hub; run `git reset --hard hub/main` and redo your commit"
                        );
                        let rec = records::record_rejection(
                            &self.hub,
                            slug,
                            incoming,
                            Reason::History,
                            Actor::Director,
                            &msg,
                        )?;
                        report.rejected.push(rec);
                    }
                    return Ok(());
                }
                commits
            }
        };

        self.process_task_commits(slug, commits, report)
    }

    /// Walks `commits` oldest-first, cumulatively. Each commit is dispatched
    /// to the plan path or the existing-branch path depending on whether
    /// `refs/heads/task/<slug>` exists *right now* — which lets a plan commit
    /// earlier in this same walk flip the mode for the commits after it,
    /// matching spec's "the remaining commits in the range are then
    /// processed as for an existing branch."
    fn process_task_commits(
        &mut self,
        slug: &str,
        commits: Vec<String>,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let hub_ref = format!("refs/heads/task/{slug}");
        let mut rejected_ancestors: Vec<String> = Vec::new();

        for c in commits {
            if records::is_rejected(&self.hub, &c) {
                continue;
            }
            let parents = self.hub.parents(&c);
            let tip = self.hub.rev_parse(&hub_ref);
            // A merge whose first parent is the hub tip may be a conflict
            // resolution (spec §11): validated as one rather than refused
            // outright.
            if parents.len() == 2
                && tip.as_deref() == Some(parents[0].as_str())
                && !rejected_ancestors
                    .iter()
                    .any(|r| self.hub.is_ancestor(r, &c))
            {
                self.try_resolve(slug, &parents[0], &parents[1], &c, report)?;
                continue;
            }
            if parents.len() > 1
                || rejected_ancestors
                    .iter()
                    .any(|r| self.hub.is_ancestor(r, &c))
            {
                // Rejection scope is the slug even before the branch exists
                // (a plan attempt that used a merge commit): these are
                // strikes on the slug the moment it's later planned, since
                // strikes are just rejection records keyed by scope string.
                let msg = format!(
                    "history: task/{slug} commit {} is a merge commit; the only director merge is a conflict resolution, on the hub's task tip",
                    short(&c)
                );
                let rec = records::record_rejection(
                    &self.hub,
                    slug,
                    &c,
                    Reason::History,
                    Actor::Director,
                    &msg,
                )?;
                report.rejected.push(rec);
                rejected_ancestors.push(c);
                continue;
            }

            match tip {
                None => self.try_plan(slug, &c, report)?,
                Some(tip) => self.try_existing(slug, &tip, &c, report)?,
            }
        }
        Ok(())
    }

    fn try_plan(
        &mut self,
        slug: &str,
        candidate: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or_else(|| "hub has no main".to_string())?;
        match self.validate_plan(slug, &main_tip, candidate) {
            Ok(()) => {
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub
                    .update_ref_cas(&hub_ref, candidate, "")
                    .map_err(|e| e.to_string())?;
                records::record_import(
                    &self.hub,
                    candidate,
                    Actor::Director,
                    Transition::Plan,
                    Some(slug),
                )?;
                report
                    .imported
                    .push((hub_ref, candidate.to_string(), Transition::Plan));
            }
            Err(msg) => {
                let rec = records::record_rejection(
                    &self.hub,
                    slug,
                    candidate,
                    Reason::Plan,
                    Actor::Director,
                    &msg,
                )?;
                report.rejected.push(rec);
            }
        }
        Ok(())
    }

    /// slug unused (incl. retired); the cumulative diff from `main_tip` adds
    /// only `tasks/<slug>/task.toml`; then the shared task.toml validation.
    fn validate_plan(&self, slug: &str, main_tip: &str, candidate: &str) -> Result<(), String> {
        if !valid_slug(slug) {
            return Err(format!(
                "plan: {slug:?} is not a valid slug (must match [a-z0-9][a-z0-9-]*)"
            ));
        }
        if self.hub.ref_exists(&format!("refs/retired/{slug}")) {
            return Err(format!(
                "plan: task/{slug} is retired; slugs are never reused"
            ));
        }
        let accepted = records::accepted_tasks(&self.hub, main_tip);
        if accepted.contains_key(slug) {
            return Err(format!(
                "plan: task/{slug} is already accepted; slugs are never reused"
            ));
        }
        if records::is_explored(&self.hub, slug) {
            return Err(format!(
                "plan: task/{slug} is an explored spike; slugs are never reused"
            ));
        }

        let expected_path = format!("tasks/{slug}/task.toml");
        let diffs = self.hub.diff_names(main_tip, candidate);
        if diffs.len() != 1 || diffs[0].0 != 'A' || diffs[0].1 != expected_path {
            return Err(format!(
                "plan: task/{slug}'s commits may only add {expected_path}"
            ));
        }

        let text = self
            .hub
            .show_file(candidate, &expected_path)
            .ok_or_else(|| format!("plan: {expected_path} missing at {candidate}"))?;
        let cfg = config::task_config_from_str(&text)
            .map_err(|e| format!("plan: {expected_path}: {e}"))?;
        self.validate_task_config(slug, main_tip, &cfg)
    }

    /// Dispatches a single cumulative candidate `C` on an existing
    /// `task/<slug>` branch (tip `T` before this commit) to whichever
    /// transition its shape matches (spec §9): an empty commit is a marker
    /// (`accept:`/`retire:`, or `ownership` for anything else empty, e.g. a
    /// director-authored `block:`); a non-empty commit is a `post` (adds
    /// `prompt.md`), a `replan` (touches only `task.toml`, on a not-yet-
    /// posted branch), a `send-back` (the task is `tested`), or `ownership`
    /// (anything else, including any commit while the task is in a status
    /// the director may not touch).
    fn try_existing(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let status = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .map(|t| t.status);

        let diffs = self.hub.diff_names(tip, candidate);
        let cumulative_empty = diffs.is_empty();

        if self.hub.is_empty_commit(candidate) {
            return self.try_marker(slug, tip, candidate, cumulative_empty, status, report);
        }

        let ctx = self.fsm_ctx(&hub_state, slug);
        let verdict = |mv: Move| ctx.map(|c| fsm::decide(Actor::Director, mv, c));
        match status {
            Some(Status::Pending | Status::Ready | Status::Staged | Status::Orphaned) => {
                let prompt_path = format!("tasks/{slug}/prompt.md");
                let has_prompt = self.hub.blob_hash(candidate, &prompt_path).is_some();
                if has_prompt && verdict(Move::Post) == Some(Verdict::Reject(Reason::Ownership)) {
                    let msg = format!(
                        "task/{slug}'s post is its task agent's (spec §17), not the director's; with `[player] task_agent_cmd = \"\"` the director posts itself"
                    );
                    return self.reject(slug, candidate, Reason::Ownership, msg, report);
                }
                if has_prompt {
                    return self.try_post(
                        slug,
                        tip,
                        candidate,
                        status.unwrap(),
                        &hub_state,
                        Actor::Director,
                        report,
                    );
                }
                let task_toml_path = format!("tasks/{slug}/task.toml");
                if diffs.len() == 1 && diffs[0].1 == task_toml_path {
                    if let Some(Verdict::Reject(reason)) = verdict(Move::Replan) {
                        let msg = format!(
                            "{}: task/{slug}'s task agent is already running on this plan (its prompt was written at launch): retire the task to change it",
                            reason.as_str()
                        );
                        return self.reject(slug, candidate, reason, msg, report);
                    }
                    return self.try_replan(slug, tip, candidate, report);
                }
                let msg = format!(
                    "task/{slug} is {}: this commit adds no prompt.md (not a post) and touches more than task.toml (not a replan)",
                    status.unwrap().as_str()
                );
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
            Some(Status::Tested) if ctx.is_some_and(|c| !c.spike) && is_trim(slug, &diffs) => {
                match verdict(Move::Trim) {
                    Some(Verdict::Allow) => self.try_trim(
                        slug,
                        tip,
                        candidate,
                        &diffs,
                        &hub_state,
                        Actor::Director,
                        report,
                    ),
                    Some(Verdict::Reject(reason)) => {
                        let msg = format!("trim: task/{slug} must be tested to trim its tests");
                        self.reject(slug, candidate, reason, msg, report)
                    }
                    _ => Ok(()),
                }
            }
            Some(Status::Tested) => match verdict(Move::SendBack) {
                Some(Verdict::Allow) if ctx.is_some_and(|c| c.spike) => {
                    self.try_spike_send_back(slug, tip, candidate, &diffs, &hub_state, report)
                }
                Some(Verdict::Allow) => self.try_send_back(
                    slug,
                    tip,
                    candidate,
                    &diffs,
                    &hub_state,
                    Actor::Director,
                    report,
                ),
                Some(Verdict::Reject(reason)) => {
                    let msg = format!(
                        "task/{slug}'s send-backs are its task agent's (spec §17), not the director's: accept, or ask it (`dashboard ask {slug}`)"
                    );
                    self.reject(slug, candidate, reason, msg, report)
                }
                _ => Ok(()),
            },
            Some(Status::Contested) => match verdict(Move::ContestReply) {
                Some(Verdict::Allow) => self.try_contest_reply(
                    slug,
                    tip,
                    candidate,
                    &diffs,
                    &hub_state,
                    Actor::Director,
                    report,
                ),
                Some(Verdict::Reject(reason)) => {
                    let msg = format!(
                        "task/{slug}'s contest is its task agent's to answer (spec §18), not the director's: ask it (`dashboard ask {slug}`)"
                    );
                    self.reject(slug, candidate, reason, msg, report)
                }
                _ => Ok(()),
            },
            Some(Status::Conflicted) => {
                let main = hub_state
                    .tasks
                    .iter()
                    .find(|t| t.slug == slug)
                    .and_then(|t| t.conflict.as_ref())
                    .map(|c| c.main.clone())
                    .unwrap_or_default();
                let msg = format!(
                    "task/{slug} is conflicted: resolve it with a merge of main {main} on the hub's task tip (or retire it)"
                );
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
            Some(other) => {
                let reason = match verdict(Move::Other) {
                    Some(Verdict::Reject(reason)) => reason,
                    Some(Verdict::Ignore | Verdict::Hold) => return Ok(()),
                    _ => Reason::Ownership,
                };
                let msg = format!(
                    "task/{slug} is {} — the director may not commit until the worker finishes",
                    other.as_str()
                );
                self.reject(slug, candidate, reason, msg, report)
            }
            None => {
                let msg =
                    format!("task/{slug} has no derivable status; refusing to guess a transition");
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
        }
    }

    /// Records a rejection under `reason` for `candidate` and pushes it onto
    /// the report. Shared by every `try_existing` sub-handler's error path.
    fn reject(
        &mut self,
        slug: &str,
        candidate: &str,
        reason: Reason,
        msg: String,
        report: &mut TickReport,
    ) -> Result<(), String> {
        self.reject_by(Actor::Director, slug, candidate, reason, msg, report)
    }

    /// `reject` for a commit by `actor`: a task agent's rejection is also
    /// relayed to its session (spec §17), like a worker's; a director's
    /// never is.
    fn reject_by(
        &mut self,
        actor: Actor,
        slug: &str,
        candidate: &str,
        reason: Reason,
        msg: String,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let rec = records::record_rejection(&self.hub, slug, candidate, reason, actor, &msg)?;
        report.rejected.push(rec);
        if actor == Actor::TaskAgent {
            let prompts = self.prompts()?;
            let recovery = match reason {
                Reason::History => "task-agent.recovery-history",
                _ => "task-agent.recovery-other",
            };
            let recovery = prompts.render(recovery, &[("slug", slug)]);
            self.relay(
                &state::agent_key(slug),
                &prompts.render(
                    "task-agent.rejected",
                    &[("message", &msg), ("recovery", &recovery)],
                ),
            );
        }
        Ok(())
    }

    /// `fsm`'s view of `slug` in `hub_state`, or none for an unknown task.
    fn fsm_ctx(&self, hub_state: &state::HubState, slug: &str) -> Option<fsm::Ctx> {
        let task = hub_state.tasks.iter().find(|t| t.slug == slug)?;
        Some(fsm::Ctx {
            status: task.status,
            managed: state::task_agent_managed(&hub_state.roadmap, &task.config),
            spike: task.spike.is_some(),
            agent: task.agent.is_some(),
        })
    }

    /// The fsm verdict for the director making `mv` on `slug`'s branch now.
    /// An unknown task has no context: `None`.
    fn director_verdict(&self, slug: &str, mv: Move) -> Result<Option<Verdict>, String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        Ok(self
            .fsm_ctx(&hub_state, slug)
            .map(|c| fsm::decide(Actor::Director, mv, c)))
    }

    /// An empty commit on an existing branch: a marker (spec §5, §9). Only
    /// meaningful when the cumulative diff from `tip` is *also* empty — if
    /// it isn't, a rejected non-empty commit sits between `tip` and
    /// `candidate` (this empty commit is just the tip of that same broken
    /// stack), so it's rejected as `ownership` rather than silently treated
    /// as a marker on old, already-rejected content.
    fn try_marker(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        cumulative_empty: bool,
        status: Option<Status>,
        report: &mut TickReport,
    ) -> Result<(), String> {
        if !cumulative_empty {
            let msg = format!(
                "task/{slug} commit {} is an empty marker, but its content since {} isn't (a rejected non-empty commit sits between): fix that commit instead",
                short(candidate),
                short(tip)
            );
            return self.reject(slug, candidate, Reason::Ownership, msg, report);
        }

        let subject = self.hub.commit_subject(candidate).unwrap_or_default();
        if subject.starts_with("accept:") {
            return self.try_accept(slug, tip, candidate, status, report);
        }
        if subject.starts_with("retire:") {
            return self.try_retire(slug, tip, candidate, status, report);
        }
        if subject.starts_with("conclude:") {
            return self.try_conclude(slug, tip, candidate, status, report);
        }
        if subject.starts_with("stage:") {
            return self.try_stage(slug, tip, candidate, status, true, report);
        }
        if subject.starts_with("unstage:") {
            return self.try_stage(slug, tip, candidate, status, false, report);
        }
        let msg = format!(
            "task/{slug} commit {} is an empty marker with subject {subject:?} — only `accept:`/`retire:`/`conclude:`/`stage:`/`unstage:` are the director's to make here",
            short(candidate)
        );
        self.reject(slug, candidate, Reason::Ownership, msg, report)
    }

    // ---- stage / unstage ----

    /// A `stage:` releases a `ready` task: its task agent launches (a
    /// `ready` task never starts on its own). An `unstage:` puts a `staged`
    /// one back, while no task agent exists yet. Neither moves anything but
    /// the branch tip. A spike is never staged: it has no task agent.
    fn try_stage(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        status: Option<Status>,
        stage: bool,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let (marker, needs, transition) = if stage {
            ("stage", Status::Ready, Transition::Stage)
        } else {
            ("unstage", Status::Staged, Transition::Unstage)
        };
        let mv = if stage { Move::Stage } else { Move::Unstage };
        let problem = match self.director_verdict(slug, mv)? {
            Some(Verdict::Allow) => None,
            Some(Verdict::Ignore | Verdict::Hold) => return Ok(()),
            _ if status != Some(needs) => Some(format!(
                "{marker}: task/{slug} is {} — only a {} task can be {marker}d",
                status.map(|s| s.as_str()).unwrap_or("unknown"),
                needs.as_str()
            )),
            _ if stage => Some(format!(
                "stage: task/{slug} is a spike — it has no task agent to start"
            )),
            _ => Some(format!(
                "unstage: task/{slug}'s task agent is already running — retire the task to stop it"
            )),
        };
        if let Some(msg) = problem {
            return self.reject(slug, candidate, Reason::Plan, msg, report);
        }
        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .update_ref_cas(&hub_ref, candidate, tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            Actor::Director,
            transition,
            Some(slug),
        )?;
        report
            .imported
            .push((hub_ref, candidate.to_string(), transition));
        Ok(())
    }

    // ---- post (spec §9, §6.3) ----

    fn try_post(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        status: Status,
        hub_state: &state::HubState,
        actor: Actor,
        report: &mut TickReport,
    ) -> Result<(), String> {
        // A flake that doesn't reproduce is `evidence`, not `post`: the
        // director made no structural mistake (it has its own strike limit,
        // 1 by default, and the player retires the task itself).
        let mut reason = Reason::Post;
        let validated = if records::is_spike_at(&self.hub, candidate, slug) {
            self.validate_spike_post(slug, tip, candidate, status, actor, hub_state)
        } else {
            self.validate_post(slug, candidate, status, actor, hub_state)
                .and_then(|()| {
                    self.evidence_miss(slug, candidate).map_or(Ok(()), |miss| {
                        reason = Reason::Evidence;
                        Err(miss)
                    })
                })
        };
        match validated {
            Ok(()) => {
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub
                    .update_ref_cas(&hub_ref, candidate, tip)
                    .map_err(|e| e.to_string())?;
                records::record_import(&self.hub, candidate, actor, Transition::Post, Some(slug))?;
                report
                    .imported
                    .push((hub_ref, candidate.to_string(), Transition::Post));
                // Launching the worker clone + tmux session happens in
                // `launch_workers`, later in the same tick (spec §8.3): it
                // scans every `Posted` task without pins yet, which this one
                // now is.
            }
            Err(msg) => {
                self.reject_by(actor, slug, candidate, reason, msg.clone(), report)?;
                if reason == Reason::Evidence {
                    let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
                    let strikes = hub_state
                        .tasks
                        .iter()
                        .find(|t| t.slug == slug)
                        .and_then(|t| t.strikes.get(&Reason::Evidence).copied())
                        .unwrap_or(0);
                    if strikes >= hub_state.roadmap.limits.strikes.limit(Reason::Evidence) {
                        self.auto_retire(slug, tip, &msg, report)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// For a flaky-test-fix at `commit`: the text of the first `[[evidence]]`
    /// that doesn't reproduce the flake there (run on `main`'s code plus the
    /// task's own files, cached by commit). `None` for any other task or
    /// when every evidence reproduces.
    fn evidence_miss(&self, slug: &str, commit: &str) -> Option<String> {
        let cfg = records::task_config_at(&self.hub, commit, slug)?;
        if cfg.kind == TaskKind::TestSpeedup {
            return self.slow_miss(&cfg);
        }
        if cfg.kind != TaskKind::FlakyTestFix {
            return None;
        }
        cfg.evidence
            .iter()
            .find_map(|e| flaky::miss(e, &self.runner.run_evidence(commit, e)))
    }

    /// For a test-speedup: why its `[[slow]]` evidence doesn't hold on
    /// `main` now (read from the cached suite run, not re-measured): `main`
    /// isn't over its budget, a named check isn't slow there (killed, or a
    /// tenth of `suite_fail` or more), or a check killed there isn't named,
    /// which the worker could never get the suite passing without.
    fn slow_miss(&self, cfg: &TaskConfig) -> Option<String> {
        let main_tip = self.hub.rev_parse("refs/heads/main")?;
        let Some(over) = &self.main_over else {
            return Some(format!(
                "evidence did not reproduce: main's suite is within its [budget] at {}",
                short(&main_tip)
            ));
        };
        let suite_fail_ms = self.runner.budget().suite_fail * 1000;
        let is_slow = |name: &str| {
            over.timed_out.contains(name)
                || (suite_fail_ms > 0
                    && over.millis.get(name).is_some_and(|m| {
                        *m * 100 >= suite_fail_ms * speedup::SLOW_SHARE_PERCENT
                    }))
        };
        for s in &cfg.slow {
            if !over.millis.contains_key(&s.check) {
                return Some(format!(
                    "evidence did not reproduce: {:?} is not a check of main's suite",
                    s.check
                ));
            }
            if !is_slow(&s.check) {
                return Some(format!(
                    "evidence did not reproduce: {:?} is not slow on main ({}; slow means killed, or {}% of suite_fail)",
                    s.check,
                    checks::fmt_secs(over.millis[&s.check]),
                    speedup::SLOW_SHARE_PERCENT
                ));
            }
        }
        over.timed_out
            .iter()
            .find(|t| !cfg.slow.iter().any(|s| &s.check == *t))
            .map(|t| {
                format!(
                    "evidence is incomplete: {t:?} is killed on main too, so name it in a [[slow]] (the suite can't pass until it is faster)"
                )
            })
    }

    /// The player's own retire, for a dead end that needs no director
    /// judgement (a flake that doesn't reproduce): the branch moves to
    /// `refs/retired/<slug>` under a player-made marker commit on top of
    /// `tip` whose body is the post-mortem. There is no worker work to
    /// review, so no `retire:` commit is needed.
    fn auto_retire(
        &mut self,
        slug: &str,
        tip: &str,
        post_mortem: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let tree = self
            .hub
            .rev_parse(&format!("{tip}^{{tree}}"))
            .ok_or_else(|| format!("no tree at {tip}"))?;
        let subject = post_mortem.split(" (").next().unwrap_or(post_mortem);
        let marker = self
            .hub
            .commit_tree(
                &tree,
                &[tip],
                &format!("retire: {subject}\n\n{post_mortem}"),
            )
            .map_err(|e| e.to_string())?;

        self.hub
            .update_ref(&format!("refs/retired/{slug}"), &marker)
            .map_err(|e| e.to_string())?;
        self.hub
            .delete_ref_cas(&format!("refs/heads/task/{slug}"), tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            &marker,
            Actor::Player,
            Transition::Retire,
            Some(slug),
        )?;
        report
            .imported
            .push((format!("refs/retired/{slug}"), marker, Transition::Retire));

        self.end_task_sessions(slug);
        Ok(())
    }

    /// spec §9's post row: status must be `ready` (other tasks may be in
    /// flight: spec §4's parallel tasks); every accepted dependency's merge must already be an
    /// ancestor of `candidate` (else the branch hasn't been unblocked into
    /// yet); `task.toml` must parse and declare a check; its suite file
    /// `architecture/Tests/<slug>.lean` must carry a matching check and the whole suite
    /// must load (spec §6.3); and that check must currently
    /// *fail* (spec §3.1's evidence requirement).
    fn validate_post(
        &self,
        slug: &str,
        candidate: &str,
        status: Status,
        actor: Actor,
        hub_state: &state::HubState,
    ) -> Result<(), String> {
        let task = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .ok_or_else(|| format!("post: task/{slug}: no task state"))?;

        let may_post = self
            .fsm_ctx(hub_state, slug)
            .is_some_and(|c| fsm::decide(actor, Move::Post, c) == Verdict::Allow);
        if !may_post {
            if status == Status::Orphaned {
                let retired = records::retired_slugs(&self.hub);
                let retired_deps: Vec<&str> = task
                    .config
                    .depends_on
                    .iter()
                    .filter(|d| retired.iter().any(|r| r == *d))
                    .map(|s| s.as_str())
                    .collect();
                return Err(format!(
                    "post: task/{slug} is orphaned: depends on retired {}",
                    retired_deps.join(", ")
                ));
            }
            let accepted = records::accepted_tasks(&self.hub, &hub_state.main_tip);
            let unmet: Vec<&str> = task
                .config
                .depends_on
                .iter()
                .filter(|d| !accepted.contains_key(*d))
                .map(|s| s.as_str())
                .collect();
            return Err(format!(
                "post: task/{slug}: dependencies not accepted: {}",
                unmet.join(", ")
            ));
        }

        let accepted = records::accepted_tasks(&self.hub, &hub_state.main_tip);
        for dep in &task.config.depends_on {
            if let Some(merge_sha) = accepted.get(dep) {
                if !self.hub.is_ancestor(merge_sha, candidate) {
                    return Err(format!(
                        "post: task/{slug}: wait for the player to merge main into the branch, then fetch (missing {dep}'s acceptance merge)"
                    ));
                }
            }
        }

        let task_toml_path = format!("tasks/{slug}/task.toml");
        let text = self
            .hub
            .show_file(candidate, &task_toml_path)
            .ok_or_else(|| format!("post: task/{slug}: {task_toml_path} missing"))?;
        let cfg = config::task_config_from_str(&text)
            .map_err(|e| format!("post: task/{slug}: {task_toml_path}: {e}"))?;
        let suite = self.suite_at(candidate).map_err(|e| format!("post: task/{slug}: {e}"))?;
        let tests_path = config::tests_file(slug);
        let own = suite.from_file(&tests_path);
        let has_file = self.hub.blob_hash(candidate, &tests_path).is_some();
        if cfg.kind == TaskKind::TestSpeedup {
            // No task check: its evidence is the named checks' times on main
            // (`evidence_miss`), and it adds nothing to the suite.
            if has_file {
                return Err(format!(
                    "post: task/{slug}: a test-speedup adds no check to the suite ({tests_path})"
                ));
            }
            if suite.list_cmd.is_none() {
                return Err(format!(
                    "post: task/{slug}: a test-speedup needs `list_cmd` in the `suite where` block of {} (the player lists the tests it removes with it)",
                    config::TESTS_LEAN
                ));
            }
            return Ok(());
        }
        let check = cfg
            .check
            .ok_or_else(|| format!("post: task/{slug}: {task_toml_path} has no [check] section"))?;
        if cfg.kind == TaskKind::FlakyTestFix {
            // The stress check is a one-off proof for this task, an exception
            // to spec §6.3: it must not slow every future suite.
            if has_file || suite.check(&check.name).is_some() {
                return Err(format!(
                    "post: task/{slug}: a flaky-test-fix's check {:?} must not be added to the suite ({} or {tests_path})",
                    check.name,
                    config::TESTS_LEAN
                ));
            }
            if suite.list_cmd.is_none() {
                return Err(format!(
                    "post: task/{slug}: a flaky-test-fix needs `list_cmd` in the `suite where` block of {} (the player compares test inventories with it)",
                    config::TESTS_LEAN
                ));
            }
        } else {
            // A task's checks test what it changes: within its modules.
            for c in &own {
                if c.all {
                    return Err(format!("post: task/{slug}: check {} in {tests_path} covers all; a task's checks cover its modules ({})", c.name, cfg.modules.join(", ")));
                }
                if let Some(n) = c.covers.iter().find(|n| !cfg.modules.contains(n)) {
                    return Err(format!("post: task/{slug}: check {} in {tests_path} covers {n}, outside the task's modules ({})", c.name, cfg.modules.join(", ")));
                }
            }
            // The check must live in the task's own file, not Tests.lean, so
            // parallel posts never edit the same file.
            if !own.iter().any(|c| {
                c.name == check.name
                    && c.cmd == check.cmd
                    && c.runs == check.runs
                    && c.min_pass == check.min_pass
            }) {
                return Err(format!(
                    "post: task/{slug}: {tests_path} has no check matching the task check {:?} (same name, cmd, runs, min_pass)",
                    check.name
                ));
            }
        }

        let result = self
            .runner
            .run_task_check(candidate, slug)
            .map_err(|e| format!("post: task/{slug}: {e}"))?;
        if let Some(why) = result.impurity() {
            let _ = records::record_impurity(&self.hub, slug, candidate, &result);
            return Err(format!("post: task/{slug}: {why}"));
        }
        if result.pass {
            return Err(format!(
                "post: task/{slug}: task check already passes — it must fail at post"
            ));
        }

        Ok(())
    }

    // ---- send-back (spec §9, §8.3) ----

    /// spec §9's send-back row: the cumulative diff must add exactly the
    /// next review file (`reviews/r<N+1>.md`, `N` = the current review
    /// round); every other changed path must be a test path (spec §6.2) or
    /// a suite file (`Tests.lean`, `Tests/`); a round beyond `max_review_rounds` escalates
    /// instead of importing; and the project suite must now *fail* (spec
    /// §3.1's evidence requirement for a send-back). On success, re-pins and
    /// relays the review note to the worker's session (spec §8.3).
    fn try_send_back(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        diffs: &[(char, String)],
        hub_state: &state::HubState,
        actor: Actor,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let task = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .ok_or_else(|| format!("send-back: task/{slug}: no task state"))?;
        let n = task.review_round;
        let expected_path = format!("tasks/{slug}/reviews/r{}.md", n + 1);
        let review_prefix = format!("tasks/{slug}/reviews/");

        let added_a_review_file = diffs.iter().any(|(status, path)| {
            *status == 'A' && path.starts_with(&review_prefix) && is_review_filename(path)
        });
        let added_expected_file = diffs
            .iter()
            .any(|(status, path)| *status == 'A' && *path == expected_path);

        if !added_expected_file {
            if !added_a_review_file {
                let msg = format!(
                    "task/{slug}'s commit doesn't add a review file (expected {expected_path})"
                );
                return self.reject_by(actor, slug, candidate, Reason::Ownership, msg, report);
            }
            let msg = format!(
                "send-back: task/{slug}: expected review round {} to add {expected_path}",
                n + 1
            );
            return self.reject_by(actor, slug, candidate, Reason::Review, msg, report);
        }

        let scope = self.agent_scope(slug, tip);
        for (_, path) in diffs {
            if path_under_test_paths(path, &scope) {
                continue;
            }
            let msg = format!(
                "send-back: task/{slug}: a send-back may only add the review note and tests, not {path:?}"
            );
            return self.reject_by(actor, slug, candidate, Reason::Review, msg, report);
        }

        let round = n + 1;
        if round > hub_state.roadmap.limits.max_review_rounds {
            let cause = format!(
                "review rounds exceeded: {round} > {}",
                hub_state.roadmap.limits.max_review_rounds
            );
            records::record_escalation(&self.hub, slug, candidate, &cause)?;
            report.escalated.push((slug.to_string(), cause));
            return Ok(());
        }

        let suite = self.run_task_set(slug, candidate, Some(tip));
        self.note_ledgers(slug, candidate, &suite);
        if suite.pass {
            let msg = format!(
                "send-back: task/{slug}: send-back without evidence: the task's run set still passes"
            );
            return self.reject_by(actor, slug, candidate, Reason::Review, msg, report);
        }
        // Nor is one failing only where main already does (a check killed
        // for its timeout there too).
        if let Some(names) = self.held_by_main(&suite) {
            let msg = format!(
                "send-back: task/{slug}: send-back without evidence: the suite fails only on checks that already fail on main: {}",
                names.join(", ")
            );
            return self.reject_by(actor, slug, candidate, Reason::Review, msg, report);
        }
        // A suite failing only because a test dirtied its worktree is no
        // evidence against the worker's code.
        if let Some(why) = suite.impurity() {
            let msg = format!("send-back: task/{slug}: {why}");
            return self.reject_by(actor, slug, candidate, Reason::Review, msg, report);
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .update_ref_cas(&hub_ref, candidate, tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            actor,
            Transition::SendBack,
            Some(slug),
        )?;
        report
            .imported
            .push((hub_ref, candidate.to_string(), Transition::SendBack));

        self.pin(slug, candidate, report)?;
        let review_body = self
            .hub
            .show_file(candidate, &expected_path)
            .unwrap_or_default();
        let text = self.prompts()?.render(
            "worker.review",
            &[
                ("round", &round.to_string()),
                ("slug", slug),
                ("review", &review_body),
            ],
        );
        self.relay(slug, &text);
        Ok(())
    }

    // ---- trim (spec §6.5) ----

    /// A commit on a `tested` task that drops tests the task added from the
    /// suite: the director keeps only those that matter to the application.
    /// It may delete or edit test files the task added (absent on `main`)
    /// and remove checks from `architecture/Tests/<slug>.lean` and
    /// `<slug>.worker.lean` (with their `files`); the suite
    /// must still pass. The task stays `tested`, re-pinned at the trim, and
    /// its task agent is asked for no new review.
    fn try_trim(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        diffs: &[(char, String)],
        hub_state: &state::HubState,
        actor: Actor,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or_else(|| "hub has no main".to_string())?;
        let own = [config::tests_file(slug), config::worker_tests_file(slug)];
        let (before, after) = match (self.suite_at(tip), self.suite_at(candidate)) {
            (Ok(b), Ok(a)) => (b, a),
            (_, Err(e)) | (Err(e), _) => {
                let msg = format!("trim: task/{slug}: {e}");
                return self.reject_by(actor, slug, candidate, Reason::Accept, msg, report);
            }
        };
        if let Some(extra) = trim_additions(&before, &after, &own) {
            let msg = format!(
                "trim: task/{slug}: {} and {} may only lose checks; {extra:?} is new or changed",
                own[0], own[1]
            );
            return self.reject_by(actor, slug, candidate, Reason::Accept, msg, report);
        }
        // The files of the checks the trim removed are its to delete too.
        let dropped: Vec<&str> = before
            .checks
            .iter()
            .filter(|c| after.check(&c.name).is_none())
            .map(|c| c.name.as_str())
            .collect();
        let dropped_files = before.files_of(&dropped);
        for (status, path) in diffs {
            if own.contains(path) {
                continue;
            }
            let added_by_task = self.hub.blob_hash(&main_tip, path).is_none();
            if *status != 'A'
                && added_by_task
                && (path_under_test_paths(path, &before.test_paths) || path_under_test_paths(path, &dropped_files))
            {
                continue;
            }
            let msg = format!(
                "trim: task/{slug}: a trim only deletes or edits test files this task added and removes checks from {}, not {path:?} (a send-back adds tasks/{slug}/reviews/r<N>.md)",
                own[0]
            );
            return self.reject_by(actor, slug, candidate, Reason::Accept, msg, report);
        }

        let suite = self.run_task_set(slug, candidate, Some(tip));
        self.note_ledgers(slug, candidate, &suite);
        if !suite.pass {
            let msg = format!(
                "trim: task/{slug}: the run set fails without the trimmed tests\n{}",
                suite.summary()
            );
            return self.reject_by(actor, slug, candidate, Reason::Accept, msg, report);
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .update_ref_cas(&hub_ref, candidate, tip)
            .map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, actor, Transition::Trim, Some(slug))?;
        report
            .imported
            .push((hub_ref, candidate.to_string(), Transition::Trim));
        self.pin(slug, candidate, report)?;
        // Already reviewed: the trim is the review's outcome, not new work.
        let agent = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .is_some_and(|t| t.agent.is_some());
        if agent && records::agent_asked(&self.hub, slug).as_deref() == Some(tip) {
            records::set_agent_asked(&self.hub, slug, candidate)?;
        }
        Ok(())
    }

    // ---- contest (spec §18) ----

    /// A worker's `contest:` marker: a test it must pass is wrong. Like a
    /// block it must be a clean marker on the hub tip, but it doesn't
    /// escalate: the task is `contested` until the task agent (or the
    /// director) replies. A contest beyond `max_contest_rounds` escalates
    /// instead of importing, as a send-back beyond `max_review_rounds` does.
    fn try_contest(
        &mut self,
        slug: &str,
        hub_tip: &str,
        candidate: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        if !self.hub.diff_names(hub_tip, candidate).is_empty() {
            let msg = format!(
                "history: task/{slug}'s contest: commit isn't a clean reset to the hub tip — reset to hub, then contest"
            );
            return self.reject_and_relay(slug, candidate, Reason::History, msg, report);
        }
        if records::is_spike_at(&self.hub, hub_tip, slug) {
            let msg = format!(
                "suite: task/{slug} is a spike: it has no director's test to contest (block: instead)"
            );
            return self.reject_and_relay(slug, candidate, Reason::Suite, msg, report);
        }
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let n = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .map(|t| t.contest_round)
            .unwrap_or(0)
            + 1;
        let max = hub_state.roadmap.limits.max_contest_rounds;
        if n > max {
            let subject = self.hub.commit_subject(candidate).unwrap_or_default();
            let cause = format!("contest rounds exceeded: {n} > {max} ({subject})");
            records::record_escalation(&self.hub, slug, candidate, &cause)?;
            report.escalated.push((slug.to_string(), cause));
            return Ok(());
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .update_ref_cas(&hub_ref, candidate, hub_tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            Actor::Worker,
            Transition::Contest,
            Some(slug),
        )?;
        report
            .imported
            .push((hub_ref, candidate.to_string(), Transition::Contest));
        let text = self
            .prompts()?
            .render("worker.contest-recorded", &[("n", &n.to_string())]);
        self.relay(slug, &text);
        Ok(())
    }

    /// The reply to a contest, by the task agent or (task agents off) the
    /// director: one commit adding `tasks/<slug>/contests/c<N>.md` and
    /// optionally fixed tests, nothing else. No suite requirement, and no
    /// review round spent: the reply may keep the test and explain it. The
    /// player re-pins and relays the reply to the worker.
    fn try_contest_reply(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        diffs: &[(char, String)],
        hub_state: &state::HubState,
        actor: Actor,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let task = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .ok_or_else(|| format!("contest: task/{slug}: no task state"))?;
        let n = task.contest_round + 1;
        let expected = format!("tasks/{slug}/contests/c{n}.md");
        if !diffs.iter().any(|(st, p)| *st == 'A' && *p == expected) {
            let msg = format!("ownership: task/{slug} is contested: the reply must add {expected}");
            return self.reject_by(actor, slug, candidate, Reason::Ownership, msg, report);
        }
        let scope = self.agent_scope(slug, tip);
        if let Some((_, path)) = diffs.iter().find(|(_, p)| !path_under_test_paths(p, &scope)) {
            let msg = format!(
                "ownership: task/{slug}: a contest reply may only add the reply and fix tests, not {path:?}"
            );
            return self.reject_by(actor, slug, candidate, Reason::Ownership, msg, report);
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .update_ref_cas(&hub_ref, candidate, tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            actor,
            Transition::ContestReply,
            Some(slug),
        )?;
        report
            .imported
            .push((hub_ref, candidate.to_string(), Transition::ContestReply));

        self.pin(slug, candidate, report)?;
        let reply = self.hub.show_file(candidate, &expected).unwrap_or_default();
        let text = self.prompts()?.render(
            "worker.contest-reply",
            &[("n", &n.to_string()), ("slug", slug), ("reply", &reply)],
        );
        self.relay(slug, &text);
        Ok(())
    }

    // ---- accept (spec §9, §11) ----

    /// spec §9/§11's accept row: the task must be `tested` and not a spike.
    /// The player then lands it itself (`land`), merging `main` into the
    /// branch first if other tasks moved it.
    fn try_accept(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        status: Option<Status>,
        report: &mut TickReport,
    ) -> Result<(), String> {
        match self.director_verdict(slug, Move::Accept)? {
            Some(Verdict::Allow) => {}
            Some(Verdict::Reject(reason)) => {
                let msg = if status == Some(Status::Tested) {
                    format!(
                        "accept: task/{slug} is a spike: it never merges into main (finish it with `conclude:`)"
                    )
                } else {
                    format!(
                        "accept: task/{slug} is {} — it must be tested before it can be accepted",
                        status.map(|s| s.as_str()).unwrap_or("unknown")
                    )
                };
                return self.reject(slug, candidate, reason, msg, report);
            }
            Some(Verdict::Ignore | Verdict::Hold) => return Ok(()),
            None => {
                let msg = format!(
                    "accept: task/{slug} is unknown — it must be tested before it can be accepted"
                );
                return self.reject(slug, candidate, Reason::Accept, msg, report);
            }
        }

        self.land(slug, tip, candidate, Transition::Accept, report)
    }

    /// Lands `candidate` (an `accept:` marker, or a director's resolution of
    /// a conflict) on top of the hub tip `tip` (spec §11). When `main` has
    /// moved since the branch last saw it, the player merges `main` into the
    /// branch itself (a `sync` commit); a conflict stops there, with the
    /// task `conflicted` and the exact `main` commit and paths noted on
    /// `candidate` for the director to resolve. Otherwise the branch merges
    /// into `main` (`--no-ff`, `Task: <slug>` trailer) once the suite passes
    /// on the result. A suite that fails only because of the merge sends the
    /// task back to the worker (`sync_revision`, no review round spent).
    /// What landing `slug` runs on `merge_sha` (spec §11): the run set of
    /// the task's modules plus every module whose paths `main` changed since
    /// the merge base; the whole suite when either side touched `shared`.
    fn landing_set(&self, slug: &str, candidate: &str, main_tip: &str, merge_sha: &str) -> Result<checks::RunSet, String> {
        let tip = self.hub.rev_parse(&format!("refs/heads/task/{slug}")).unwrap_or_else(|| candidate.to_string());
        let mut modules = records::task_config_at(&self.hub, &tip, slug).map(|c| c.modules).unwrap_or_default();
        if modules.is_empty() {
            return self.runner.run_set_for(merge_sha, &[], None);
        }
        let base = self.hub.run(&["merge-base", main_tip, candidate]).map_err(|e| e.to_string())?;
        let e = records::exported_at(&self.hub, merge_sha, Some(self.runner.exporter()))?
            .ok_or_else(|| format!("no {} at {merge_sha}", config::ARCH_LEAN_MANIFEST))?;
        let main_diff: Vec<String> = self.hub.diff_names(&base, main_tip).into_iter().map(|(_, p)| p).collect();
        for m in &e.arch.modules {
            if !modules.contains(&m.name) && main_diff.iter().any(|p| path_under_test_paths(p, &m.paths)) {
                modules.push(m.name.clone());
            }
        }
        let task_diff = self.hub.diff_names(&base, candidate);
        if let Some(p) = main_diff
            .iter()
            .chain(task_diff.iter().map(|(_, p)| p))
            .find(|p| path_under_test_paths(p, &e.arch.shared))
        {
            return Ok(checks::RunSet::whole(e.suite.checks, &format!("the whole suite ({p} is shared)")));
        }
        let mut set = self.runner.run_set_for(merge_sha, &modules, None)?;
        set.why = format!("the run set of {}", modules.join(", "));
        Ok(set)
    }

    fn land(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        transition: Transition,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let hub_ref = format!("refs/heads/task/{slug}");
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or_else(|| "hub has no main".to_string())?;
        // Held on this very main already: merging again would only build a
        // new merge commit and run the whole suite on it for nothing.
        if self.held_accepts.get(slug) == Some(&(candidate.to_string(), main_tip.clone())) {
            return Ok(());
        }

        let mut sync: Option<String> = None;
        if !self.hub.is_ancestor(&main_tip, candidate) {
            let (mut tree, mut conflicts) = self
                .hub
                .merge_tree_conflicts(candidate, &main_tip)
                .map_err(|e| e.to_string())?;
            // Conflicts only in lock files the roadmap can regenerate are
            // the player's to resolve; the suite judges the result.
            if !conflicts.is_empty() {
                let regen = self.roadmap_at(&main_tip)?.player.lock_regen;
                if conflicts.iter().all(|c| regen.contains_key(c)) {
                    match self.runner.regen_locks(candidate, &main_tip, &regen) {
                        Ok(t) => {
                            tree = t;
                            conflicts.clear();
                        }
                        Err(e) => report.errors.push(format!(
                            "task/{slug}: lock_regen could not resolve {}: {e}",
                            conflicts.join(", ")
                        )),
                    }
                }
            }
            if !conflicts.is_empty() {
                self.hub
                    .update_ref_cas(&hub_ref, candidate, tip)
                    .map_err(|e| e.to_string())?;
                records::record_import(
                    &self.hub,
                    candidate,
                    Actor::Director,
                    transition,
                    Some(slug),
                )?;
                report
                    .imported
                    .push((hub_ref, candidate.to_string(), transition));
                records::record_conflict(&self.hub, candidate, &main_tip, &conflicts)?;
                report.conflicts.push((
                    slug.to_string(),
                    format!(
                        "merging main {main_tip} into task/{slug} conflicts in {}",
                        conflicts.join(", ")
                    ),
                ));
                return Ok(());
            }
            sync = Some(
                self.hub
                    .commit_tree(
                        &tree,
                        &[candidate, &main_tip],
                        &format!("Merge main into task/{slug}"),
                    )
                    .map_err(|e| e.to_string())?,
            );
        }
        let head = sync.clone().unwrap_or_else(|| candidate.to_string());

        // The `Task: <slug>` trailer must be the merge commit's last
        // paragraph, so `records::accepted_tasks`'s line scan finds it.
        let marker_body = self.accept_marker_body(candidate);
        let message = if marker_body.is_empty() {
            format!("Merge task/{slug}\n\nTask: {slug}")
        } else {
            format!("Merge task/{slug}\n\n{marker_body}\n\nTask: {slug}")
        };
        let tree = self
            .hub
            .rev_parse(&format!("{head}^{{tree}}"))
            .ok_or_else(|| format!("no tree at {head}"))?;
        let merge_sha = self
            .hub
            .commit_tree(&tree, &[&main_tip, &head], &message)
            .map_err(|e| e.to_string())?;

        let set = self.landing_set(slug, candidate, &main_tip, &merge_sha);
        let suite = match &set {
            Ok(set) => self.runner.run_set(&merge_sha, set),
            Err(e) => checks::SuiteResult::failing(e.clone()),
        };
        let why = set.map(|s| s.why).unwrap_or_default();
        self.note_ledgers(slug, &merge_sha, &suite);
        if !suite.pass {
            // Not the work's fault, nor the merge's: leave the marker
            // unimported (no strike, no sync_revision) until main is clean.
            if let Some(names) = self.held_by_main(&suite) {
                let key = (candidate.to_string(), main_tip.clone());
                if self.held_accepts.insert(slug.to_string(), key.clone()) != Some(key) {
                    report.held.push((
                        slug.to_string(),
                        format!(
                            "{} {} fails only on checks already bad on main (failing, impure or killed): {}",
                            transition.as_str(),
                            short(candidate),
                            names.join(", ")
                        ),
                    ));
                }
                return Ok(());
            }
            if sync.is_none() && transition == Transition::Accept {
                // Nothing was merged in: this is the code that passed at
                // finish, so the failure isn't the merge's.
                let msg = format!("accept: task/{slug}: {}", suite.summary());
                return self.reject(slug, candidate, Reason::Accept, msg, report);
            }
            self.hub
                .update_ref_cas(&hub_ref, &head, tip)
                .map_err(|e| e.to_string())?;
            match &sync {
                Some(sync) => {
                    records::record_import(
                        &self.hub,
                        candidate,
                        Actor::Director,
                        transition,
                        Some(slug),
                    )?;
                    records::record_import(
                        &self.hub,
                        sync,
                        Actor::Player,
                        Transition::SyncRevision,
                        Some(slug),
                    )?;
                    report
                        .imported
                        .push((hub_ref.clone(), candidate.to_string(), transition));
                    report
                        .imported
                        .push((hub_ref, sync.clone(), Transition::SyncRevision));
                }
                None => {
                    records::record_import(
                        &self.hub,
                        candidate,
                        Actor::Director,
                        Transition::SyncRevision,
                        Some(slug),
                    )?;
                    report.imported.push((
                        hub_ref,
                        candidate.to_string(),
                        Transition::SyncRevision,
                    ));
                }
            }
            self.pin(slug, &head, report)?;
            report.merge_failed.push((
                slug.to_string(),
                format!(
                    "{why} fails with main merged into task/{slug}; back to the worker\n{}",
                    suite.summary()
                ),
            ));
            let text = self.prompts()?.render(
                "worker.merge-failed",
                &[("slug", slug), ("summary", &suite.summary())],
            );
            self.relay(slug, &text);
            return Ok(());
        }

        self.hub
            .update_ref_cas(&hub_ref, &head, tip)
            .map_err(|e| e.to_string())?;
        self.hub
            .update_ref_cas("refs/heads/main", &merge_sha, &main_tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            Actor::Director,
            transition,
            Some(slug),
        )?;
        report
            .imported
            .push((hub_ref.clone(), candidate.to_string(), transition));
        if let Some(sync) = &sync {
            records::record_import(&self.hub, sync, Actor::Player, Transition::Sync, Some(slug))?;
            report
                .imported
                .push((hub_ref, sync.clone(), Transition::Sync));
        }
        records::record_import(
            &self.hub,
            &merge_sha,
            Actor::Player,
            Transition::MergeAccept,
            Some(slug),
        )?;
        report.imported.push((
            "refs/heads/main".to_string(),
            merge_sha,
            Transition::MergeAccept,
        ));

        // The task branch itself stays in the hub as a permanent record
        // (spec §9); only the worker's live session and its now-unneeded
        // worktree end.
        self.end_task_sessions(slug);
        Ok(())
    }

    /// The body of the `accept:` marker that `head` lands: `head` itself, or
    /// the nearest one before a resolution on its first-parent line.
    fn accept_marker_body(&self, head: &str) -> String {
        self.hub
            .rev_list(&["--first-parent", head])
            .into_iter()
            .find(|c| {
                self.hub
                    .commit_subject(c)
                    .is_some_and(|s| s.starts_with("accept:"))
            })
            .and_then(|c| self.hub.commit_body(&c))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    // ---- conflict resolution (spec §11) ----

    /// A director merge commit `candidate` with parents (`tip`, `theirs`) on
    /// `task/<slug>`: the resolution of the conflict the player recorded
    /// when it couldn't land an accept, and the only merge a director may
    /// make (any other stays a `history` rejection, spec §5). It must merge
    /// exactly the recorded `main` commit, and may differ from git's own
    /// merge of the two only on the recorded conflicted paths: it resolves
    /// that conflict and writes no other code. Then the player re-pins and
    /// lands it like the accept it completes.
    fn try_resolve(
        &mut self,
        slug: &str,
        tip: &str,
        theirs: &str,
        candidate: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let task = hub_state.tasks.iter().find(|t| t.slug == slug);
        let resolvable = self
            .fsm_ctx(&hub_state, slug)
            .map(|c| fsm::decide(Actor::Director, Move::Resolve, c));
        let conflict = task
            .and_then(|t| t.conflict.clone())
            .filter(|_| resolvable == Some(Verdict::Allow));
        let Some(conflict) = conflict else {
            let msg = format!(
                "history: task/{slug} is {} — the only director merge is the resolution of a conflict the player recorded (the player merges main itself)",
                task.map(|t| t.status.as_str()).unwrap_or("unknown")
            );
            return self.reject(slug, candidate, Reason::History, msg, report);
        };
        if theirs != conflict.main {
            let msg = format!(
                "resolve: task/{slug}'s conflict is with main {}: merge exactly that commit, not {}",
                conflict.main,
                short(theirs)
            );
            return self.reject(slug, candidate, Reason::Ownership, msg, report);
        }

        let (merged, _) = self
            .hub
            .merge_tree_conflicts(tip, theirs)
            .map_err(|e| e.to_string())?;
        let stray: Vec<String> = self
            .hub
            .diff_names(&merged, candidate)
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| !conflict.paths.contains(p))
            .collect();
        if !stray.is_empty() {
            let msg = format!(
                "resolve: task/{slug}: a resolution may only change the conflicted paths ({}), not {}",
                conflict.paths.join(", "),
                stray.join(", ")
            );
            return self.reject(slug, candidate, Reason::Ownership, msg, report);
        }

        self.pin(slug, candidate, report)?;
        self.land(slug, tip, candidate, Transition::Resolve, report)
    }

    // ---- spikes (spec §14) ----

    /// spec §14's post row for a spike: the spike is `ready`; the post commit only adds the director's own files
    /// (`prompt.md`, `questions/1.toml`, optional `baseline/1.toml` and its
    /// scripts, anything else under `tasks/<slug>/` but `claims/` and
    /// `findings/`); the question is valid; and the director's baseline claims
    /// pass on the current code. There is no task check and no suite
    /// entry: a spike never merges.
    fn validate_spike_post(
        &self,
        slug: &str,
        tip: &str,
        candidate: &str,
        status: Status,
        actor: Actor,
        hub_state: &state::HubState,
    ) -> Result<(), String> {
        let may_post = self
            .fsm_ctx(hub_state, slug)
            .is_some_and(|c| fsm::decide(actor, Move::Post, c) == Verdict::Allow);
        if !may_post {
            return Err(format!(
                "post: task/{slug} is {} — a spike can only be posted while ready",
                status.as_str()
            ));
        }
        // Only the director's own files, under tasks/<slug>/: nothing outside
        // it (the base must stay `main`'s code), and nothing in the
        // worker's `claims/` or `findings/`.
        let prefix = format!("tasks/{slug}/");
        for (_, path) in self.hub.diff_names(tip, candidate) {
            let ok = path
                .strip_prefix(&prefix)
                .is_some_and(|rest| !rest.starts_with("claims/") && !rest.starts_with("findings/"));
            if !ok {
                return Err(format!(
                    "post: task/{slug}: a spike's post may only add the director's files under {prefix} (prompt, questions/, baseline/), not {path:?}"
                ));
            }
        }

        let rounds = records::question_rounds(&self.hub, candidate, slug);
        if rounds != vec![1] {
            return Err(format!(
                "post: task/{slug}: a spike's post must add exactly {} (found rounds {rounds:?})",
                spike::question_path(slug, 1)
            ));
        }
        let question = spike::read_question(&self.hub, candidate, slug, 1)
            .map_err(|e| format!("post: task/{slug}: {e}"))?;
        if question.refines.is_some() {
            return Err(format!(
                "post: task/{slug}: the first question can't refine an earlier round"
            ));
        }

        let baseline = spike::read_baseline(&self.hub, candidate, slug, 1)
            .map_err(|e| format!("post: task/{slug}: {e}"))?;
        for (i, c) in baseline.iter().enumerate() {
            if baseline[..i].iter().any(|o| o.name == c.name) {
                return Err(format!(
                    "post: task/{slug}: baseline claim name {:?} is used twice",
                    c.name
                ));
            }
        }
        let failures = spike::failing_at(&self.runner, candidate, &baseline, "on the current code");
        if !failures.is_empty() {
            return Err(format!(
                "post: task/{slug}: baseline claims must pass on the current code\n{}",
                failures.join("\n")
            ));
        }
        Ok(())
    }

    /// A spike's finish claim (spec §14): a path check, then
    /// `spike::evaluate_finish` runs and reads everything the player will
    /// vouch for. Failures are `claims` strikes. A valid claim is imported,
    /// and the player's verified record of the round goes into the
    /// exploration note on that commit.
    fn finish_spike(
        &mut self,
        slug: &str,
        hub_tip: &str,
        candidate: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let round = records::spike_round(&self.hub, hub_tip, slug);
        let bad =
            spike::worker_path_violations(&self.hub.diff_names(hub_tip, candidate), slug, round);
        if !bad.is_empty() {
            let msg = format!(
                "pins: task/{slug}'s finish claim touches files under tasks/{slug}/ that aren't the worker's to write: {}",
                bad.join(", ")
            );
            return self.reject_and_relay(slug, candidate, Reason::Pins, msg, report);
        }

        let Some(base) = records::base_commit(&self.hub, slug) else {
            return Err(format!(
                "task/{slug} is a spike with no recorded base commit"
            ));
        };
        match spike::evaluate_finish(&self.hub, &self.runner, slug, &base, candidate) {
            Err(msg) => {
                let msg = format!("claims: task/{slug}: {msg}");
                self.reject_and_relay(slug, candidate, Reason::Claims, msg, report)
            }
            Ok(ev) => {
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub
                    .update_ref_cas(&hub_ref, candidate, hub_tip)
                    .map_err(|e| e.to_string())?;
                records::record_import(
                    &self.hub,
                    candidate,
                    Actor::Worker,
                    Transition::Finish,
                    Some(slug),
                )?;
                records::record_exploration(
                    &self.hub,
                    candidate,
                    records::ExplorationNote {
                        slug: slug.to_string(),
                        round: ev.round,
                        question: ev.question.question.clone(),
                        answer: ev.answer.clone(),
                        claims: ev.claims,
                        concluded: false,
                        at: 0,
                    },
                )?;
                report
                    .imported
                    .push((hub_ref, candidate.to_string(), Transition::Finish));
                let text = self
                    .prompts()?
                    .render("spike.finish-accepted", &[("answer", &ev.answer)]);
                self.relay(slug, &text);
                Ok(())
            }
        }
    }

    /// A send-back on a tested spike: it adds the next round's question (and
    /// optionally its baseline claims) and nothing else, within
    /// `max_review_rounds`; the new baseline claims must pass on the current
    /// code. It needs no failing test — the new question is the target. The
    /// operator's approval is a process rule the player can't see.
    fn try_spike_send_back(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        diffs: &[(char, String)],
        hub_state: &state::HubState,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let n = records::spike_round(&self.hub, tip, slug);
        let next = n + 1;
        let question_file = spike::question_path(slug, next);
        let q_prefix = format!("tasks/{slug}/questions/");

        if !diffs
            .iter()
            .any(|(st, p)| *st == 'A' && *p == question_file)
        {
            if !diffs
                .iter()
                .any(|(st, p)| *st == 'A' && p.starts_with(&q_prefix))
            {
                let msg = format!(
                    "task/{slug}'s commit doesn't add a question file (expected {question_file})"
                );
                return self.reject(slug, candidate, Reason::Ownership, msg, report);
            }
            let msg =
                format!("send-back: task/{slug}: expected round {next} to add {question_file}");
            return self.reject(slug, candidate, Reason::Review, msg, report);
        }
        for (st, path) in diffs {
            if *st != 'A' || !spike::send_back_path_ok(path, slug, next) {
                let msg = format!(
                    "send-back: task/{slug}: a spike send-back may only add {question_file} and baseline claims for round {next}, not {path:?}"
                );
                return self.reject(slug, candidate, Reason::Review, msg, report);
            }
        }

        let question = match spike::read_question(&self.hub, candidate, slug, next) {
            Ok(q) => q,
            Err(e) => {
                return self.reject(
                    slug,
                    candidate,
                    Reason::Review,
                    format!("send-back: task/{slug}: {e}"),
                    report,
                );
            }
        };
        if let Some(r) = question.refines {
            if r < 1 || r > n {
                let msg = format!(
                    "send-back: task/{slug}: {question_file}: refines = {r}, but only rounds 1..={n} exist"
                );
                return self.reject(slug, candidate, Reason::Review, msg, report);
            }
        }
        let baseline = match spike::read_baseline(&self.hub, candidate, slug, next) {
            Ok(b) => b,
            Err(e) => {
                return self.reject(
                    slug,
                    candidate,
                    Reason::Review,
                    format!("send-back: task/{slug}: {e}"),
                    report,
                );
            }
        };
        if let Some(c) = baseline
            .iter()
            .enumerate()
            .find_map(|(i, c)| baseline[..i].iter().any(|o| o.name == c.name).then_some(c))
        {
            let msg = format!(
                "send-back: task/{slug}: baseline claim name {:?} is used twice",
                c.name
            );
            return self.reject(slug, candidate, Reason::Review, msg, report);
        }

        // The Nth question is the (N-1)th send-back's; spec §10's cap counts
        // send-backs, so round `n` here is send-back number `n`.
        if n > hub_state.roadmap.limits.max_review_rounds {
            let cause = format!(
                "review rounds exceeded: {n} > {}",
                hub_state.roadmap.limits.max_review_rounds
            );
            records::record_escalation(&self.hub, slug, candidate, &cause)?;
            report.escalated.push((slug.to_string(), cause));
            return Ok(());
        }

        if !baseline.is_empty() {
            let Some(base) = records::base_commit(&self.hub, slug) else {
                return Err(format!(
                    "task/{slug} is a spike with no recorded base commit"
                ));
            };
            let overlay = spike::overlay_commit(&self.hub, &base, candidate, slug)?;
            let failures =
                spike::failing_at(&self.runner, &overlay, &baseline, "on the current code");
            if !failures.is_empty() {
                let msg = format!(
                    "send-back: task/{slug}: baseline claims must pass on the current code\n{}",
                    failures.join("\n")
                );
                return self.reject(slug, candidate, Reason::Review, msg, report);
            }
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .update_ref_cas(&hub_ref, candidate, tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            Actor::Director,
            Transition::SendBack,
            Some(slug),
        )?;
        report
            .imported
            .push((hub_ref, candidate.to_string(), Transition::SendBack));

        records::set_pins(&self.hub, slug, candidate)?;
        let raw = self
            .hub
            .show_file(candidate, &question_file)
            .unwrap_or_default();
        let prompts = self.prompts()?;
        let round = next.to_string();
        let header = prompts.render(
            "spike.next-round",
            &[
                ("round", &round),
                ("slug", slug),
                ("findings", &spike::findings_path(slug, next)),
                ("claims", &spike::claims_path(slug, next)),
            ],
        );
        let question = prompts.render(
            "spike.question",
            &[("round", &round), ("question", raw.trim_end())],
        );
        self.sessions
            .send(slug, &Prompts::with_block(&header, &question));
        Ok(())
    }

    /// `conclude:` on a tested spike: the branch moves to
    /// `refs/explored/<slug>` (it never merges), the player records the final
    /// answer, and the session ends. The answer is the one the player
    /// verified at the finish; the director and operator agreeing with it is
    /// their conversation.
    fn try_conclude(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        status: Option<Status>,
        report: &mut TickReport,
    ) -> Result<(), String> {
        match self.director_verdict(slug, Move::Conclude)? {
            Some(Verdict::Allow) => {}
            Some(Verdict::Reject(reason)) => {
                let msg = if reason == Reason::Ownership {
                    format!(
                        "conclude: task/{slug} is not a spike — an ordinary task is finished with `accept:`"
                    )
                } else {
                    format!(
                        "conclude: task/{slug} is {} — a spike needs a validated finish before it can be concluded",
                        status.map(|s| s.as_str()).unwrap_or("unknown")
                    )
                };
                return self.reject(slug, candidate, reason, msg, report);
            }
            Some(Verdict::Ignore | Verdict::Hold) => return Ok(()),
            None => {
                let msg = format!(
                    "conclude: task/{slug} is unknown — a spike needs a validated finish before it can be concluded"
                );
                return self.reject(slug, candidate, Reason::Accept, msg, report);
            }
        }
        let Some(note) = records::exploration(&self.hub, tip) else {
            let msg = format!("conclude: task/{slug} has no verified answer recorded at its tip");
            return self.reject(slug, candidate, Reason::Accept, msg, report);
        };

        self.hub
            .update_ref(&format!("refs/explored/{slug}"), candidate)
            .map_err(|e| e.to_string())?;
        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .delete_ref_cas(&hub_ref, tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            Actor::Director,
            Transition::Conclude,
            Some(slug),
        )?;
        records::record_exploration(
            &self.hub,
            candidate,
            records::ExplorationNote {
                concluded: true,
                ..note
            },
        )?;
        report.imported.push((
            format!("refs/explored/{slug}"),
            candidate.to_string(),
            Transition::Conclude,
        ));

        self.end_task_sessions(slug);
        Ok(())
    }

    // ---- retire (spec §9) ----

    /// spec §9's retire row: allowed only when the task isn't posted or in
    /// flight (pending/ready/orphaned), or is escalated. Moves the branch to
    /// `refs/retired/<slug>` and ends the worker session.
    fn try_retire(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        status: Option<Status>,
        report: &mut TickReport,
    ) -> Result<(), String> {
        // A spike the operator gives up on may also be retired once its
        // finish is validated (spec §14); an ordinary tested task may not.
        let allowed = match self.director_verdict(slug, Move::Retire)? {
            Some(Verdict::Allow) => true,
            Some(Verdict::Ignore | Verdict::Hold) => return Ok(()),
            Some(Verdict::Reject(_)) | None => false,
        };
        if !allowed {
            let msg = format!(
                "retire: task/{slug} is {} — retire is only allowed while pending/ready/orphaned (not posted or in flight), conflicted, contested or escalated",
                status.map(|s| s.as_str()).unwrap_or("unknown")
            );
            return self.reject(slug, candidate, Reason::Retire, msg, report);
        }

        self.hub
            .update_ref(&format!("refs/retired/{slug}"), candidate)
            .map_err(|e| e.to_string())?;
        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .delete_ref_cas(&hub_ref, tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            Actor::Director,
            Transition::Retire,
            Some(slug),
        )?;
        report.imported.push((
            format!("refs/retired/{slug}"),
            candidate.to_string(),
            Transition::Retire,
        ));

        self.end_task_sessions(slug);
        Ok(())
    }

    fn try_replan(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        match self.validate_replan(slug, tip, candidate) {
            Ok(()) => {
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub
                    .update_ref_cas(&hub_ref, candidate, tip)
                    .map_err(|e| e.to_string())?;
                records::record_import(
                    &self.hub,
                    candidate,
                    Actor::Director,
                    Transition::Replan,
                    Some(slug),
                )?;
                report
                    .imported
                    .push((hub_ref, candidate.to_string(), Transition::Replan));
            }
            Err(msg) => self.reject(slug, candidate, Reason::Plan, msg, report)?,
        }
        Ok(())
    }

    /// Same validation as `validate_plan` minus "slug unused", plus: the
    /// task can't depend on itself (a self-dependency would otherwise slip
    /// past the "dependency exists" check, since the branch already exists).
    fn validate_replan(&self, slug: &str, base: &str, candidate: &str) -> Result<(), String> {
        let expected_path = format!("tasks/{slug}/task.toml");
        let diffs = self.hub.diff_names(base, candidate);
        if diffs.len() != 1 || diffs[0].1 != expected_path {
            return Err(format!(
                "plan: task/{slug}'s replan commits may only change {expected_path}"
            ));
        }
        let text = self
            .hub
            .show_file(candidate, &expected_path)
            .ok_or_else(|| format!("plan: {expected_path} missing at {candidate}"))?;
        let cfg = config::task_config_from_str(&text)
            .map_err(|e| format!("plan: {expected_path}: {e}"))?;
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or_else(|| "hub has no main".to_string())?;
        self.validate_task_config(slug, &main_tip, &cfg)
    }

    /// Shared by plan and replan: phase, dependency existence/retirement/
    /// self-reference, and the no-cycles rule over the hub's task graph plus
    /// this candidate.
    fn validate_task_config(
        &self,
        slug: &str,
        main_tip: &str,
        cfg: &TaskConfig,
    ) -> Result<(), String> {
        let roadmap = self.roadmap_at(main_tip)?;
        if !cfg.phase.is_empty() && !roadmap.phases.iter().any(|p| p.id == cfg.phase) {
            return Err(format!("plan: task/{slug}: unknown phase {:?}", cfg.phase));
        }

        let accepted = records::accepted_tasks(&self.hub, main_tip);
        let retired = records::retired_slugs(&self.hub);
        for dep in &cfg.depends_on {
            if dep == slug {
                return Err(format!("plan: task/{slug} cannot depend on itself"));
            }
            if retired.iter().any(|r| r == dep) {
                return Err(format!("plan: task/{slug} depends on retired task {dep:?}"));
            }
            if records::is_explored(&self.hub, dep)
                || self
                    .task_config_at(&accepted, dep)
                    .is_some_and(|c| c.kind == TaskKind::Spike)
            {
                return Err(format!(
                    "plan: task/{slug} depends on spike {dep:?}: nothing may depend on a spike"
                ));
            }
            let exists = self.hub.ref_exists(&format!("refs/heads/task/{dep}"))
                || accepted.contains_key(dep);
            if !exists {
                return Err(format!("plan: task/{slug} depends on unknown task {dep:?}"));
            }
        }

        if self.creates_cycle(&accepted, slug, &cfg.depends_on) {
            return Err(format!("plan: task/{slug}'s dependencies contain a cycle"));
        }

        if !cfg.modules.is_empty() {
            let exporter = self.runner.exporter();
            let arch = records::exported_at(&self.hub, main_tip, Some(exporter))
                .map_err(|e| format!("plan: task/{slug}: {e}"))?
                .ok_or_else(|| format!("plan: task/{slug}: main has no {}", config::ARCH_LEAN_MANIFEST))?
                .arch;
            for m in &cfg.modules {
                if arch.node(m).is_none() {
                    return Err(format!("plan: task/{slug}: modules names unknown {m:?}"));
                }
            }
            records::impacted_at(&self.hub, main_tip, &cfg.modules, Some(exporter))
                .map_err(|e| format!("plan: task/{slug}: {e}"))?;
        }
        Ok(())
    }

    /// The dependency graph reachable from `deps` (as if they belonged to
    /// `slug`), read from each dependency's live branch tip or (once
    /// accepted) `main`. True if that graph reaches `slug` again.
    fn creates_cycle(
        &self,
        accepted: &BTreeMap<String, String>,
        slug: &str,
        deps: &[String],
    ) -> bool {
        let mut stack: Vec<String> = deps.to_vec();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(d) = stack.pop() {
            if d == slug {
                return true;
            }
            if !seen.insert(d.clone()) {
                continue;
            }
            if let Some(cfg) = self.task_config_at(accepted, &d) {
                stack.extend(cfg.depends_on);
            }
        }
        false
    }

    fn task_config_at(
        &self,
        accepted: &BTreeMap<String, String>,
        slug: &str,
    ) -> Option<TaskConfig> {
        let tip = self
            .hub
            .rev_parse(&format!("refs/heads/task/{slug}"))
            .or_else(|| accepted.get(slug).cloned())?;
        let text = self
            .hub
            .show_file(&tip, &format!("tasks/{slug}/task.toml"))?;
        config::task_config_from_str(&text).ok()
    }

    /// A failing suite is a `budget` strike when a check was killed for
    /// running too long, else a `suite` one.
    fn suite_failure_reason(suite: &checks::SuiteResult) -> Reason {
        if suite.checks.iter().any(|c| c.timed_out) {
            Reason::Budget
        } else {
            Reason::Suite
        }
    }

    fn roadmap_at(&self, commit: &str) -> Result<RoadmapConfig, String> {
        let text = self
            .hub
            .show_file(commit, "roadmap.toml")
            .ok_or_else(|| format!("roadmap.toml not found at {commit}"))?;
        config::roadmap_config_from_str(&text)
    }

    // ---- launch (spec §8.3, §9's launch row) ----

    /// Every `Posted` task that has never been launched (no
    /// `refs/dashboard/pins/<slug>` yet — pins are only ever taken at launch
    /// or send-back, and a `Posted` task hasn't been sent back) gets a
    /// worker clone and a tmux session. A launch failure never retries
    /// itself: whatever went wrong, this method always records *some* trace
    /// of the attempt (pins, at least) before returning, so the task isn't
    /// silently re-attempted every tick forever (spec's deferred "not
    /// relaunched automatically").
    fn launch_workers(&mut self, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let candidates: Vec<(String, String)> = hub_state
            .tasks
            .iter()
            .filter(|t| t.status == Status::Posted && !records::has_pins(&self.hub, &t.slug))
            .map(|t| (t.slug.clone(), t.tip.clone()))
            .collect();
        for (slug, tip) in candidates {
            self.try_launch(&slug, &tip, report)?;
        }
        Ok(())
    }

    /// Reports a launch the player won't make yet. The task stays `posted`
    /// and is looked at again every tick (each a cache hit), so the same
    /// reason at the same tip is reported once, not every tick.
    fn hold_launch(&mut self, slug: &str, tip: &str, msg: String, report: &mut TickReport) {
        let key = format!("{tip}\n{msg}");
        if self.launch_told.get(slug) != Some(&key) {
            self.launch_told.insert(slug.to_string(), key);
            report.errors.push(msg);
        }
    }

    fn try_launch(&mut self, slug: &str, tip: &str, report: &mut TickReport) -> Result<(), String> {
        let is_spike = records::is_spike_at(&self.hub, tip, slug);
        if is_spike {
            // A spike has no task check; what must still hold at launch is
            // the director's baseline claims on the current code.
            let round = records::spike_round(&self.hub, tip, slug);
            let baseline = spike::read_baseline(&self.hub, tip, slug, round)
                .map_err(|e| format!("launch: task/{slug}: {e}"))?;
            let failures = spike::failing_at(&self.runner, tip, &baseline, "on the current code");
            if !failures.is_empty() {
                let msg = format!(
                    "launch: task/{slug}: the director's baseline claims no longer pass at the tip — not launching\n{}",
                    failures.join("\n")
                );
                self.hold_launch(slug, tip, msg, report);
                return Ok(());
            }
        } else if records::is_test_speedup_at(&self.hub, tip, slug) {
            // No task check: it's launched while its evidence still holds.
            if let Some(miss) = self.evidence_miss(slug, tip) {
                return self.auto_retire(slug, tip, &miss, report);
            }
        } else {
            let result = self
                .runner
                .run_task_check(tip, slug)
                .map_err(|e| format!("launch: task/{slug}: {e}"))?;
            if let Some(why) = result.impurity() {
                let _ = records::record_impurity(&self.hub, slug, tip, &result);
                let msg = format!("launch: task/{slug}: {why} — not launching");
                self.hold_launch(slug, tip, msg, report);
                return Ok(());
            }
            if result.pass {
                let msg = format!(
                    "launch: task/{slug}: the task check now passes at the tip — not launching a worker for work that's already done"
                );
                self.hold_launch(slug, tip, msg, report);
                return Ok(());
            }
            if let Some(miss) = self.evidence_miss(slug, tip) {
                return self.auto_retire(slug, tip, &miss, report);
            }
        }

        // Pins are taken before anything else can fail, so a launch that
        // goes wrong from here on is never silently retried next tick.
        self.pin(slug, tip, report)?;
        if is_spike {
            records::set_base(&self.hub, slug, tip)?;
        }

        let clone_dir = paths::worker_clone(&self.director, slug);
        if clone_dir.exists() {
            report.errors.push(format!("launch: task/{slug}: {} already exists — refusing to touch a leftover agent clone (spec §5)", clone_dir.display()));
            return Ok(());
        }
        if let Err(e) = self.create_worker_clone(slug, tip, &clone_dir) {
            report.errors.push(format!("launch: task/{slug}: {e}"));
            return Ok(());
        }

        let prompt_file = match Self::git_dir(&clone_dir) {
            Ok(dir) => dir.join("dashboard-prompt.md"),
            Err(e) => {
                report.errors.push(format!("launch: task/{slug}: {e}"));
                return Ok(());
            }
        };
        if let Err(e) = self.write_prompt_file(slug, tip, &prompt_file) {
            report.errors.push(format!("launch: task/{slug}: {e}"));
            return Ok(());
        }

        match self.agent_cmd_at_main() {
            Ok(agent_cmd) => {
                match self.launch_session(slug, &clone_dir, &agent_cmd, &prompt_file) {
                    Ok(()) => report.launched.push(slug.to_string()),
                    Err(e) => report.errors.push(format!("launch: task/{slug}: {e}")),
                }
            }
            Err(e) => report.errors.push(format!("launch: task/{slug}: {e}")),
        }
        Ok(())
    }

    /// prompts.kdl at main's tip (spec §19), validated on its way onto main.
    fn prompts(&self) -> Result<Prompts, String> {
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or_else(|| "hub has no main".to_string())?;
        let text = self
            .hub
            .show_file(&main_tip, prompts::FILE)
            .ok_or_else(|| format!("{} missing on main: commit one (spec §19)", prompts::FILE))?;
        Prompts::parse(&text)
    }

    fn agent_cmd_at_main(&self) -> Result<String, String> {
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or_else(|| "hub has no main".to_string())?;
        Ok(self.roadmap_at(&main_tip)?.player.agent_cmd)
    }

    /// Ensures the project's one, permanent worker workspace exists (a
    /// `git clone` of the hub — `paths::worker_workspace` — created once
    /// and trusted once by the operator), brings it up to date with the hub,
    /// then `git worktree add -B task/<slug> <clone_dir> origin/task/<slug>`:
    /// a fresh worktree on the task branch that shares the workspace's git
    /// identity — and so its trust — instead of being a fresh repository of
    /// its own. Never touched again by the player except by fetching from
    /// it. Safe to share one repository across every task's worktree only
    /// because slugs are never reused (spec §8.1): no two worktrees ever
    /// contend for the same branch name. Parallel tasks (spec §4) each get their own worktree and
    /// branch; the player is the only process touching the workspace, one
    /// git command at a time.
    /// An ordinary task's worktree is sparse from the start: it holds the
    /// task's files only, the patterns of `records::workspace_at` at `tip`
    /// (spec §21).
    fn create_worker_clone(&self, slug: &str, tip: &str, clone_dir: &Path) -> Result<(), String> {
        let patterns = match self.worker_tree_kind(slug, tip) {
            TreeKind::Sparse => Some(self.worker_patterns(slug, tip)?),
            TreeKind::Full => None,
        };
        self.add_worktree(&format!("task/{slug}"), slug, clone_dir, patterns.as_deref())
    }

    /// Whether `slug`'s worker gets a sparse tree (spec §21): a spike, a
    /// flaky-test-fix and a test-speedup keep a full one.
    fn worker_tree_kind(&self, slug: &str, tip: &str) -> TreeKind {
        match records::task_config_at(&self.hub, tip, slug).map(|c| c.kind) {
            Some(TaskKind::Spike | TaskKind::FlakyTestFix | TaskKind::TestSpeedup) => TreeKind::Full,
            _ => TreeKind::Sparse,
        }
    }

    /// The sparse patterns of `slug`'s worker tree at `tip`.
    fn worker_patterns(&self, slug: &str, tip: &str) -> Result<Vec<String>, String> {
        Ok(records::workspace_at(&self.hub, slug, tip, Some(self.runner.exporter()))?.patterns())
    }

    /// Records `slug`'s pins at `commit` (spec §8.3) and, for a sparse
    /// worker tree that exists, recomputes its patterns from the same
    /// commit (spec §21). The pins stand either way: a failure to rewrite
    /// the patterns is reported, never a rejection.
    fn pin(&self, slug: &str, commit: &str, report: &mut TickReport) -> Result<(), String> {
        records::set_pins(&self.hub, slug, commit)?;
        let clone_dir = paths::worker_clone(&self.director, slug);
        if !clone_dir.exists() || self.worker_tree_kind(slug, commit) == TreeKind::Full {
            return Ok(());
        }
        let set = self
            .worker_patterns(slug, commit)
            .and_then(|p| Git::new(&clone_dir).sparse_set(&p).map_err(|e| e.to_string()));
        if let Err(e) = set {
            report.errors.push(format!("task/{slug}: rewriting the worker's sparse patterns: {e}"));
        }
        Ok(())
    }

    /// `create_worker_clone`'s mechanics for any agent: a worktree of the
    /// shared workspace at `clone_dir`, on the local branch `branch`, started
    /// at the hub's `task/<slug>`; with `sparse`, a sparse checkout of those
    /// patterns from the start (non-cone, per worktree).
    fn add_worktree(&self, branch: &str, slug: &str, clone_dir: &Path, sparse: Option<&[String]>) -> Result<(), String> {
        let workspace_dir = paths::worker_workspace(&self.director);
        if !workspace_dir.join(".git").exists() {
            if let Some(parent) = workspace_dir.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let hub_str = path_str(&self.hub.dir)?;
            let workspace_str = path_str(&workspace_dir)?;
            let cwd_git = Git::new(workspace_dir.parent().unwrap_or_else(|| Path::new(".")));
            cwd_git
                .run(&["clone", "--quiet", "--no-checkout", hub_str, workspace_str])
                .map_err(|e| format!("git clone (worker workspace): {e}"))?;
        }

        let workspace = Git::new(&workspace_dir);
        // So `dashboard test` finds the home from any worktree of it.
        if let Some(home) = self.home.root.to_str() {
            workspace
                .config_set("dashboard.home", home)
                .map_err(|e| format!("git config dashboard.home (worker workspace): {e}"))?;
        }
        workspace
            .run(&["fetch", "--quiet", "origin"])
            .map_err(|e| format!("fetching hub into the worker workspace: {e}"))?;

        if let Some(parent) = clone_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let clone_str = path_str(clone_dir)?;
        let start = format!("origin/task/{slug}");
        let mut args = vec!["worktree", "add", "--quiet"];
        if sparse.is_some() {
            args.push("--no-checkout");
        }
        args.extend(["-B", branch, clone_str, &start]);
        workspace.run(&args).map_err(|e| format!("git worktree add: {e}"))?;
        if let Some(patterns) = sparse {
            let wt = Git::new(clone_dir);
            wt.sparse_set(patterns).map_err(|e| format!("git sparse-checkout set: {e}"))?;
            wt.run(&["read-tree", "-mu", "HEAD"]).map_err(|e| format!("git read-tree: {e}"))?;
        }
        Ok(())
    }

    /// The real, private git-admin directory for `clone_dir`. For a plain
    /// clone this is just `<clone_dir>/.git`, but a worktree's `.git` is a
    /// *file* (pointing at `<workspace>/.git/worktrees/<name>`), so it can't
    /// be joined into further like a directory — this asks git for the real
    /// path instead of assuming the plain-clone shape.
    fn git_dir(clone_dir: &Path) -> Result<PathBuf, String> {
        let out = Git::new(clone_dir)
            .run(&["rev-parse", "--git-dir"])
            .map_err(|e| e.to_string())?;
        let dir = PathBuf::from(out);
        Ok(if dir.is_absolute() {
            dir
        } else {
            clone_dir.join(dir)
        })
    }

    /// Removes `slug`'s worker worktree and its local branch in the
    /// shared workspace repo (`paths::worker_workspace`), once its task
    /// is finished (accepted or retired). The hub's own `refs/heads/
    /// task/<slug>` (or `refs/retired/<slug>`) stays forever as the
    /// permanent record (spec §8.1) — the worktree was only ever scratch
    /// space for the worker to work in, so nothing is lost by reclaiming
    /// it. Unlike a plain clone (self-contained, harmless to leave forever),
    /// a worktree's administrative footprint lives *inside* the shared
    /// workspace repo, so leaving every finished task's worktree and branch
    /// registered there would grow that one repo's bookkeeping without
    /// bound over a long-lived project.
    ///
    /// Best-effort and silent on the common no-op cases (the task was never
    /// launched, or the workspace itself doesn't exist yet); a real removal
    /// failure is only a warning; the task's own transition already
    /// succeeded by the time this runs, so it's too late for this to be a
    /// rejection.
    fn cleanup_worker_clone(&self, slug: &str) {
        self.remove_worktree(
            &paths::worker_clone(&self.director, slug),
            &format!("task/{slug}"),
        );
    }

    /// Ends every session a finished task (accepted, retired, concluded) still
    /// has, its worker's and its task agent's (spec §17), and removes both
    /// worktrees.
    fn end_task_sessions(&mut self, slug: &str) {
        self.sessions.kill(slug);
        self.cleanup_worker_clone(slug);
        if records::agent_launched(&self.hub, slug) {
            self.sessions.kill(&state::agent_key(slug));
            self.remove_worktree(
                &paths::agent_clone(&self.director, slug),
                &format!("agent/{slug}"),
            );
        }
    }

    /// `cleanup_worker_clone`'s mechanics for any agent's worktree and its
    /// local branch.
    fn remove_worktree(&self, clone_dir: &Path, branch: &str) {
        if !clone_dir.exists() {
            return;
        }
        let workspace_dir = paths::worker_workspace(&self.director);
        if !workspace_dir.join(".git").exists() {
            return;
        }
        let workspace = Git::new(&workspace_dir);
        let Ok(clone_str) = path_str(clone_dir) else {
            eprintln!(
                "dashboard: worktree {} is not valid UTF-8, leaving it in place",
                clone_dir.display()
            );
            return;
        };
        if let Err(e) = workspace.run(&["worktree", "remove", "--force", clone_str]) {
            eprintln!("dashboard: removing worktree {}: {e}", clone_dir.display());
            return;
        }
        // Now safe to drop: the worktree that checked it out is gone, and
        // the hub's own ref is the permanent record, not this one.
        let _ = workspace.run(&["branch", "-D", branch]);
    }

    /// Writes the player's preamble plus `tasks/<slug>/prompt.md` at `tip`
    /// into `prompt_file`, which lives under the clone's private git-admin
    /// directory (`git_dir`) so it's never part of the worktree and can't be
    /// committed by the worker.
    fn write_prompt_file(&self, slug: &str, tip: &str, prompt_file: &Path) -> Result<(), String> {
        let prompt_body = self
            .hub
            .show_file(tip, &format!("tasks/{slug}/prompt.md"))
            .ok_or_else(|| format!("tasks/{slug}/prompt.md missing at {tip}"))?;
        let pinned = records::pinned_files(&self.hub, slug).unwrap_or_default();
        let pinned = pinned_block(pinned.keys().map(|s| s.as_str()));
        let prompts = self.prompts()?;
        let vars = [
            ("slug", slug),
            ("project", self.project.as_str()),
            ("pinned", pinned.as_str()),
        ];
        let content = if records::is_spike_at(&self.hub, tip, slug) {
            let round = records::spike_round(&self.hub, tip, slug);
            let question = self
                .hub
                .show_file(tip, &spike::question_path(slug, round))
                .unwrap_or_default();
            let question = prompts.render(
                "spike.question",
                &[
                    ("round", &round.to_string()),
                    ("question", question.trim_end()),
                ],
            );
            prompts.spike_launch(
                &prompts.render("spike.launch", &vars),
                &prompt_body,
                &question,
            )
        } else {
            let kind = records::task_config_at(&self.hub, tip, slug).and_then(|cfg| match cfg
                .kind
            {
                TaskKind::Task => Some(prompts.render("worker.workspace", &[("slug", slug)])),
                TaskKind::FlakyTestFix => {
                    let (fix_paths, replacements) =
                        flaky::prompt_lists(&cfg.fix_paths, &cfg.replace);
                    Some(prompts.render(
                        "worker.flaky",
                        &[("slug", slug), ("fix_paths", &fix_paths), ("replacements", &replacements)],
                    ))
                }
                TaskKind::TestSpeedup => {
                    let slow = cfg
                        .slow
                        .iter()
                        .map(|s| format!("  - {}", s.check))
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some(prompts.render("worker.test-speedup", &[("slow", &slow)]))
                }
                _ => None,
            });
            let mut preamble = prompts.render("worker.launch", &vars[..2]);
            if self.queued() {
                preamble = prompts::Prompts::with_block(&preamble, &prompts.render("worker.queued", &[]));
            }
            prompts.worker_launch(
                &preamble,
                kind.as_deref(),
                &prompt_body,
            )
        };
        std::fs::write(prompt_file, content).map_err(|e| format!("{}: {e}", prompt_file.display()))
    }

    /// Relaunches `slug`'s worker session with the same clone and prompt
    /// file it already has (spec's deferred "not relaunched automatically" —
    /// this is the manual trigger, exposed as `dashboard relaunch <slug>`).
    /// Only valid once pins exist (the task was actually launched before),
    /// the session isn't already alive, and the task is still in a state the
    /// worker may work on.
    pub fn relaunch(&mut self, slug: &str) -> Result<(), String> {
        if !records::has_pins(&self.hub, slug) {
            return Err(format!(
                "relaunch: task/{slug} has no pins — it was never launched"
            ));
        }
        if self.sessions.alive(slug) {
            return Err(format!("relaunch: task/{slug}'s session is already alive"));
        }
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let task = hub_state.tasks.iter().find(|t| t.slug == slug);
        let status = task.map(|t| t.status);
        if !matches!(
            status,
            Some(Status::Posted | Status::InProgress | Status::InRevision | Status::Contested)
        ) {
            return Err(format!(
                "relaunch: task/{slug} is {} — relaunch only applies while posted, in progress, or in revision",
                status.map(|s| s.as_str()).unwrap_or("unknown")
            ));
        }

        let clone_dir = paths::worker_clone(&self.director, slug);
        if !clone_dir.exists() {
            return Err(format!(
                "relaunch: task/{slug}'s clone {} is missing",
                clone_dir.display()
            ));
        }
        let prompt_file = Self::git_dir(&clone_dir)?.join("dashboard-prompt.md");
        if !prompt_file.exists() {
            return Err(format!(
                "relaunch: task/{slug}'s prompt file {} is missing",
                prompt_file.display()
            ));
        }

        let task = task.expect("a status implies a task");
        self.check_relaunch_limit(task, false, hub_state.roadmap.limits.max_relaunches)?;
        let agent_cmd = self.agent_cmd_at_main()?;
        self.launch_session(slug, &clone_dir, &agent_cmd, &prompt_file)?;
        records::record_relaunch(&self.hub, &task.tip, false)
    }

    /// `[limits] max_relaunches`: `task`'s worker (or, with `agent`, its
    /// task agent) may be relaunched that many times since its last
    /// progress (`records::relaunch_count`, at the task tip). One more is
    /// refused and escalates the task instead (unless it already is), the
    /// ordinary way: an escalation record at the tip. Unlike any other
    /// escalation, the operator may clear it once they've fixed the cause
    /// (`reset_relaunch`); otherwise a retire resolves it (spec §10).
    fn check_relaunch_limit(
        &self,
        task: &state::TaskState,
        agent: bool,
        limit: u32,
    ) -> Result<(), String> {
        let count = if agent {
            task.agent.as_ref().map_or(0, |a| a.relaunches)
        } else {
            task.relaunches
        };
        if count < limit {
            return Ok(());
        }
        let who = if agent { "task agent" } else { "worker" };
        let slug = &task.slug;
        let outcome = if task.status == Status::Escalated {
            "the task is already escalated".to_string()
        } else {
            let cause = format!(
                "relaunch limit ({limit}) reached: the {who} was relaunched {count} times since its last progress and died again"
            );
            records::record_relaunch_escalation(&self.hub, slug, &task.tip, &cause, agent)?;
            "the task is escalated to the operator instead".to_string()
        };
        let flag = if agent { " --agent" } else { "" };
        Err(format!(
            "relaunch: task/{slug}'s {who} was already relaunched {count} times since its last progress ([limits] max_relaunches = {limit}) — not relaunched; {outcome} (once the operator has fixed what keeps killing it: dashboard relaunch {slug}{flag} --reset; or retire, then replan)"
        ))
    }

    /// `dashboard relaunch --reset`: clears `slug`'s relaunch-limit
    /// escalation for its worker (or, with `agent`, its task agent), starts
    /// that session's relaunch count over and relaunches it. The cap mostly
    /// catches the environment (a bad `agent_cmd`, an expired login, a
    /// dialog, a usage limit), which the operator can fix; every other
    /// escalation stays retire-only. Refused unless the task's current
    /// escalation cause is that session's relaunch limit, or while the
    /// session is alive. The escalation is kept as evidence
    /// (`records::clear_escalation`).
    pub fn reset_relaunch(&mut self, slug: &str, agent: bool) -> Result<(), String> {
        let (key, who) = if agent {
            (state::agent_key(slug), "task agent")
        } else {
            (slug.to_string(), "session")
        };
        if self.sessions.alive(&key) {
            return Err(format!("relaunch: task/{slug}'s {who} is already alive"));
        }
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let task = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .ok_or_else(|| format!("relaunch: no task {slug}"))?;
        if task.status != Status::Escalated {
            return Err(format!(
                "relaunch --reset: task/{slug} is {}, not escalated — nothing to reset",
                task.status.as_str()
            ));
        }
        let cause = task.escalation.clone().unwrap_or_default();
        // The current cause must be the record's: strikes at their limit or
        // a `block:` marker outrank it (`state::escalation_cause`).
        let session = records::escalation(&self.hub, slug)
            .filter(|r| r.cause == cause)
            .and_then(|r| r.relaunch_session());
        match session {
            Some(s) if s == agent => {}
            Some(s) => {
                let flag = if s { " --agent" } else { "" };
                return Err(format!(
                    "relaunch --reset: task/{slug} is escalated for {cause}; that's the {}'s relaunch limit: dashboard relaunch {slug}{flag} --reset",
                    if s { "task agent" } else { "worker" }
                ));
            }
            None => {
                return Err(format!(
                    "relaunch --reset: task/{slug} is escalated for {cause}; only a relaunch-limit escalation can be reset (retire, then replan)"
                ));
            }
        }
        records::clear_escalation(&self.hub, slug, agent)
            .map_err(|e| format!("relaunch --reset: {e}"))?;
        records::record_relaunch_reset(&self.hub, &task.tip, agent)
            .map_err(|e| format!("relaunch --reset: escalation cleared, but {e}"))?;
        let relaunched = if agent {
            self.relaunch_agent(slug)
        } else {
            self.relaunch(slug)
        };
        relaunched
            .map_err(|e| format!("relaunch --reset: task/{slug}'s escalation is cleared, but {e}"))
    }

    // ---- worker import (spec §5, §7, §9) ----

    /// Every task with pins whose status is `Posted`/`InProgress`/
    /// `InRevision` and whose clone dir exists gets a fetch-and-validate
    /// pass. A missing clone (never launched, or a relaunch pending) is
    /// simply skipped — there's nothing to fetch from.
    fn import_workers(&mut self, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let candidates: Vec<(String, String)> = hub_state
            .tasks
            .iter()
            .filter(|t| {
                self.fsm_ctx(&hub_state, &t.slug)
                    .is_some_and(|c| fsm::decide(Actor::Worker, Move::Finish, c) == Verdict::Allow)
                    && records::has_pins(&self.hub, &t.slug)
            })
            .map(|t| (t.slug.clone(), t.tip.clone()))
            .collect();

        for (slug, hub_tip) in candidates {
            let clone_dir = paths::worker_clone(&self.director, &slug);
            if !clone_dir.exists() {
                continue;
            }
            self.import_worker(&slug, &hub_tip, &clone_dir, report)?;
        }
        Ok(())
    }

    /// `git fetch --no-tags <clone> +refs/heads/task/<slug>:refs/incoming/worker/<slug>`,
    /// then classifies the result exactly as spec's "Import semantics"
    /// describes: behind/caught-up is a no-op, diverged is a `history`
    /// rejection, a merge commit anywhere in the new range is a `history`
    /// rejection, and otherwise only the newest commit (the finish claim) is
    /// validated as a cumulative change from `hub_tip`.
    fn import_worker(
        &mut self,
        slug: &str,
        hub_tip: &str,
        clone_dir: &Path,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let clone_str = path_str(clone_dir)?;
        let incoming_ref = format!("refs/incoming/worker/{slug}");
        let refspec = format!("+refs/heads/task/{slug}:{incoming_ref}");
        if self
            .hub
            .run(&["fetch", "--no-tags", clone_str, &refspec])
            .is_err()
        {
            return Ok(()); // the worker hasn't pushed a commit yet, or the branch is missing
        }
        let incoming = match self.hub.rev_parse(&incoming_ref) {
            Some(sha) => sha,
            None => return Ok(()),
        };
        // A held finish the player has since merged main under: the worker
        // is rebasing it (`release_held`), so it's not a diverged clone.
        if records::released_commit(&self.hub, slug).as_deref() == Some(incoming.as_str()) {
            return Ok(());
        }

        if self.hub.is_ancestor(&incoming, hub_tip) {
            return Ok(()); // behind or caught up
        }
        if !self.hub.is_ancestor(hub_tip, &incoming) {
            if !records::is_rejected(&self.hub, &incoming) {
                let msg = format!("history: task/{slug}'s clone diverged from the hub");
                self.reject_and_relay(slug, &incoming, Reason::History, msg, report)?;
            }
            return Ok(());
        }

        let commits = self
            .hub
            .rev_list(&[&format!("{hub_tip}..{incoming}"), "--reverse"]);
        if commits.iter().any(|c| self.hub.parents(c).len() > 1) {
            let msg = format!(
                "history: task/{slug}'s clone contains a merge commit; only the player creates merges"
            );
            self.reject_and_relay(slug, &incoming, Reason::History, msg, report)?;
            return Ok(());
        }

        if records::is_rejected(&self.hub, &incoming) {
            return Ok(());
        }

        // A newer commit than the held one: the hold is over either way.
        if records::held_commit(&self.hub, slug).is_some_and(|h| h != incoming) {
            records::release_held(&self.hub, slug, false)?;
        }
        self.validate_finish_claim(slug, hub_tip, &incoming, report)
    }

    /// `candidate` (the newest commit fetched, `C` in spec's notation) is
    /// either a block marker, a stray empty commit, or a finish claim —
    /// dispatched exactly as spec's worker-import bullets describe.
    fn validate_finish_claim(
        &mut self,
        slug: &str,
        hub_tip: &str,
        candidate: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        if self.hub.is_empty_commit(candidate) {
            let subject = self.hub.commit_subject(candidate).unwrap_or_default();
            if subject.starts_with("block:") {
                if !self.hub.diff_names(hub_tip, candidate).is_empty() {
                    let msg = format!(
                        "history: task/{slug}'s block: commit isn't a clean reset to the hub tip — reset to hub, then block"
                    );
                    return self.reject_and_relay(slug, candidate, Reason::History, msg, report);
                }
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub
                    .update_ref_cas(&hub_ref, candidate, hub_tip)
                    .map_err(|e| e.to_string())?;
                records::record_import(
                    &self.hub,
                    candidate,
                    Actor::Worker,
                    Transition::Block,
                    Some(slug),
                )?;
                report
                    .imported
                    .push((hub_ref, candidate.to_string(), Transition::Block));
                return Ok(());
            }
            if subject.starts_with("contest:") {
                return self.try_contest(slug, hub_tip, candidate, report);
            }
            let msg = "suite: empty commit is neither a finish claim nor a block:/contest: marker"
                .to_string();
            return self.reject_and_relay(slug, candidate, Reason::Suite, msg, report);
        }

        let pins_violations = records::check_pins(&self.hub, slug, candidate);
        if !pins_violations.is_empty() {
            let msg = format!(
                "pins: task/{slug}'s finish claim changed or removed pinned files: {}",
                pins_violations.join(", ")
            );
            return self.reject_and_relay(slug, candidate, Reason::Pins, msg, report);
        }

        if records::is_spike_at(&self.hub, hub_tip, slug) {
            return self.finish_spike(slug, hub_tip, candidate, report);
        }

        if records::task_config_at(&self.hub, hub_tip, slug).is_some_and(|c| c.kind == TaskKind::Task) {
            let out = records::scope_violations(&self.hub, slug, hub_tip, candidate, Some(self.runner.exporter()));
            if !out.is_empty() {
                let msg = format!(
                    "scope: task/{slug}'s finish claim writes outside its modules: {} (run `dashboard scope`)",
                    out.join(", ")
                );
                return self.reject_and_relay(slug, candidate, Reason::Scope, msg, report);
            }
        }

        if let Some(stray) = self.flaky_scope_violations(slug, hub_tip, candidate) {
            let msg = format!(
                "pins: task/{slug} is a flaky-test-fix and may only change its fix_paths (or add new test files), not {}. If the flake is in the product code, make a `block:` commit instead: the director replans it as an ordinary task",
                stray.join(", ")
            );
            return self.reject_and_relay(slug, candidate, Reason::Pins, msg, report);
        }

        let speedup = records::task_config_at(&self.hub, hub_tip, slug)
            .filter(|c| c.kind == TaskKind::TestSpeedup);
        if let Some(cfg) = &speedup {
            let problems = self.test_speedup_scope(hub_tip, candidate, cfg);
            if !problems.is_empty() {
                let msg = format!(
                    "pins: task/{slug} is a test-speedup and may only change the test tree and its checks' cmd / retry_failed / retry_on, not: {}. If the slowness is in the product code, make a `block:` commit instead: the director replans it",
                    problems.join("; ")
                );
                return self.reject_and_relay(slug, candidate, Reason::Pins, msg, report);
            }
        }

        let suite = self.run_task_set(slug, candidate, Some(hub_tip));
        self.note_ledgers(slug, candidate, &suite);
        if !suite.pass {
            // Failing only where main already fails (impurely, or killed for
            // its timeout): not the worker's doing. Left unimported, it's
            // re-judged every tick (a cache hit) and goes through once main
            // is clean. A test-speedup is the fix, not the held work.
            if speedup.is_none() {
                if let Some(names) = self.held_by_main(&suite) {
                    return self.hold_finish(slug, candidate, &names, report);
                }
            }
            let reason = Self::suite_failure_reason(&suite);
            let msg = format!("{}: task/{slug}: {}", reason.as_str(), suite.summary());
            return self.reject_and_relay(slug, candidate, reason, msg, report);
        }
        let budget = self.runner.budget();
        if let Some(why) = suite.over_budget(&budget) {
            if let (Some(cfg), Some(main)) = (&speedup, &self.main_over) {
                // Still over, but `main` is too: progress is what counts.
                let named: Vec<String> = cfg.slow.iter().map(|s| s.check.clone()).collect();
                let problems = speedup::ratchet_problems(&named, &main.millis, &budget, &suite);
                if !problems.is_empty() {
                    let msg = format!(
                        "budget: task/{slug}: still {}, and not enough progress against main: {}",
                        why.trim_end_matches('.'),
                        problems.join("; ")
                    );
                    return self.reject_and_relay(slug, candidate, Reason::Budget, msg, report);
                }
            } else if let Some(names) = self.held_over_total(&suite) {
                return self.hold_finish(slug, candidate, &names, report);
            } else {
                let msg = format!("budget: task/{slug}: {why}");
                return self.reject_and_relay(slug, candidate, Reason::Budget, msg, report);
            }
        }
        if let Some(msg) = suite.slow(&budget) {
            report.slow.push((slug.to_string(), msg));
        }

        if let Err(problem) = self.validate_flaky_finish(slug, hub_tip, candidate) {
            let msg = format!("suite: task/{slug}: {problem}");
            return self.reject_and_relay(slug, candidate, Reason::Suite, msg, report);
        }
        let speedup_notes = match &speedup {
            Some(_) => match self.test_speedup_notes(slug, hub_tip, candidate) {
                Ok(notes) => notes,
                Err(problem) => {
                    let msg = format!("suite: task/{slug}: {problem}");
                    return self.reject_and_relay(slug, candidate, Reason::Suite, msg, report);
                }
            },
            None => String::new(),
        };

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub
            .update_ref_cas(&hub_ref, candidate, hub_tip)
            .map_err(|e| e.to_string())?;
        records::record_import(
            &self.hub,
            candidate,
            Actor::Worker,
            Transition::Finish,
            Some(slug),
        )?;
        report
            .imported
            .push((hub_ref, candidate.to_string(), Transition::Finish));
        if !speedup_notes.is_empty() {
            records::record_speedup(&self.hub, candidate, &speedup_notes)?;
            report.speedups.push((slug.to_string(), speedup_notes));
        }
        let text = self.prompts()?.render("worker.finish-accepted", &[]);
        self.relay(slug, &text);
        Ok(())
    }

    /// Files every check in `suite` that passed only on a retry into the
    /// flake ledger, and every impure one into the impurity ledger, under
    /// `scope`, so the director can plan a hygiene task with evidence. Best
    /// effort: a ledger write never fails a transition.
    fn note_ledgers(&self, scope: &str, commit: &str, suite: &checks::SuiteResult) {
        for c in &suite.checks {
            let _ = records::record_flake(&self.hub, scope, commit, c);
            let _ = records::record_impurity(&self.hub, scope, commit, c);
        }
    }

    /// For a flaky-test-fix: the paths `candidate` changes outside the task's
    /// `fix_paths`, other than brand-new files under `test_paths`. A flake
    /// whose cause is in the product code isn't this kind of task (it takes
    /// the regular path), so touching anything else is refused. `None` for
    /// any other task, or when nothing strays.
    fn flaky_scope_violations(
        &self,
        slug: &str,
        hub_tip: &str,
        candidate: &str,
    ) -> Option<Vec<String>> {
        let cfg = records::task_config_at(&self.hub, hub_tip, slug)
            .filter(|c| c.kind == TaskKind::FlakyTestFix)?;
        let test_paths = self.test_paths_at(hub_tip);
        let stray: Vec<String> = self
            .hub
            .diff_names(hub_tip, candidate)
            .into_iter()
            .filter(|(status, path)| {
                !cfg.fix_paths.contains(path)
                    && !(*status == 'A' && path_under_test_paths(path, &test_paths))
            })
            .map(|(_, path)| path)
            .collect();
        (!stray.is_empty()).then_some(stray)
    }

    /// Holds a worker's finish `candidate`, not judged: `names` are the
    /// checks `main` already fails (or `suite time`). Told once per commit.
    fn hold_finish(
        &mut self,
        slug: &str,
        candidate: &str,
        names: &[String],
        report: &mut TickReport,
    ) -> Result<(), String> {
        if records::held_commit(&self.hub, slug).as_deref() != Some(candidate) {
            records::set_held(&self.hub, slug, candidate)?;
            let checks = names.join(", ");
            report.held.push((
                slug.to_string(),
                format!(
                    "finish {} fails only where main already does (failing, impure or over budget): {checks}",
                    short(candidate)
                ),
            ));
            let text = self
                .prompts()?
                .render("worker.held", &[("checks", &checks)]);
            self.relay(slug, &text);
        }
        Ok(())
    }

    /// For a test-speedup: what `candidate` changes beyond its scope. The
    /// paths must be the test tree, the suite files (`Tests.lean`, `Tests/`)
    /// or the named checks' `files`; the suite definition may change only
    /// in its checks' `cmd` / `retry_*` (`speedup::scope_problems`). Never
    /// `roadmap.toml`, so never `[budget]`.
    fn test_speedup_scope(&self, hub_tip: &str, candidate: &str, cfg: &TaskConfig) -> Vec<String> {
        let (base, cand) = match (self.suite_at(hub_tip), self.suite_at(candidate)) {
            (Ok(b), Ok(c)) => (b, c),
            (_, Err(e)) | (Err(e), _) => return vec![format!("the suite doesn't load: {e}")],
        };
        let named: Vec<&str> = cfg.slow.iter().map(|s| s.check.as_str()).collect();
        let files = base.files_of(&named);
        let mut problems: Vec<String> = self
            .hub
            .diff_names(hub_tip, candidate)
            .into_iter()
            .map(|(_, path)| path)
            .filter(|path| {
                !(config::is_tests_path(path)
                    || path_under_test_paths(path, &base.test_paths)
                    || path_under_test_paths(path, &files))
            })
            .collect();
        problems.extend(speedup::scope_problems(&base, &cand));
        problems
    }

    /// A test-speedup's finish: the removed tests (inventory diff against the
    /// pinned base with `list_cmd`) and rewired `cmd`s, as the text the
    /// director sees. Removing is allowed, never hidden. Empty if none.
    fn test_speedup_notes(
        &self,
        slug: &str,
        hub_tip: &str,
        candidate: &str,
    ) -> Result<String, String> {
        let cand = self.suite_at(candidate)?;
        let base_commit = self
            .hub
            .rev_parse(&format!("refs/dashboard/pins/{slug}"))
            .ok_or("no pins recorded")?;
        let list_cmd = cand
            .list_cmd
            .clone()
            .ok_or("the suite has no list_cmd")?;
        let before = self.runner.run_list(&base_commit, &list_cmd)?;
        let after = self.runner.run_list(candidate, &list_cmd)?;
        let base = self.suite_at(hub_tip)?;
        Ok(speedup::removal_text(
            &flaky::removed(&before, &after),
            &speedup::changed_checks(&base, &cand),
        ))
    }

    /// A flaky-test-fix's finish, on top of the passing suite: the director's
    /// task check passes (the player, not the worker, decides the new test
    /// is stable), no test but the replaced ones vanished since the pinned
    /// base, and every replacement exists. Always `Ok` for other tasks.
    fn validate_flaky_finish(
        &self,
        slug: &str,
        hub_tip: &str,
        candidate: &str,
    ) -> Result<(), String> {
        let Some(cfg) = records::task_config_at(&self.hub, hub_tip, slug)
            .filter(|c| c.kind == TaskKind::FlakyTestFix)
        else {
            return Ok(());
        };

        let check = self.runner.run_task_check(candidate, slug)?;
        if !check.pass {
            let passed = check.exits.iter().filter(|&&e| e == 0).count();
            let tail = check
                .output_tail
                .as_deref()
                .map(|t| format!("\n{t}"))
                .unwrap_or_default();
            return Err(format!(
                "the stability check {:?} fails ({passed}/{} runs passed){tail}",
                check.name, check.runs
            ));
        }

        let list_cmd = self.suite_at(candidate)?.list_cmd.ok_or("the suite has no list_cmd")?;
        let base = self
            .hub
            .rev_parse(&format!("refs/dashboard/pins/{slug}"))
            .ok_or("no pins recorded")?;
        let before = self.runner.run_list(&base, &list_cmd)?;
        let after = self.runner.run_list(candidate, &list_cmd)?;
        let problems = flaky::inventory_problems(&before, &after, &cfg.replace);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("\n"))
        }
    }

    /// Shared by every worker-side rejection: records it, then re-derives
    /// state and relays a recovery message to the worker's session unless
    /// this very rejection just escalated the task (spec §10: "the player
    /// stops relaying"). Director rejections never go through here — only
    /// `reject` (no relay), per spec.
    fn reject_and_relay(
        &mut self,
        slug: &str,
        candidate: &str,
        reason: Reason,
        msg: String,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let rec =
            records::record_rejection(&self.hub, slug, candidate, reason, Actor::Worker, &msg)?;
        report.rejected.push(rec);

        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let escalated = hub_state
            .tasks
            .iter()
            .any(|t| t.slug == slug && t.status == Status::Escalated);
        if escalated {
            return Ok(());
        }

        let prompts = self.prompts()?;
        let recovery = match reason {
            Reason::History => "worker.recovery-history",
            _ => "worker.recovery-other",
        };
        let recovery = prompts.render(recovery, &[("slug", slug)]);
        self.relay(
            slug,
            &prompts.render(
                "worker.rejected",
                &[("message", &msg), ("recovery", &recovery)],
            ),
        );
        Ok(())
    }

    // ---- task agents (spec §17) ----

    /// Every live task with a task agent: fetch its `agent/<slug>` and
    /// validate what's new there as the task agent's post or send-back.
    fn import_task_agents(&mut self, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let candidates: Vec<(String, String)> = hub_state
            .tasks
            .iter()
            .filter(|t| {
                t.agent.is_some()
                    && !matches!(
                        t.status,
                        Status::Accepted | Status::Retired | Status::Explored
                    )
            })
            .map(|t| (t.slug.clone(), t.tip.clone()))
            .collect();
        for (slug, hub_tip) in candidates {
            let clone_dir = paths::agent_clone(&self.director, &slug);
            if clone_dir.exists() {
                self.import_task_agent(&slug, &hub_tip, &clone_dir, report)?;
            }
        }
        Ok(())
    }

    /// `import_worker`'s shape for a task agent's clone. One difference: the
    /// hub's task branch can move under a commit the player hasn't imported
    /// yet (`main` merged into a ready task, a replan). That isn't the
    /// agent's doing, so it's asked to rebase, once per commit, with no
    /// strike. Rewriting a commit the player already imported from it is
    /// `history`, as for anyone.
    fn import_task_agent(
        &mut self,
        slug: &str,
        hub_tip: &str,
        clone_dir: &Path,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let clone_str = path_str(clone_dir)?;
        let incoming_ref = format!("refs/incoming/agent/{slug}");
        if self
            .hub
            .run(&[
                "fetch",
                "--no-tags",
                clone_str,
                &format!("+refs/heads/agent/{slug}:{incoming_ref}"),
            ])
            .is_err()
        {
            return Ok(());
        }
        let Some(incoming) = self.hub.rev_parse(&incoming_ref) else {
            return Ok(());
        };
        if self.hub.is_ancestor(&incoming, hub_tip) || records::is_rejected(&self.hub, &incoming) {
            return Ok(());
        }
        if !self.hub.is_ancestor(hub_tip, &incoming) {
            let base = self
                .hub
                .run(&["merge-base", hub_tip, &incoming])
                .unwrap_or_default();
            // Only this task's own agent commits count, walked along the
            // branch's first parents: a `main` merged in brings other tasks'
            // task-agent commits with it, and those aren't this agent's.
            let rewrote_imported = base.is_empty()
                || self
                    .hub
                    .rev_list(&["--first-parent", &format!("{base}..{hub_tip}")])
                    .iter()
                    .any(|c| {
                        records::import_note(&self.hub, c).is_some_and(|n| {
                            n.actor == Actor::TaskAgent && n.slug.as_deref() == Some(slug)
                        })
                    });
            if rewrote_imported {
                let msg = format!(
                    "history: agent/{slug} diverged from the hub: a commit the player imported from you was rewritten"
                );
                return self.reject_by(
                    Actor::TaskAgent,
                    slug,
                    &incoming,
                    Reason::History,
                    msg,
                    report,
                );
            }
            if self.agent_rebase_told.get(slug) != Some(&incoming) {
                let text = self
                    .prompts()?
                    .render("task-agent.rebase", &[("slug", slug)]);
                self.relay(&state::agent_key(slug), &text);
                self.agent_rebase_told.insert(slug.to_string(), incoming);
            }
            return Ok(());
        }
        if self
            .hub
            .rev_list(&[&format!("{hub_tip}..{incoming}")])
            .iter()
            .any(|c| self.hub.parents(c).len() > 1)
        {
            let msg = format!(
                "history: agent/{slug} contains a merge commit; only the player creates merges"
            );
            return self.reject_by(
                Actor::TaskAgent,
                slug,
                &incoming,
                Reason::History,
                msg,
                report,
            );
        }
        self.try_agent_commit(slug, hub_tip, &incoming, report)
    }

    /// The newest commit a task agent made, validated as one cumulative
    /// change from the hub tip: its post while the task is staged, a
    /// send-back while it's tested. Either may touch only tests, the suite
    /// files and `tasks/<slug>/` (with `[check]` the only part of `task.toml`
    /// it may change), and it never commits a marker (spec §17).
    fn try_agent_commit(
        &mut self,
        slug: &str,
        tip: &str,
        candidate: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let Some(task) = hub_state.tasks.iter().find(|t| t.slug == slug) else {
            return Ok(());
        };
        let status = task.status;

        if self.hub.is_empty_commit(candidate) {
            let msg = format!(
                "ownership: task/{slug}: a task agent never commits a marker; accept:/retire: are the director's, so recommend in your report instead"
            );
            return self.reject_by(
                Actor::TaskAgent,
                slug,
                candidate,
                Reason::Ownership,
                msg,
                report,
            );
        }
        let diffs = self.hub.diff_names(tip, candidate);
        if let Err(msg) = self.agent_paths_ok(slug, tip, candidate, &diffs) {
            return self.reject_by(
                Actor::TaskAgent,
                slug,
                candidate,
                Reason::Ownership,
                msg,
                report,
            );
        }

        let mv = match status {
            Status::Staged => Move::Post,
            Status::Tested if is_trim(slug, &diffs) => Move::Trim,
            Status::Tested => Move::SendBack,
            Status::Contested => Move::ContestReply,
            _ => Move::Post,
        };
        let verdict = fsm::decide(
            Actor::TaskAgent,
            mv,
            self.fsm_ctx(&hub_state, slug).unwrap(),
        );
        let step = match (verdict, mv) {
            (Verdict::Allow, Move::Post) => {
                if self
                    .hub
                    .blob_hash(candidate, &format!("tasks/{slug}/prompt.md"))
                    .is_none()
                {
                    let msg = format!(
                        "ownership: task/{slug} isn't posted yet and this commit adds no tasks/{slug}/prompt.md: your only commit now is the post"
                    );
                    return self.reject_by(
                        Actor::TaskAgent,
                        slug,
                        candidate,
                        Reason::Ownership,
                        msg,
                        report,
                    );
                }
                self.try_post(
                    slug,
                    tip,
                    candidate,
                    status,
                    &hub_state,
                    Actor::TaskAgent,
                    report,
                )?;
                "post"
            }
            (Verdict::Allow, Move::SendBack) => {
                self.try_send_back(
                    slug,
                    tip,
                    candidate,
                    &diffs,
                    &hub_state,
                    Actor::TaskAgent,
                    report,
                )?;
                "send-back"
            }
            (Verdict::Allow, Move::Trim) => {
                self.try_trim(
                    slug,
                    tip,
                    candidate,
                    &diffs,
                    &hub_state,
                    Actor::TaskAgent,
                    report,
                )?;
                "trim"
            }
            (Verdict::Allow, Move::ContestReply) => {
                self.try_contest_reply(
                    slug,
                    tip,
                    candidate,
                    &diffs,
                    &hub_state,
                    Actor::TaskAgent,
                    report,
                )?;
                "contest reply"
            }
            (verdict, _) => {
                let other = status;
                if verdict == Verdict::Hold {
                    // Not the agent's doing: the plan moved the task off
                    // `staged`. Leave the commit unimported, tell it once.
                    if self.agent_hold_told.get(slug) != Some(&candidate.to_string()) {
                        let text = self.prompts()?.render(
                            "task-agent.hold",
                            &[("slug", slug), ("status", other.as_str())],
                        );
                        self.relay(&state::agent_key(slug), &text);
                        self.agent_hold_told
                            .insert(slug.to_string(), candidate.to_string());
                    }
                    return Ok(());
                }
                let msg = format!(
                    "ownership: task/{slug} is {}: there is nothing for its task agent to commit now",
                    other.as_str()
                );
                return self.reject_by(
                    Actor::TaskAgent,
                    slug,
                    candidate,
                    Reason::Ownership,
                    msg,
                    report,
                );
            }
        };

        let key = state::agent_key(slug);
        if report.imported.iter().any(|(_, sha, _)| sha == candidate) {
            let template = match step {
                "post" => "task-agent.imported-post",
                "send-back" => "task-agent.imported-send-back",
                "trim" => "task-agent.imported-trim",
                _ => "task-agent.imported-reply",
            };
            let text = self
                .prompts()?
                .render(template, &[("commit", short(candidate))]);
            self.relay(&key, &text);
        } else if let Some((_, cause)) = report.escalated.iter().find(|(s, _)| s == slug) {
            let text = self
                .prompts()?
                .render("task-agent.escalated", &[("slug", slug), ("cause", cause)]);
            self.relay(&key, &text);
        }
        Ok(())
    }

    /// Where a task agent (or the director's send-back and contest reply)
    /// may write on `slug`, read at `tip` so a commit can't widen it for
    /// itself (`records::agent_scope_at`).
    fn agent_scope(&self, slug: &str, tip: &str) -> Vec<String> {
        records::agent_scope_at(&self.hub, slug, tip, Some(self.runner.exporter()))
    }

    /// Where a task agent may write (`agent_scope`); and in `task.toml`
    /// only `[check]`, the rest being the director's plan.
    fn agent_paths_ok(
        &self,
        slug: &str,
        tip: &str,
        candidate: &str,
        diffs: &[(char, String)],
    ) -> Result<(), String> {
        let scope = self.agent_scope(slug, tip);
        for (_, path) in diffs {
            if !path_under_test_paths(path, &scope) {
                return Err(format!(
                    "ownership: task/{slug}: a task agent writes its task's tests, the prompt, reviews and the APIs inside its modules, never {path:?}: the code is the worker's"
                ));
            }
        }
        let toml_path = format!("tasks/{slug}/task.toml");
        if diffs.iter().any(|(_, p)| *p == toml_path) {
            let plan = |c: &str| {
                self.hub
                    .show_file(c, &toml_path)
                    .and_then(|t| toml::from_str::<toml::Table>(&t).ok())
                    .map(|mut t| {
                        t.remove("check");
                        t
                    })
            };
            if plan(tip) != plan(candidate) {
                return Err(format!(
                    "ownership: task/{slug}: in task.toml a task agent may only set [check]; the rest is the director's plan"
                ));
            }
        }
        Ok(())
    }

    /// Launches a task agent for every agent-managed task that is ready (to
    /// post it) or tested (to review it) and has none yet; asks a running one
    /// to review each newly tested tip; and collects every report a task
    /// agent left (spec §17).
    fn run_task_agents(&mut self, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        for task in &hub_state.tasks {
            let step = match task.status {
                Status::Staged => Some(AgentStep::Post),
                Status::Tested => Some(AgentStep::Review),
                Status::Contested => Some(AgentStep::Contest),
                _ => None,
            };
            if let Some(step) =
                step.filter(|_| state::task_agent_managed(&hub_state.roadmap, &task.config))
            {
                if task.agent.is_none() {
                    self.launch_task_agent(task, step, &hub_state.roadmap, report)?;
                } else if step != AgentStep::Post
                    && records::agent_asked(&self.hub, &task.slug).as_deref()
                        != Some(task.tip.as_str())
                {
                    records::set_agent_asked(&self.hub, &task.slug, &task.tip)?;
                    let text =
                        self.agent_step_text(&self.prompts()?, task, step, &hub_state.roadmap);
                    self.relay(&state::agent_key(&task.slug), &text);
                }
            }
            if !matches!(
                task.status,
                Status::Accepted | Status::Retired | Status::Explored
            ) {
                self.collect_agent_report(&task.slug, &task.tip, report)?;
            }
        }
        Ok(())
    }

    fn launch_task_agent(
        &mut self,
        task: &state::TaskState,
        step: AgentStep,
        roadmap: &RoadmapConfig,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let slug = &task.slug;
        // Recorded first, so a launch that fails below is never retried
        // every tick (as for workers; `dashboard relaunch --agent`).
        records::set_agent_launched(&self.hub, slug, &task.tip)?;
        if step != AgentStep::Post {
            records::set_agent_asked(&self.hub, slug, &task.tip)?;
        }

        let dir = paths::agent_clone(&self.director, slug);
        if dir.exists() {
            report.errors.push(format!("task agent: task/{slug}: {} already exists — refusing to touch a leftover agent clone", dir.display()));
            return Ok(());
        }
        if let Err(e) = self.add_worktree(&format!("agent/{slug}"), slug, &dir, None) {
            report.errors.push(format!("task agent: task/{slug}: {e}"));
            return Ok(());
        }
        let prompt_file = match Self::git_dir(&dir) {
            Ok(d) => d.join("dashboard-prompt.md"),
            Err(e) => {
                report.errors.push(format!("task agent: task/{slug}: {e}"));
                return Ok(());
            }
        };
        if let Err(e) = self.write_agent_prompt(task, step, roadmap, &dir, &prompt_file, false) {
            report.errors.push(format!("task agent: task/{slug}: {e}"));
            return Ok(());
        }
        match self.launch_session(
            &state::agent_key(slug),
            &dir,
            &roadmap.player.task_agent_cmd,
            &prompt_file,
        ) {
            Ok(()) => report.agents_launched.push(slug.clone()),
            Err(e) => report.errors.push(format!("task agent: task/{slug}: {e}")),
        }
        Ok(())
    }

    /// The task agent's prompt: the player's preamble (its role and rules,
    /// the task, the report format), then the step it starts on.
    fn write_agent_prompt(
        &self,
        task: &state::TaskState,
        step: AgentStep,
        roadmap: &RoadmapConfig,
        dir: &Path,
        prompt_file: &Path,
        relaunched: bool,
    ) -> Result<(), String> {
        let prompts = self.prompts()?;
        let report_file = Self::git_dir(dir)?
            .join("dashboard-report.md")
            .display()
            .to_string();
        let slug = task.slug.as_str();
        let toml = self
            .hub
            .show_file(&task.tip, &format!("tasks/{slug}/task.toml"))
            .unwrap_or_default();
        let goal = roadmap
            .phases
            .iter()
            .find(|p| p.id == task.config.phase)
            .and_then(|p| p.goal.clone())
            .unwrap_or_default();
        let phase = if goal.is_empty() {
            task.config.phase.clone()
        } else {
            format!("{}: {goal}", task.config.phase)
        };
        let brief = if task.config.brief.trim().is_empty() {
            "(none)"
        } else {
            task.config.brief.trim()
        };
        let preamble = prompts.render(
            "task-agent.preamble",
            &[
                ("slug", slug),
                ("project", &self.project),
                ("phase", &phase),
                ("task_toml", toml.trim()),
                ("brief", brief),
                ("report_file", &report_file),
            ],
        );
        let relaunched =
            relaunched.then(|| prompts.render("task-agent.relaunched", &[("slug", slug)]));
        let preamble = if roadmap.player.runs == config::Runs::Queued {
            Prompts::with_block(&preamble, &prompts.render("task-agent.queued", &[]))
        } else {
            preamble
        };
        let text = prompts.agent_launch(
            &preamble,
            relaunched.as_deref(),
            &self.agent_step_text(&prompts, task, step, roadmap),
        );
        std::fs::write(prompt_file, text).map_err(|e| format!("{}: {e}", prompt_file.display()))
    }

    /// What the task agent is asked to do now: the post, a review round, a
    /// contest reply, or nothing until the player pastes a step.
    fn agent_step_text(
        &self,
        prompts: &Prompts,
        task: &state::TaskState,
        step: AgentStep,
        roadmap: &RoadmapConfig,
    ) -> String {
        let slug = task.slug.as_str();
        match step {
            AgentStep::Post => {
                let check = if task.config.kind == TaskKind::FlakyTestFix {
                    prompts.render("task-agent.post-check-flaky", &[])
                } else if task.config.kind == TaskKind::TestSpeedup {
                    prompts.render("task-agent.post-check-speedup", &[])
                } else {
                    prompts.render("task-agent.post-check-task", &[("slug", slug)])
                };
                prompts.render("task-agent.post", &[("slug", slug), ("check", &check)])
            }
            AgentStep::Review => {
                let round = task.review_round + 1;
                let max = roadmap.limits.max_review_rounds;
                let base = self
                    .hub
                    .rev_parse(&format!("refs/dashboard/pins/{slug}"))
                    .unwrap_or_else(|| format!("origin/task/{slug}~1"));
                let cap = if round > max {
                    prompts.render("task-agent.review-cap-over", &[("max", &max.to_string())])
                } else if round == max {
                    prompts.render("task-agent.review-cap-last", &[])
                } else {
                    String::new()
                };
                let notes = records::speedup_notes(&self.hub, &task.tip)
                    .map(|n| prompts.render("task-agent.review-notes", &[("notes", &n)]))
                    .unwrap_or_default();
                prompts.render(
                    "task-agent.review",
                    &[
                        ("round", &round.to_string()),
                        ("slug", slug),
                        ("tip", &task.tip),
                        ("base", &base),
                        ("cap", &cap),
                        ("notes", &notes),
                    ],
                )
            }
            AgentStep::Contest => {
                let n = task.contest_round + 1;
                let reason = task.contest.clone().unwrap_or_default();
                let remaining = roadmap
                    .limits
                    .max_contest_rounds
                    .saturating_sub(n)
                    .to_string();
                prompts.render(
                    "task-agent.contest",
                    &[
                        ("n", &n.to_string()),
                        ("slug", slug),
                        ("reason", &reason),
                        ("remaining", &remaining),
                    ],
                )
            }
            AgentStep::Wait => prompts.render(
                "task-agent.wait",
                &[("slug", slug), ("status", task.status.as_str())],
            ),
        }
    }

    /// Reads and removes the report a task agent left in its clone's git
    /// directory, noting it on the task tip (spec §17) and reporting it.
    fn collect_agent_report(
        &mut self,
        slug: &str,
        tip: &str,
        report: &mut TickReport,
    ) -> Result<(), String> {
        let dir = paths::agent_clone(&self.director, slug);
        if !dir.exists() {
            return Ok(());
        }
        let Ok(file) = Self::git_dir(&dir).map(|d| d.join("dashboard-report.md")) else {
            return Ok(());
        };
        let Ok(text) = std::fs::read_to_string(&file) else {
            return Ok(());
        };
        let _ = std::fs::remove_file(&file);
        // Its stop after this is the wait for the director (`owes_work`).
        if let Some(git_dir) = file.parent() {
            let _ = std::fs::write(git_dir.join(REPORTED_FILE), "");
        }
        let rec = records::record_agent_report(
            &self.hub,
            tip,
            records::AgentReport {
                verdict: report_verdict(&text),
                text: text.trim().to_string(),
                at: 0,
            },
        )?;
        report.agent_reports.push((slug.to_string(), rec));
        Ok(())
    }

    /// Restarts `slug`'s dead task agent (`dashboard relaunch --agent`) in its
    /// existing clone, with a fresh prompt for where the task is now.
    pub fn relaunch_agent(&mut self, slug: &str) -> Result<(), String> {
        if !records::agent_launched(&self.hub, slug) {
            return Err(format!(
                "relaunch: task/{slug} has no task agent — one is launched automatically once the task is ready"
            ));
        }
        let key = state::agent_key(slug);
        if self.sessions.alive(&key) {
            return Err(format!(
                "relaunch: task/{slug}'s task agent is already alive"
            ));
        }
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let task = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .ok_or_else(|| format!("relaunch: no task {slug}"))?;
        if matches!(
            task.status,
            Status::Accepted | Status::Retired | Status::Explored
        ) {
            return Err(format!(
                "relaunch: task/{slug} is {} — its task agent is done",
                task.status.as_str()
            ));
        }
        if !hub_state.roadmap.player.task_agents_enabled() {
            return Err(
                "relaunch: task agents are off (`[player] task_agent_cmd` is empty)".to_string(),
            );
        }
        let dir = paths::agent_clone(&self.director, slug);
        if !dir.exists() {
            return Err(format!(
                "relaunch: task/{slug}'s task agent clone {} is missing",
                dir.display()
            ));
        }
        let step = match task.status {
            Status::Pending | Status::Ready | Status::Staged | Status::Orphaned => AgentStep::Post,
            Status::Tested => AgentStep::Review,
            Status::Contested => AgentStep::Contest,
            _ => AgentStep::Wait,
        };
        self.check_relaunch_limit(task, true, hub_state.roadmap.limits.max_relaunches)?;
        let prompt_file = Self::git_dir(&dir)?.join("dashboard-prompt.md");
        self.write_agent_prompt(task, step, &hub_state.roadmap, &dir, &prompt_file, true)?;
        if step != AgentStep::Post && step != AgentStep::Wait {
            records::set_agent_asked(&self.hub, slug, &task.tip)?;
        }
        self.launch_session(
            &key,
            &dir,
            &hub_state.roadmap.player.task_agent_cmd,
            &prompt_file,
        )?;
        records::record_relaunch(&self.hub, &task.tip, true)
    }

    /// Pastes the director's `question` into `slug`'s task agent session
    /// (`dashboard ask`); the answer comes back as its next report.
    pub fn ask(&mut self, slug: &str, question: &str) -> Result<(), String> {
        if !records::agent_launched(&self.hub, slug) {
            return Err(format!("ask: task/{slug} has no task agent"));
        }
        let key = state::agent_key(slug);
        if !self.sessions.alive(&key) {
            return Err(format!(
                "ask: task/{slug}'s task agent is dead — `dashboard relaunch {slug} --agent` first"
            ));
        }
        let report_file =
            Self::git_dir(&paths::agent_clone(&self.director, slug))?.join("dashboard-report.md");
        let file = report_file.display().to_string();
        let text = self.prompts()?.render(
            "task-agent.question",
            &[("question", question), ("report_file", &file)],
        );
        self.relay(&key, &text);
        Ok(())
    }

    // ---- unblock merges (spec §11) ----

    /// For every not-yet-posted task branch whose deps are all accepted (or
    /// that has none) and whose tip doesn't contain `main`'s tip, merges
    /// `main` into the branch in the bare hub (no worktree needed for a
    /// tree-level merge). Keyed on `main` itself, not only on the deps'
    /// merges: a branch unblocked earlier must keep picking up whatever
    /// lands on `main` afterwards, or the director's post commit can't
    /// update pinned tests that only exist on the newer `main`.
    fn unblock_merges(&mut self, report: &mut TickReport) -> Result<(), String> {
        let main_tip = self
            .hub
            .rev_parse("refs/heads/main")
            .ok_or_else(|| "hub has no main".to_string())?;
        let accepted = records::accepted_tasks(&self.hub, &main_tip);

        for (refname, tip) in self.hub.for_each_ref("refs/heads/task/*") {
            let Some(slug) = refname.strip_prefix("refs/heads/task/") else {
                continue;
            };

            if !matches!(
                records::import_note(&self.hub, &tip).map(|n| n.transition),
                None | Some(Transition::Plan)
                    | Some(Transition::Replan)
                    | Some(Transition::MergeUnblock)
                    | Some(Transition::Stage)
                    | Some(Transition::Unstage)
            ) {
                continue;
            }

            if accepted.contains_key(slug) {
                continue;
            }
            let Some(cfg) = self.task_config_at(&accepted, slug) else {
                continue;
            };
            if !cfg.depends_on.iter().all(|d| accepted.contains_key(d)) {
                continue;
            }
            if self.hub.is_ancestor(&main_tip, &tip) {
                continue;
            }

            match self.hub.merge_tree_write(&tip, &main_tip) {
                Ok(Some(tree)) => match self.hub.commit_tree(
                    &tree,
                    &[&tip, &main_tip],
                    &format!("Merge main into task/{slug}"),
                ) {
                    Ok(merge_sha) => match self.hub.update_ref_cas(&refname, &merge_sha, &tip) {
                        Ok(()) => {
                            records::record_import(
                                &self.hub,
                                &merge_sha,
                                Actor::Player,
                                Transition::MergeUnblock,
                                Some(slug),
                            )?;
                            report.imported.push((
                                refname.clone(),
                                merge_sha,
                                Transition::MergeUnblock,
                            ));
                            report.merges.push(format!("task/{slug}: merged main"));
                        }
                        Err(e) => report.merges.push(format!(
                            "task/{slug}: ref moved during merge, skipped this tick ({e})"
                        )),
                    },
                    Err(e) => report
                        .merges
                        .push(format!("task/{slug}: commit-tree failed: {e}")),
                },
                Ok(None) => report
                    .merges
                    .push(format!("task/{slug}: merge conflict with main, skipped")),
                Err(e) => report
                    .merges
                    .push(format!("task/{slug}: merge-tree failed: {e}")),
            }
        }
        Ok(())
    }
}

// --------------------------------------------------------------------------
// Player state persistence (step 7: `home.root/player-state.json`)
// --------------------------------------------------------------------------

/// `player-state.json`: what `emit_events` has already told the director.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PlayerState {
    /// `Player::last`.
    #[serde(default)]
    status: BTreeMap<String, Status>,
    /// `Player::deaths_told`.
    #[serde(default)]
    deaths_told: BTreeMap<String, String>,
    /// `Player::idle_told`.
    #[serde(default)]
    idle_told: BTreeMap<String, String>,
    /// `Player::panes`.
    #[serde(default)]
    panes: BTreeMap<String, PaneSeen>,
}

/// What a session's screen last looked like (`Player::watch_pane`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PaneSeen {
    /// `pane_hash` of the captured screen.
    hash: u64,
    /// The session's launch id when it was captured: a relaunch (in any
    /// process) starts a new watch even if the screen happens to match.
    launch: String,
    /// Unix time the screen was first seen like this.
    since: u64,
    /// This unchanged screen was already reported idle (or a stop it sat at
    /// was): nothing more until it changes.
    told: bool,
}

/// A stable (FNV-1a) hash of a captured screen, persisted across restarts,
/// ignoring trailing whitespace and blank lines (tmux pads the capture to
/// the pane's height). Anything else that moves counts as output: a
/// working agent's spinner or timer, but also an idle TUI's clock, which
/// keeps such a session from ever looking stale.
fn pane_hash(screen: &str) -> u64 {
    let mut lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in lines.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// "15 min", or "40 s" under a minute: how long a pane sat unchanged.
fn duration_text(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min", secs / 60)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Loads the state from `home.player_state()`. The bool is `first_run`: true
/// when no file was found at all (as opposed to a file that happens to parse
/// to nothing), which is the "very first run" spec's event emission stays
/// silent for. The older format, a bare map of slug to (status, session
/// alive), is migrated: a session it saw dead counts as already told.
fn load_player_state(home: &Home) -> (PlayerState, bool) {
    let Ok(text) = std::fs::read_to_string(home.player_state()) else {
        return (PlayerState::default(), true);
    };
    if let Ok(old) = serde_json::from_str::<BTreeMap<String, (Status, bool)>>(&text) {
        let state = PlayerState {
            idle_told: BTreeMap::new(),
            panes: BTreeMap::new(),
            deaths_told: old
                .iter()
                .filter(|(_, (_, alive))| !alive)
                .map(|(key, _)| (key.clone(), String::new()))
                .collect(),
            status: old
                .into_iter()
                .map(|(key, (status, _))| (key, status))
                .collect(),
        };
        return (state, false);
    }
    (serde_json::from_str(&text).unwrap_or_default(), false)
}

fn save_player_state(home: &Home, state: &PlayerState) -> Result<(), String> {
    let path = home.player_state();
    let json = serde_json::to_string(state).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))
}

/// Session `key`'s latest launch id (`home.launches()`), or "" when none
/// was recorded (a session launched before launch ids existed).
fn launch_id(home: &Home, key: &str) -> String {
    std::fs::read_to_string(home.launches().join(key))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Gives `key` a new launch id: the time and this process's pid, unique
/// enough for two launches never to share one.
fn record_launch(home: &Home, key: &str) -> Result<(), String> {
    let dir = home.launches();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(key);
    std::fs::write(&path, format!("{nanos}-{}\n", std::process::id()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The event a task's *new* status is worth telling the director about, and
/// its message (spec step 7). Statuses outside this set (pending, orphaned,
/// posted, in_progress, in_revision) never get their own event — they're
/// either not director-actionable or already covered by the transition that
/// caused them (e.g. `in_revision` is the director's own send-back).
///
/// With `managed` (spec §17), `ready` and `tested` belong to the task agent,
/// so the director hears `agent_launched` and `agent_report` instead.
fn status_event(task: &state::TaskState, managed: bool) -> Option<(EventKind, String)> {
    match task.status {
        Status::Tested | Status::Contested if managed => None,
        Status::Contested => Some((EventKind::Contested, task.contest.clone().unwrap_or_default())),
        Status::Tested if task.spike.is_some() => {
            Some((EventKind::Tested, "spike finish validated — claims passed; review the findings with the operator, then send back a new question or conclude".to_string()))
        }
        Status::Tested => Some((EventKind::Tested, "finish accepted — tests passed, awaiting director review".to_string())),
        Status::Escalated => Some((EventKind::Escalated, task.escalation.clone().unwrap_or_default())),
        Status::Ready if task.spike.is_some() => Some((EventKind::Ready, format!("spike task/{} is planned: post its question when the operator agrees", task.slug))),
        Status::Ready if managed => Some((EventKind::Ready, format!("task/{}'s dependencies are all accepted and merged in; it waits for a `stage:` commit before its task agent starts", task.slug))),
        Status::Ready => Some((EventKind::Ready, format!("task/{}'s dependencies are all accepted and merged in", task.slug))),
        Status::Accepted => Some((EventKind::Accepted, format!("task/{} accepted and merged into main", task.slug))),
        Status::Explored => {
            let answer = task.spike.as_ref().and_then(|sp| sp.answer.as_deref()).unwrap_or("?");
            Some((EventKind::Explored, format!("spike {} concluded: {answer}", task.slug)))
        }
        Status::Retired => Some((EventKind::Retired, first_line(task.post_mortem.as_deref().unwrap_or("")))),
        // `conflicted`: emitted from the tick report instead (`emit_events`).
        Status::Pending | Status::Staged | Status::Orphaned | Status::Posted | Status::InProgress | Status::InRevision | Status::Conflicted => None,
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

// --------------------------------------------------------------------------
// Free helpers
// --------------------------------------------------------------------------

fn valid_slug(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `path` is allowed on a director `main` commit: exactly `roadmap.toml`, or
/// under a `docs_paths` entry (an exact file match, or a directory prefix).
fn path_allowed(path: &str, docs_paths: &[String]) -> bool {
    if path == "roadmap.toml" || path == prompts::FILE || config::is_arch_path(path) {
        return true;
    }
    docs_paths.iter().any(|d| {
        let prefix = d.trim_end_matches('/');
        path == prefix || path.starts_with(&format!("{prefix}/"))
    })
}

/// `path`'s basename looks like a review note (`tasks/<slug>/reviews/r<N>.md`):
/// used only to tell "no review file at all" apart from "wrong round number"
/// in a send-back rejection message (spec §9).
/// Whether a non-empty commit on a `tested` task is a trim (spec §6.5):
/// it leaves `tasks/<slug>/` alone, so it adds no review note (no
/// send-back) and edits no plan.
fn is_trim(slug: &str, diffs: &[(char, String)]) -> bool {
    let task_prefix = format!("tasks/{slug}/");
    !diffs.iter().any(|(_, path)| path.starts_with(&task_prefix))
}

fn is_review_filename(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or("");
    base.starts_with('r') && base.ends_with(".md")
}

fn path_str(p: &Path) -> Result<&str, String> {
    p.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", p.display()))
}

/// The pinned files as a list for a worker's prompt.
fn pinned_block<'a>(paths: impl Iterator<Item = &'a str>) -> String {
    let mut sorted: Vec<&str> = paths.collect();
    sorted.sort_unstable();
    if sorted.is_empty() {
        "  (no pinned files recorded)".to_string()
    } else {
        sorted
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A worker's working tree (spec §21): its task's files only, or the
/// whole tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeKind {
    Sparse,
    Full,
}

/// What a task agent is asked to do (spec §17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentStep {
    Post,
    Review,
    /// Reply to the worker's contest (spec §18).
    Contest,
    /// Nothing pending (a relaunch while the worker works).
    Wait,
}

/// The `verdict:` of a task agent's report, or `unknown`.
fn report_verdict(text: &str) -> String {
    const KNOWN: [&str; 6] = [
        "posted",
        "accept",
        "sent-back",
        "replied",
        "unsure",
        "answer",
    ];
    text.lines()
        .find_map(|l| l.trim().strip_prefix("verdict:"))
        .and_then(|v| v.split_whitespace().next())
        .map(|v| {
            v.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .to_ascii_lowercase()
        })
        .filter(|v| KNOWN.contains(&v.as_str()))
        .unwrap_or_else(|| "unknown".to_string())
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
// Full coverage (real git repos, real hubs, the fetch/import/merge pipeline)
// lives in tests/player.rs; these unit tests only cover the pure helpers.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_slug_accepts_lowercase_digits_and_hyphens() {
        assert!(valid_slug("foo"));
        assert!(valid_slug("foo-bar-2"));
        assert!(valid_slug("2foo"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("-foo"));
        assert!(!valid_slug("Foo"));
        assert!(!valid_slug("foo_bar"));
        assert!(!valid_slug("foo bar"));
    }

    #[test]
    fn path_allowed_checks_exact_files_and_directory_prefixes() {
        let docs = vec!["docs/adr".to_string(), "CHANGELOG.md".to_string()];
        assert!(path_allowed("roadmap.toml", &[]));
        assert!(path_allowed("CHANGELOG.md", &docs));
        assert!(path_allowed("docs/adr/0001.md", &docs));
        assert!(!path_allowed("docs/adr-other/0001.md", &docs));
        assert!(!path_allowed("src/main.rs", &docs));
    }

    #[test]
    fn null_sessions_never_alive_and_records_calls() {
        let mut s = NullSessions::default();
        assert!(!s.alive("foo"));
        s.send("foo", "hello");
        s.kill("foo");
        assert_eq!(s.sent, vec![("foo".to_string(), "hello".to_string())]);
        assert_eq!(s.killed, vec!["foo".to_string()]);
    }

    #[test]
    fn screen_tail_keeps_the_last_non_blank_lines_within_the_cap() {
        assert_eq!(screen_tail(None), "no screen captured");
        assert_eq!(screen_tail(Some(" \n\n  \n")), "no screen captured");
        assert_eq!(
            screen_tail(Some("a  \n\n  b\t\n\n\n")),
            "last screen:\n  a\n    b"
        );

        // 25 lines of 200 bytes: cut to the newest that fit.
        let wide: String = (0..25).map(|i| format!("{i:0>200}\n")).collect();
        let tail = screen_tail(Some(&wide));
        assert!(tail.len() <= SCREEN_BYTES + 32, "{}", tail.len());
        assert!(tail.starts_with("last screen:\n  …\n"), "{tail}");
        assert!(tail.ends_with(&format!("{:0>200}", 24)));

        // One enormous line: its end, still capped, on a char boundary.
        let huge = "é".repeat(10_000);
        let tail = screen_tail(Some(&huge));
        assert!(tail.len() <= SCREEN_BYTES + 32, "{}", tail.len());
        assert!(tail.ends_with("éé"));
    }

    #[test]
    fn null_sessions_reap_a_lingering_dead_session_once() {
        let mut s = NullSessions::default();
        s.alive.insert("a".to_string());
        s.lingering.insert("a".to_string(), Remains::default());
        assert_eq!(s.reap("a"), None, "a live session is never reaped");
        s.alive.remove("a");
        assert_eq!(s.reap("a"), Some(Remains::default()));
        assert_eq!(s.reap("a"), None);
        assert_eq!(s.reaped, vec!["a".to_string()]);
    }

    #[test]
    fn report_verdict_reads_the_verdict_line() {
        assert_eq!(
            report_verdict("slug: s\nverdict: accept\nwhy: ok"),
            "accept"
        );
        assert_eq!(report_verdict("verdict: Sent-Back (r1)"), "sent-back");
        assert_eq!(report_verdict("  verdict: `unsure`"), "unsure");
        assert_eq!(report_verdict("verdict: maybe"), "unknown");
        assert_eq!(report_verdict("no verdict here"), "unknown");
    }

    #[test]
    fn tick_report_display_no_changes() {
        let report = TickReport::default();
        assert_eq!(report.to_string(), "no changes");
    }
}
