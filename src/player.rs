// The player (spec.md §4-§5, §9, §11): the only writer of the hub. Each tick
// fetches the director's clone, imports and validates its commits onto the
// hub's refs, and runs the player's own merges. Step 5a covered the
// skeleton, history rules, director `main` (docs-only commits) and `task/*`
// branches' plan/replan transitions, and unblock merges. Step 5b filled in
// the rest of the director-side transitions on an existing branch: post,
// send-back, accept and retire, plus the marker-commit dispatch
// (`accept:`/`retire:`, and rejecting any other empty commit as `ownership`
// since only the subagent may write `block:`). Every validation is against
// the *cumulative* diff from the hub's current tip to the candidate, exactly
// as in 5a, so a fixing commit on top of a rejected one still has a chance.
// Step 6 (this step) adds the subagent side (spec §7, §8.3, §9's launch row):
// launching a subagent clone + tmux session right after a task is posted,
// importing and validating its finish/block claims (only the newest commit
// in the fetched range is ever validated — each one is a whole finish claim,
// spec's "Import semantics"), and relaying subagent rejections and
// send-backs into its tmux session. Step 7 adds events and `dashboard wait`.

use crate::checks;
use crate::config::{self, RoadmapConfig, Reason, TaskConfig, TaskKind};
use crate::events::{self, EventKind};
use crate::flaky;
use crate::git::Git;
use crate::paths::{self, Home};
use crate::records::{self, Actor, RejectionRecord, Transition};
use crate::spike;
use crate::state::{self, Status};
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
    /// Starts the subagent's session: `agent_cmd` run in `clone_dir` with
    /// `prompt_file`'s contents as its first message (spec §9's launch row).
    fn launch(&mut self, slug: &str, clone_dir: &Path, agent_cmd: &str, prompt_file: &Path) -> Result<(), String>;
    fn send(&mut self, slug: &str, message: &str);
    fn kill(&mut self, slug: &str);
}

/// A `Sessions` that records every `launch`/`send`/`kill` call so tests can
/// assert on them instead of touching tmux, with a configurable `alive` set
/// so a test can mark a session dead again (or never alive at all) to drive
/// the relaunch/no-relaunch paths. `launch` inserts the slug into `alive`;
/// `kill` removes it — a test after that point can remove it directly too,
/// to simulate a session dying on its own.
#[derive(Debug, Default)]
pub struct NullSessions {
    pub sent: Vec<(String, String)>,
    pub killed: Vec<String>,
    pub launched: Vec<LaunchCall>,
    pub alive: BTreeSet<String>,
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

    fn launch(&mut self, slug: &str, clone_dir: &Path, agent_cmd: &str, prompt_file: &Path) -> Result<(), String> {
        self.launched.push(LaunchCall { slug: slug.to_string(), clone_dir: clone_dir.to_path_buf(), agent_cmd: agent_cmd.to_string(), prompt_file: prompt_file.to_path_buf() });
        self.alive.insert(slug.to_string());
        Ok(())
    }

    fn send(&mut self, slug: &str, message: &str) {
        self.sent.push((slug.to_string(), message.to_string()));
    }

    fn kill(&mut self, slug: &str) {
        self.killed.push(slug.to_string());
        self.alive.remove(slug);
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
        Self { project: project.into() }
    }
}

impl Sessions for TmuxSessions {
    fn alive(&self, slug: &str) -> bool {
        state::tmux_alive(&state::tmux_session_name(&self.project, slug))
    }

    /// `tmux new-session -d -s <name> -c <clone_dir> sh -c '<agent_cmd>
    /// "$(cat <prompt_file>)"'`. `clone_dir` and `agent_cmd` are passed as
    /// plain argv elements (no shell involved for the outer `tmux` call), but
    /// `prompt_file` is interpolated into the inner `sh -c` string, so it's
    /// the one path that needs shell quoting.
    fn launch(&mut self, slug: &str, clone_dir: &Path, agent_cmd: &str, prompt_file: &Path) -> Result<(), String> {
        let name = state::tmux_session_name(&self.project, slug);
        let clone_str = clone_dir.to_str().ok_or_else(|| format!("{} is not valid UTF-8", clone_dir.display()))?;
        let prompt_str = prompt_file.to_str().ok_or_else(|| format!("{} is not valid UTF-8", prompt_file.display()))?;
        let inner = format!("{agent_cmd} \"$(cat {})\"", shell_quote(prompt_str));
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &name, "-c", clone_str, "sh", "-c", &inner])
            .status()
            .map_err(|e| format!("tmux new-session for {name}: {e}"))?;
        if !status.success() {
            return Err(format!("tmux new-session for {name} failed (exit {:?})", status.code()));
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

        let child = Command::new("tmux").args(["load-buffer", "-b", &buf, "-"]).stdin(Stdio::piped()).spawn();
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
                eprintln!("dashboard: tmux load-buffer for {name} failed (exit {:?})", status.code());
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
        if let Err(e) = Command::new("tmux").args(["paste-buffer", "-d", "-p", "-b", &buf, "-t", &target]).status() {
            eprintln!("dashboard: tmux paste-buffer for {name} failed: {e}");
            return;
        }
        if let Err(e) = Command::new("tmux").args(["send-keys", "-t", &target, "Enter"]).status() {
            eprintln!("dashboard: tmux send-keys for {name} failed: {e}");
        }
    }

    /// `tmux kill-session`, ignoring "no such session" (and any other
    /// failure — a task being killed twice, or a session that already died
    /// on its own, are both fine outcomes here).
    fn kill(&mut self, slug: &str) {
        let name = state::tmux_session_name(&self.project, slug);
        let _ = Command::new("tmux").args(["kill-session", "-t", &format!("={name}")]).output();
    }
}

/// Wraps `s` in single quotes for safe interpolation into a `sh -c` string,
/// escaping any single quote in `s` itself (`'` -> `'\''`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}


// --------------------------------------------------------------------------
// TickReport
// --------------------------------------------------------------------------

/// What one `Player::tick` call did. `imported` is `(ref, sha, transition)`
/// for every commit the player advanced a hub ref to (director docs commits
/// on main, plans, replans, and the player's own unblock/accept merges land
/// here too via their `Transition::MergeUnblock`/`MergeAccept` notes, as do
/// subagent finish/block commits via `Transition::Finish`/`Block`). `merges`
/// carries a short human line per unblock-merge attempt, success or failure
/// (spec §11) — there's no reason to model it more richly than the CLI's
/// report needs. `escalated` is `(slug, cause)` for a send-back that would
/// exceed `max_review_rounds` (spec §10): the offending commit is
/// deliberately not imported and not recorded as a rejection, so it needs
/// its own place in the report. `launched` is every slug a subagent session
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
            lines.push(format!("rejected {} [{}]: {}", rej.scope, rej.reason.as_str(), rej.message));
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
    director: PathBuf,
    project: String,
    runner: checks::Runner,
    sessions: S,
    /// Every slug's status and session-liveness as of the end of the last
    /// tick that ran `emit_events`, persisted to `home.player_state()` so a
    /// restart doesn't re-emit events for statuses the director has already
    /// been told about (step 7).
    last: BTreeMap<String, (Status, bool)>,
    /// True until the first `emit_events` call completes: with no state file
    /// on disk at `open` time, that first call only initializes `last` (spec:
    /// "The very first run with no state file initializes silently, without
    /// emitting").
    first_run: bool,
    /// The last stale-base commit of each task agent the player told to
    /// rebase (`import_task_agent`), so it says so once, not every tick.
    agent_rebase_told: BTreeMap<String, String>,
}

impl<S: Sessions> Player<S> {
    /// Opens the player against an already-`dashboard init`-ed home: reads
    /// `dashboard.director` and `dashboard.project` from the hub's config.
    pub fn open(home: Home, sessions: S) -> Result<Self, String> {
        let hub = Git::new(home.hub());
        let director = hub.config_get("dashboard.director").ok_or_else(|| "hub is missing dashboard.director — run `dashboard init`".to_string())?;
        let project = hub.config_get("dashboard.project").ok_or_else(|| "hub is missing dashboard.project — run `dashboard init`".to_string())?;
        let runner = checks::Runner::new(&home);
        let (last, first_run) = load_player_state(&home);
        Ok(Self { home, hub, director: PathBuf::from(director), project, runner, sessions, last, first_run, agent_rebase_told: BTreeMap::new() })
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

    /// One pass: fetch, import director `main`, import director `task/*`
    /// branches, then run the player's own unblock merges (spec §5, §9, §11,
    /// in that order — see CLAUDE.md/spec.md for why the order matters:
    /// `main` can gain new phases/docs before tasks are validated against
    /// it, and a branch just planned this tick can be unblocked in the same
    /// tick if its deps already cleared).
    pub fn tick(&mut self) -> Result<TickReport, String> {
        let mut report = TickReport::default();
        self.fetch()?;
        self.import_main(&mut report)?;
        self.import_task_branches(&mut report)?;
        self.import_task_agents(&mut report)?;
        self.launch_subagents(&mut report)?;
        self.import_subagents(&mut report)?;
        self.unblock_merges(&mut report)?;
        self.run_task_agents(&mut report)?;
        self.emit_events(&report)?;
        Ok(report)
    }

    // ---- events (spec's "the director must not poll"; step 7) ----

    /// Writes the heartbeat, emits `DirectorRejected`/`PlayerError` straight
    /// from this tick's report, then diffs every task's (status, session
    /// alive) against `self.last` to emit `Tested`/`Escalated`/`Ready`/
    /// `Accepted`/`Retired`/`SessionDied`. `self.last` (and the "first run"
    /// silence) is what makes this idempotent across player restarts.
    fn emit_events(&mut self, report: &TickReport) -> Result<(), String> {
        events::write_heartbeat(&self.home)?;

        for rej in &report.rejected {
            if rej.actor == Actor::Director {
                let slug = if rej.scope == "_main" { None } else { Some(rej.scope.as_str()) };
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
        for slug in &report.agents_launched {
            let session = state::tmux_session_name(&self.project, &state::agent_key(slug));
            events::append(&self.home, EventKind::AgentLaunched, Some(slug), &format!("task agent launched; watch it: tmux attach -t {session}"))?;
        }
        for (slug, r) in &report.agent_reports {
            events::append(&self.home, EventKind::AgentReport, Some(slug), &r.text)?;
        }

        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let mut next: BTreeMap<String, (Status, bool)> = BTreeMap::new();
        for task in &hub_state.tasks {
            let alive = self.sessions.alive(&task.slug);
            let prev = self.last.get(&task.slug).copied();
            next.insert(task.slug.clone(), (task.status, alive));

            // A task agent's liveness is tracked under its own key.
            let finished = matches!(task.status, Status::Accepted | Status::Retired | Status::Explored);
            if let Some(agent) = &task.agent {
                let key = state::agent_key(&task.slug);
                let agent_was_alive = matches!(self.last.get(&key), Some((_, true)));
                next.insert(key, (task.status, agent.alive));
                if !self.first_run && agent_was_alive && !agent.alive && !finished {
                    events::append(&self.home, EventKind::AgentDied, Some(&task.slug), &format!("task/{}'s task agent session died", task.slug))?;
                }
            }

            if self.first_run {
                continue;
            }

            let managed = state::task_agent_managed(&hub_state.roadmap, &task.config);
            if prev.map(|(s, _)| s) != Some(task.status) {
                if let Some((kind, message)) = status_event(task, managed) {
                    events::append(&self.home, kind, Some(&task.slug), &message)?;
                }
            }

            let session_just_died = matches!(prev, Some((_, true))) && !alive;
            let still_in_flight = matches!(task.status, Status::Posted | Status::InProgress | Status::InRevision | Status::Contested);
            if session_just_died && still_in_flight {
                events::append(&self.home, EventKind::SessionDied, Some(&task.slug), &format!("task/{}'s subagent session died", task.slug))?;
            }
        }

        self.last = next;
        self.first_run = false;
        save_player_state(&self.home, &self.last)?;
        Ok(())
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
        let director = self.director.to_str().ok_or_else(|| format!("{} is not valid UTF-8", self.director.display()))?;
        self.hub
            .run(&["fetch", "--no-tags", "--prune", director, "+refs/heads/main:refs/incoming/director/main", "+refs/heads/task/*:refs/incoming/director/task/*"])
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
        let mut hub_tip = self.hub.rev_parse(hub_ref).ok_or_else(|| "hub has no main".to_string())?;

        if self.hub.is_ancestor(&incoming, &hub_tip) {
            return Ok(()); // director is behind (or exactly caught up); nothing to do
        }
        if !self.hub.is_ancestor(&hub_tip, &incoming) {
            if !records::is_rejected(&self.hub, &incoming) {
                let msg = "history: main diverged from hub; run `git reset --hard hub/main` and redo your commit".to_string();
                let rec = records::record_rejection(&self.hub, "_main", &incoming, Reason::History, Actor::Director, &msg)?;
                report.rejected.push(rec);
            }
            return Ok(());
        }

        let commits = self.hub.rev_list(&[&format!("{hub_tip}..{incoming}"), "--reverse"]);
        let mut rejected_ancestors: Vec<String> = Vec::new();
        for c in commits {
            if records::is_rejected(&self.hub, &c) {
                continue;
            }
            if self.hub.parents(&c).len() > 1 || rejected_ancestors.iter().any(|r| self.hub.is_ancestor(r, &c)) {
                let msg = format!("history: {} is a merge commit; only the player creates merges", short(&c));
                let rec = records::record_rejection(&self.hub, "_main", &c, Reason::History, Actor::Director, &msg)?;
                report.rejected.push(rec);
                rejected_ancestors.push(c);
                continue;
            }

            match self.validate_main_commit(&hub_tip, &c) {
                Ok(()) => {
                    self.hub.update_ref_cas(hub_ref, &c, &hub_tip).map_err(|e| e.to_string())?;
                    records::record_import(&self.hub, &c, Actor::Director, Transition::Docs, None)?;
                    report.imported.push((hub_ref.to_string(), c.clone(), Transition::Docs));
                    hub_tip = c;
                }
                Err((reason, msg)) => {
                    let rec = records::record_rejection(&self.hub, "_main", &c, reason, Actor::Director, &msg)?;
                    report.rejected.push(rec);
                }
            }
        }
        Ok(())
    }

    /// `base` is "the current hub main" at this point in the cumulative walk
    /// (it advances with every successfully-imported commit): docs_paths is
    /// read from it, per spec.
    fn validate_main_commit(&self, base: &str, candidate: &str) -> Result<(), (Reason, String)> {
        let roadmap = self.roadmap_at(base).map_err(|e| (Reason::Ownership, format!("ownership: {e}")))?;
        for (_, path) in self.hub.diff_names(base, candidate) {
            if !path_allowed(&path, &roadmap.docs_paths) {
                return Err((Reason::Ownership, format!("ownership: main commit touches {path:?}, which is neither roadmap.toml nor a docs_paths entry")));
            }
        }
        match self.hub.show_file(candidate, "roadmap.toml") {
            Some(text) => {
                config::roadmap_config_from_str(&text).map_err(|e| (Reason::Ownership, format!("ownership: roadmap.toml: {e}")))?;
            }
            None => return Err((Reason::Ownership, "ownership: roadmap.toml missing".to_string())),
        }
        let suite = self.runner.run_suite(candidate);
        self.note_flakes("_main", candidate, &suite);
        if !suite.pass {
            return Err((Reason::Suite, format!("suite: {}", suite.summary())));
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
                if slug.is_empty() { None } else { Some((slug.to_string(), sha)) }
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
    fn order_incoming_task_branches(&self, incoming: Vec<(String, String)>) -> Vec<(String, String)> {
        let slugs_in_batch: BTreeSet<String> = incoming.iter().map(|(s, _)| s.clone()).collect();
        let deps: BTreeMap<String, BTreeSet<String>> = incoming
            .iter()
            .map(|(slug, sha)| {
                let path = format!("tasks/{slug}/task.toml");
                let within_batch = self
                    .hub
                    .show_file(sha, &path)
                    .and_then(|text| config::task_config_from_str(&text).ok())
                    .map(|cfg| cfg.depends_on.into_iter().filter(|d| d != slug && slugs_in_batch.contains(d)).collect())
                    .unwrap_or_default();
                (slug.clone(), within_batch)
            })
            .collect();

        let mut ordered: Vec<(String, String)> = Vec::with_capacity(incoming.len());
        let mut remaining = incoming;
        while !remaining.is_empty() {
            let placed: BTreeSet<&str> = ordered.iter().map(|(s, _)| s.as_str()).collect();
            let ready = remaining.iter().position(|(slug, _)| deps.get(slug.as_str()).is_none_or(|d| d.iter().all(|dep| placed.contains(dep.as_str()))));
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

    fn import_task_branch(&mut self, slug: &str, incoming: &str, report: &mut TickReport) -> Result<(), String> {
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
                        let msg = format!("history: task/{slug} diverged from hub; run `git reset --hard hub/task/{slug}` and redo your commit");
                        let rec = records::record_rejection(&self.hub, slug, incoming, Reason::History, Actor::Director, &msg)?;
                        report.rejected.push(rec);
                    }
                    return Ok(());
                }
                // `--not main`: a conflict resolution brings in `main`'s own commits,
                // which were validated when they landed there.
                self.hub.rev_list(&[&format!("{hub_tip}..{incoming}"), "--reverse", "--not", "refs/heads/main"])
            }
            None => {
                // Not planned yet: the range is main..incoming, and the
                // branch's base (the oldest commit's parent) must already be
                // in the hub, else the director built it on some history the
                // hub never had (spec §5's fast-forward rule, applied at
                // creation time).
                let main_tip = self.hub.rev_parse("refs/heads/main").ok_or_else(|| "hub has no main".to_string())?;
                if self.hub.is_ancestor(incoming, &main_tip) {
                    return Ok(()); // nothing new relative to main
                }
                let commits = self.hub.rev_list(&[&format!("{main_tip}..{incoming}"), "--reverse"]);
                let base_ok = match commits.first().and_then(|oldest| self.hub.parents(oldest).into_iter().next()) {
                    Some(parent) => parent == main_tip || self.hub.is_ancestor(&parent, &main_tip),
                    None => false,
                };
                if !base_ok {
                    if !records::is_rejected(&self.hub, incoming) {
                        let msg = format!("history: task/{slug} diverged from hub; run `git reset --hard hub/main` and redo your commit");
                        let rec = records::record_rejection(&self.hub, slug, incoming, Reason::History, Actor::Director, &msg)?;
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
    fn process_task_commits(&mut self, slug: &str, commits: Vec<String>, report: &mut TickReport) -> Result<(), String> {
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
            if parents.len() == 2 && tip.as_deref() == Some(parents[0].as_str()) && !rejected_ancestors.iter().any(|r| self.hub.is_ancestor(r, &c)) {
                self.try_resolve(slug, &parents[0], &parents[1], &c, report)?;
                continue;
            }
            if parents.len() > 1 || rejected_ancestors.iter().any(|r| self.hub.is_ancestor(r, &c)) {
                // Rejection scope is the slug even before the branch exists
                // (a plan attempt that used a merge commit): these are
                // strikes on the slug the moment it's later planned, since
                // strikes are just rejection records keyed by scope string.
                let msg = format!("history: task/{slug} commit {} is a merge commit; the only director merge is a conflict resolution, on the hub's task tip", short(&c));
                let rec = records::record_rejection(&self.hub, slug, &c, Reason::History, Actor::Director, &msg)?;
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

    fn try_plan(&mut self, slug: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        let main_tip = self.hub.rev_parse("refs/heads/main").ok_or_else(|| "hub has no main".to_string())?;
        match self.validate_plan(slug, &main_tip, candidate) {
            Ok(()) => {
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub.update_ref_cas(&hub_ref, candidate, "").map_err(|e| e.to_string())?;
                records::record_import(&self.hub, candidate, Actor::Director, Transition::Plan, Some(slug))?;
                report.imported.push((hub_ref, candidate.to_string(), Transition::Plan));
            }
            Err(msg) => {
                let rec = records::record_rejection(&self.hub, slug, candidate, Reason::Plan, Actor::Director, &msg)?;
                report.rejected.push(rec);
            }
        }
        Ok(())
    }

    /// slug unused (incl. retired); the cumulative diff from `main_tip` adds
    /// only `tasks/<slug>/task.toml`; then the shared task.toml validation.
    fn validate_plan(&self, slug: &str, main_tip: &str, candidate: &str) -> Result<(), String> {
        if !valid_slug(slug) {
            return Err(format!("plan: {slug:?} is not a valid slug (must match [a-z0-9][a-z0-9-]*)"));
        }
        if self.hub.ref_exists(&format!("refs/retired/{slug}")) {
            return Err(format!("plan: task/{slug} is retired; slugs are never reused"));
        }
        let accepted = records::accepted_tasks(&self.hub, main_tip);
        if accepted.contains_key(slug) {
            return Err(format!("plan: task/{slug} is already accepted; slugs are never reused"));
        }
        if records::is_explored(&self.hub, slug) {
            return Err(format!("plan: task/{slug} is an explored spike; slugs are never reused"));
        }

        let expected_path = format!("tasks/{slug}/task.toml");
        let diffs = self.hub.diff_names(main_tip, candidate);
        if diffs.len() != 1 || diffs[0].0 != 'A' || diffs[0].1 != expected_path {
            return Err(format!("plan: task/{slug}'s commits may only add {expected_path}"));
        }

        let text = self.hub.show_file(candidate, &expected_path).ok_or_else(|| format!("plan: {expected_path} missing at {candidate}"))?;
        let cfg = config::task_config_from_str(&text).map_err(|e| format!("plan: {expected_path}: {e}"))?;
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
    fn try_existing(&mut self, slug: &str, tip: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let status = hub_state.tasks.iter().find(|t| t.slug == slug).map(|t| t.status);

        let diffs = self.hub.diff_names(tip, candidate);
        let cumulative_empty = diffs.is_empty();

        if self.hub.is_empty_commit(candidate) {
            return self.try_marker(slug, tip, candidate, cumulative_empty, status, report);
        }

        let managed = hub_state.tasks.iter().find(|t| t.slug == slug).is_some_and(|t| state::task_agent_managed(&hub_state.roadmap, &t.config));
        match status {
            Some(Status::Pending | Status::Ready | Status::Orphaned) => {
                let prompt_path = format!("tasks/{slug}/prompt.md");
                let has_prompt = self.hub.blob_hash(candidate, &prompt_path).is_some();
                if has_prompt && managed {
                    let msg = format!("task/{slug}'s post is its task agent's (spec §17), not the director's; with `[player] task_agent_cmd = \"\"` the director posts itself");
                    return self.reject(slug, candidate, Reason::Ownership, msg, report);
                }
                if has_prompt {
                    return self.try_post(slug, tip, candidate, status.unwrap(), &hub_state, Actor::Director, report);
                }
                let task_toml_path = format!("tasks/{slug}/task.toml");
                if diffs.len() == 1 && diffs[0].1 == task_toml_path {
                    return self.try_replan(slug, tip, candidate, report);
                }
                let msg = format!(
                    "task/{slug} is {}: this commit adds no prompt.md (not a post) and touches more than task.toml (not a replan)",
                    status.unwrap().as_str()
                );
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
            Some(Status::Tested) if records::is_spike_at(&self.hub, tip, slug) => self.try_spike_send_back(slug, tip, candidate, &diffs, &hub_state, report),
            Some(Status::Tested) if managed => {
                let msg = format!("task/{slug}'s send-backs are its task agent's (spec §17), not the director's: accept, or ask it (`dashboard ask {slug}`)");
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
            Some(Status::Tested) => self.try_send_back(slug, tip, candidate, &diffs, &hub_state, Actor::Director, report),
            Some(Status::Contested) if managed => {
                let msg = format!("task/{slug}'s contest is its task agent's to answer (spec §18), not the director's: ask it (`dashboard ask {slug}`)");
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
            Some(Status::Contested) => self.try_contest_reply(slug, tip, candidate, &diffs, &hub_state, Actor::Director, report),
            Some(Status::Conflicted) => {
                let main = hub_state.tasks.iter().find(|t| t.slug == slug).and_then(|t| t.conflict.as_ref()).map(|c| c.main.clone()).unwrap_or_default();
                let msg = format!("task/{slug} is conflicted: resolve it with a merge of main {main} on the hub's task tip (or retire it)");
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
            Some(other) => {
                let msg = format!("task/{slug} is {} — the director may not commit until the subagent finishes", other.as_str());
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
            None => {
                let msg = format!("task/{slug} has no derivable status; refusing to guess a transition");
                self.reject(slug, candidate, Reason::Ownership, msg, report)
            }
        }
    }

    /// Records a rejection under `reason` for `candidate` and pushes it onto
    /// the report. Shared by every `try_existing` sub-handler's error path.
    fn reject(&mut self, slug: &str, candidate: &str, reason: Reason, msg: String, report: &mut TickReport) -> Result<(), String> {
        self.reject_by(Actor::Director, slug, candidate, reason, msg, report)
    }

    /// `reject` for a commit by `actor`: a task agent's rejection is also
    /// relayed to its session (spec §17), like a subagent's; a director's
    /// never is.
    fn reject_by(&mut self, actor: Actor, slug: &str, candidate: &str, reason: Reason, msg: String, report: &mut TickReport) -> Result<(), String> {
        let rec = records::record_rejection(&self.hub, slug, candidate, reason, actor, &msg)?;
        report.rejected.push(rec);
        if actor == Actor::TaskAgent {
            let recovery = match reason {
                Reason::History => format!("git fetch origin && git reset --hard origin/task/{slug}, then redo your commit"),
                _ => format!("fix it with a new commit on top, or git reset --hard origin/task/{slug} and redo it"),
            };
            self.sessions.send(&state::agent_key(slug), &format!("The player rejected your commit.\n\n{msg}\n\nRecovery: {recovery}"));
        }
        Ok(())
    }

    /// An empty commit on an existing branch: a marker (spec §5, §9). Only
    /// meaningful when the cumulative diff from `tip` is *also* empty — if
    /// it isn't, a rejected non-empty commit sits between `tip` and
    /// `candidate` (this empty commit is just the tip of that same broken
    /// stack), so it's rejected as `ownership` rather than silently treated
    /// as a marker on old, already-rejected content.
    fn try_marker(&mut self, slug: &str, tip: &str, candidate: &str, cumulative_empty: bool, status: Option<Status>, report: &mut TickReport) -> Result<(), String> {
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
        let msg = format!("task/{slug} commit {} is an empty marker with subject {subject:?} — only `accept:`/`retire:`/`conclude:` are the director's to make here", short(candidate));
        self.reject(slug, candidate, Reason::Ownership, msg, report)
    }

    // ---- post (spec §9, §6.3) ----

    fn try_post(&mut self, slug: &str, tip: &str, candidate: &str, status: Status, hub_state: &state::HubState, actor: Actor, report: &mut TickReport) -> Result<(), String> {
        // A flake that doesn't reproduce is `evidence`, not `post`: the
        // director made no structural mistake (it has its own strike limit,
        // 1 by default, and the player retires the task itself).
        let mut reason = Reason::Post;
        let validated = if records::is_spike_at(&self.hub, candidate, slug) {
            self.validate_spike_post(slug, tip, candidate, status)
        } else {
            self.validate_post(slug, candidate, status, hub_state).and_then(|()| {
                self.evidence_miss(slug, candidate).map_or(Ok(()), |miss| {
                    reason = Reason::Evidence;
                    Err(miss)
                })
            })
        };
        match validated {
            Ok(()) => {
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub.update_ref_cas(&hub_ref, candidate, tip).map_err(|e| e.to_string())?;
                records::record_import(&self.hub, candidate, actor, Transition::Post, Some(slug))?;
                report.imported.push((hub_ref, candidate.to_string(), Transition::Post));
                // Launching the subagent clone + tmux session happens in
                // `launch_subagents`, later in the same tick (spec §8.3): it
                // scans every `Posted` task without pins yet, which this one
                // now is.
            }
            Err(msg) => {
                self.reject_by(actor, slug, candidate, reason, msg.clone(), report)?;
                if reason == Reason::Evidence {
                    let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
                    let strikes = hub_state.tasks.iter().find(|t| t.slug == slug).and_then(|t| t.strikes.get(&Reason::Evidence).copied()).unwrap_or(0);
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
        let cfg = records::task_config_at(&self.hub, commit, slug).filter(|c| c.kind == TaskKind::FlakyTestFix)?;
        cfg.evidence.iter().find_map(|e| flaky::miss(e, &self.runner.run_evidence(commit, e)))
    }

    /// The player's own retire, for a dead end that needs no director
    /// judgement (a flake that doesn't reproduce): the branch moves to
    /// `refs/retired/<slug>` under a player-made marker commit on top of
    /// `tip` whose body is the post-mortem. There is no subagent work to
    /// review, so no `retire:` commit is needed.
    fn auto_retire(&mut self, slug: &str, tip: &str, post_mortem: &str, report: &mut TickReport) -> Result<(), String> {
        let tree = self.hub.rev_parse(&format!("{tip}^{{tree}}")).ok_or_else(|| format!("no tree at {tip}"))?;
        let subject = post_mortem.split(" (").next().unwrap_or(post_mortem);
        let marker = self.hub.commit_tree(&tree, &[tip], &format!("retire: {subject}\n\n{post_mortem}")).map_err(|e| e.to_string())?;

        self.hub.update_ref(&format!("refs/retired/{slug}"), &marker).map_err(|e| e.to_string())?;
        self.hub.delete_ref_cas(&format!("refs/heads/task/{slug}"), tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, &marker, Actor::Player, Transition::Retire, Some(slug))?;
        report.imported.push((format!("refs/retired/{slug}"), marker, Transition::Retire));

        self.end_task_sessions(slug);
        Ok(())
    }

    /// spec §9's post row: status must be `ready` (other tasks may be in
    /// flight: spec §4's parallel tasks); every accepted dependency's merge must already be an
    /// ancestor of `candidate` (else the branch hasn't been unblocked into
    /// yet); `task.toml` must parse and declare a check; its suite fragment
    /// `tests.d/<slug>.toml` must carry a matching entry and the whole suite
    /// must load (spec §6.3); and that check must currently
    /// *fail* (spec §3.1's evidence requirement).
    fn validate_post(&self, slug: &str, candidate: &str, status: Status, hub_state: &state::HubState) -> Result<(), String> {
        let task = hub_state.tasks.iter().find(|t| t.slug == slug).ok_or_else(|| format!("post: task/{slug}: no task state"))?;

        if status != Status::Ready {
            if status == Status::Orphaned {
                let retired = records::retired_slugs(&self.hub);
                let retired_deps: Vec<&str> = task.config.depends_on.iter().filter(|d| retired.iter().any(|r| r == *d)).map(|s| s.as_str()).collect();
                return Err(format!("post: task/{slug} is orphaned: depends on retired {}", retired_deps.join(", ")));
            }
            let accepted = records::accepted_tasks(&self.hub, &hub_state.main_tip);
            let unmet: Vec<&str> = task.config.depends_on.iter().filter(|d| !accepted.contains_key(*d)).map(|s| s.as_str()).collect();
            return Err(format!("post: task/{slug}: dependencies not accepted: {}", unmet.join(", ")));
        }

        let accepted = records::accepted_tasks(&self.hub, &hub_state.main_tip);
        for dep in &task.config.depends_on {
            if let Some(merge_sha) = accepted.get(dep) {
                if !self.hub.is_ancestor(merge_sha, candidate) {
                    return Err(format!("post: task/{slug}: wait for the player to merge main into the branch, then fetch (missing {dep}'s acceptance merge)"));
                }
            }
        }

        let task_toml_path = format!("tasks/{slug}/task.toml");
        let text = self.hub.show_file(candidate, &task_toml_path).ok_or_else(|| format!("post: task/{slug}: {task_toml_path} missing"))?;
        let cfg = config::task_config_from_str(&text).map_err(|e| format!("post: task/{slug}: {task_toml_path}: {e}"))?;
        let check = cfg.check.ok_or_else(|| format!("post: task/{slug}: {task_toml_path} has no [check] section"))?;

        let suite = config::suite_at(&self.hub, candidate).map_err(|e| format!("post: task/{slug}: {e}"))?;
        let fragment_path = config::suite_fragment_path(slug);
        let fragment = self.hub.blob_hash(candidate, &fragment_path).is_some();
        if cfg.kind == TaskKind::FlakyTestFix {
            // The stress check is a one-off proof for this task, an exception
            // to spec §6.3: it must not slow every future suite.
            if fragment || suite.check.iter().any(|c| c.name == check.name) {
                return Err(format!("post: task/{slug}: a flaky-test-fix's check {:?} must not be added to the suite (tests.toml or {fragment_path})", check.name));
            }
            if suite.list_cmd.is_none() {
                return Err(format!("post: task/{slug}: a flaky-test-fix needs `list_cmd` in tests.toml (the player compares test inventories with it)"));
            }
        } else {
            // Read the fragment on its own: the check must live there, not in
            // tests.toml, so parallel posts never edit the same file.
            let own = self.hub.show_file(candidate, &fragment_path).and_then(|t| config::suite_from_parts("", &[(fragment_path.clone(), t)]).ok()).map(|c| c.check).unwrap_or_default();
            if !own.iter().any(|c| c.name == check.name && c.cmd == check.cmd && c.runs == check.runs && c.min_pass == check.min_pass) {
                return Err(format!("post: task/{slug}: {fragment_path} has no [[check]] entry matching the task check {:?} (same name, cmd, runs, min_pass)", check.name));
            }
        }

        let result = self.runner.run_task_check(candidate, slug).map_err(|e| format!("post: task/{slug}: {e}"))?;
        if result.pass {
            return Err(format!("post: task/{slug}: task check already passes — it must fail at post"));
        }

        Ok(())
    }

    // ---- send-back (spec §9, §8.3) ----

    /// spec §9's send-back row: the cumulative diff must add exactly the
    /// next review file (`reviews/r<N+1>.md`, `N` = the current review
    /// round); every other changed path must be a test path (spec §6.2),
    /// `tests.toml` itself, or a suite fragment under `tests.d/`; a round beyond `max_review_rounds` escalates
    /// instead of importing; and the project suite must now *fail* (spec
    /// §3.1's evidence requirement for a send-back). On success, re-pins and
    /// relays the review note to the subagent's session (spec §8.3).
    fn try_send_back(&mut self, slug: &str, tip: &str, candidate: &str, diffs: &[(char, String)], hub_state: &state::HubState, actor: Actor, report: &mut TickReport) -> Result<(), String> {
        let task = hub_state.tasks.iter().find(|t| t.slug == slug).ok_or_else(|| format!("send-back: task/{slug}: no task state"))?;
        let n = task.review_round;
        let expected_path = format!("tasks/{slug}/reviews/r{}.md", n + 1);
        let review_prefix = format!("tasks/{slug}/reviews/");

        let added_a_review_file = diffs.iter().any(|(status, path)| *status == 'A' && path.starts_with(&review_prefix) && is_review_filename(path));
        let added_expected_file = diffs.iter().any(|(status, path)| *status == 'A' && *path == expected_path);

        if !added_expected_file {
            if !added_a_review_file {
                let msg = format!("task/{slug}'s commit doesn't add a review file (expected {expected_path})");
                return self.reject_by(actor, slug, candidate, Reason::Ownership, msg, report);
            }
            let msg = format!("send-back: task/{slug}: expected review round {} to add {expected_path}", n + 1);
            return self.reject_by(actor, slug, candidate, Reason::Review, msg, report);
        }

        let test_paths = self.hub.show_file(candidate, "tests.toml").and_then(|t| config::tests_config_from_str(&t).ok()).map(|c| c.test_paths).unwrap_or_default();
        let task_prefix = format!("tasks/{slug}/");
        for (_, path) in diffs {
            if path == "tests.toml" || config::is_suite_fragment(path) || path.starts_with(&task_prefix) || path_under_test_paths(path, &test_paths) {
                continue;
            }
            let msg = format!("send-back: task/{slug}: a send-back may only add the review note and tests, not {path:?}");
            return self.reject_by(actor, slug, candidate, Reason::Review, msg, report);
        }

        let round = n + 1;
        if round > hub_state.roadmap.limits.max_review_rounds {
            let cause = format!("review rounds exceeded: {round} > {}", hub_state.roadmap.limits.max_review_rounds);
            records::record_escalation(&self.hub, slug, candidate, &cause)?;
            report.escalated.push((slug.to_string(), cause));
            return Ok(());
        }

        let suite = self.runner.run_suite(candidate);
        self.note_flakes(slug, candidate, &suite);
        if suite.pass {
            let msg = format!("send-back: task/{slug}: send-back without evidence: the project suite still passes");
            return self.reject_by(actor, slug, candidate, Reason::Review, msg, report);
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub.update_ref_cas(&hub_ref, candidate, tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, actor, Transition::SendBack, Some(slug))?;
        report.imported.push((hub_ref, candidate.to_string(), Transition::SendBack));

        records::set_pins(&self.hub, slug, candidate)?;
        let review_body = self.hub.show_file(candidate, &expected_path).unwrap_or_default();
        let header = format!("Review round {round} from the director. Fetch it: git pull --ff-only origin task/{slug}. Then make the whole project suite pass and commit.");
        self.sessions.send(slug, &format!("{header}\n\n{review_body}"));
        Ok(())
    }

    // ---- contest (spec §18) ----

    /// A subagent's `contest:` marker: a test it must pass is wrong. Like a
    /// block it must be a clean marker on the hub tip, but it doesn't
    /// escalate: the task is `contested` until the task agent (or the
    /// director) replies. A contest beyond `max_contest_rounds` escalates
    /// instead of importing, as a send-back beyond `max_review_rounds` does.
    fn try_contest(&mut self, slug: &str, hub_tip: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        if !self.hub.diff_names(hub_tip, candidate).is_empty() {
            let msg = format!("history: task/{slug}'s contest: commit isn't a clean reset to the hub tip — reset to hub, then contest");
            return self.reject_and_relay(slug, candidate, Reason::History, msg, report);
        }
        if records::is_spike_at(&self.hub, hub_tip, slug) {
            let msg = format!("suite: task/{slug} is a spike: it has no director's test to contest (block: instead)");
            return self.reject_and_relay(slug, candidate, Reason::Suite, msg, report);
        }
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let n = hub_state.tasks.iter().find(|t| t.slug == slug).map(|t| t.contest_round).unwrap_or(0) + 1;
        let max = hub_state.roadmap.limits.max_contest_rounds;
        if n > max {
            let subject = self.hub.commit_subject(candidate).unwrap_or_default();
            let cause = format!("contest rounds exceeded: {n} > {max} ({subject})");
            records::record_escalation(&self.hub, slug, candidate, &cause)?;
            report.escalated.push((slug.to_string(), cause));
            return Ok(());
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub.update_ref_cas(&hub_ref, candidate, hub_tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, Actor::Subagent, Transition::Contest, Some(slug))?;
        report.imported.push((hub_ref, candidate.to_string(), Transition::Contest));
        self.sessions.send(slug, &format!("contest c{n} recorded; don't commit further. The reply will be pasted here."));
        Ok(())
    }

    /// The reply to a contest, by the task agent or (task agents off) the
    /// director: one commit adding `tasks/<slug>/contests/c<N>.md` and
    /// optionally fixed tests, nothing else. No suite requirement, and no
    /// review round spent: the reply may keep the test and explain it. The
    /// player re-pins and relays the reply to the subagent.
    fn try_contest_reply(&mut self, slug: &str, tip: &str, candidate: &str, diffs: &[(char, String)], hub_state: &state::HubState, actor: Actor, report: &mut TickReport) -> Result<(), String> {
        let task = hub_state.tasks.iter().find(|t| t.slug == slug).ok_or_else(|| format!("contest: task/{slug}: no task state"))?;
        let n = task.contest_round + 1;
        let expected = format!("tasks/{slug}/contests/c{n}.md");
        if !diffs.iter().any(|(st, p)| *st == 'A' && *p == expected) {
            let msg = format!("ownership: task/{slug} is contested: the reply must add {expected}");
            return self.reject_by(actor, slug, candidate, Reason::Ownership, msg, report);
        }
        let test_paths = self.hub.show_file(tip, "tests.toml").and_then(|t| config::tests_config_from_str(&t).ok()).map(|c| c.test_paths).unwrap_or_default();
        let task_prefix = format!("tasks/{slug}/");
        if let Some((_, path)) = diffs.iter().find(|(_, p)| !(p == "tests.toml" || config::is_suite_fragment(p) || p.starts_with(&task_prefix) || path_under_test_paths(p, &test_paths))) {
            let msg = format!("ownership: task/{slug}: a contest reply may only add the reply and fix tests, not {path:?}");
            return self.reject_by(actor, slug, candidate, Reason::Ownership, msg, report);
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub.update_ref_cas(&hub_ref, candidate, tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, actor, Transition::ContestReply, Some(slug))?;
        report.imported.push((hub_ref, candidate.to_string(), Transition::ContestReply));

        records::set_pins(&self.hub, slug, candidate)?;
        let reply = self.hub.show_file(candidate, &expected).unwrap_or_default();
        let header = format!(
            "Reply to your contest c{n}. Fetch it: git pull --ff-only origin task/{slug} (tests may have changed; they're re-pinned). \
Then make the whole project suite pass and commit, or contest/block again."
        );
        self.sessions.send(slug, &format!("{header}\n\n{reply}"));
        Ok(())
    }

    // ---- accept (spec §9, §11) ----

    /// spec §9/§11's accept row: the task must be `tested` and not a spike.
    /// The player then lands it itself (`land`), merging `main` into the
    /// branch first if other tasks moved it.
    fn try_accept(&mut self, slug: &str, tip: &str, candidate: &str, status: Option<Status>, report: &mut TickReport) -> Result<(), String> {
        if status != Some(Status::Tested) {
            let msg = format!("accept: task/{slug} is {} — it must be tested before it can be accepted", status.map(|s| s.as_str()).unwrap_or("unknown"));
            return self.reject(slug, candidate, Reason::Accept, msg, report);
        }

        if records::is_spike_at(&self.hub, tip, slug) {
            let msg = format!("accept: task/{slug} is a spike: it never merges into main (finish it with `conclude:`)");
            return self.reject(slug, candidate, Reason::Accept, msg, report);
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
    /// task back to the subagent (`sync_revision`, no review round spent).
    fn land(&mut self, slug: &str, tip: &str, candidate: &str, transition: Transition, report: &mut TickReport) -> Result<(), String> {
        let hub_ref = format!("refs/heads/task/{slug}");
        let main_tip = self.hub.rev_parse("refs/heads/main").ok_or_else(|| "hub has no main".to_string())?;

        let mut sync: Option<String> = None;
        if !self.hub.is_ancestor(&main_tip, candidate) {
            let (tree, conflicts) = self.hub.merge_tree_conflicts(candidate, &main_tip).map_err(|e| e.to_string())?;
            if !conflicts.is_empty() {
                self.hub.update_ref_cas(&hub_ref, candidate, tip).map_err(|e| e.to_string())?;
                records::record_import(&self.hub, candidate, Actor::Director, transition, Some(slug))?;
                report.imported.push((hub_ref, candidate.to_string(), transition));
                records::record_conflict(&self.hub, candidate, &main_tip, &conflicts)?;
                report.conflicts.push((slug.to_string(), format!("merging main {main_tip} into task/{slug} conflicts in {}", conflicts.join(", "))));
                return Ok(());
            }
            sync = Some(self.hub.commit_tree(&tree, &[candidate, &main_tip], &format!("Merge main into task/{slug}")).map_err(|e| e.to_string())?);
        }
        let head = sync.clone().unwrap_or_else(|| candidate.to_string());

        // The `Task: <slug>` trailer must be the merge commit's last
        // paragraph, so `records::accepted_tasks`'s line scan finds it.
        let marker_body = self.accept_marker_body(candidate);
        let message = if marker_body.is_empty() { format!("Merge task/{slug}\n\nTask: {slug}") } else { format!("Merge task/{slug}\n\n{marker_body}\n\nTask: {slug}") };
        let tree = self.hub.rev_parse(&format!("{head}^{{tree}}")).ok_or_else(|| format!("no tree at {head}"))?;
        let merge_sha = self.hub.commit_tree(&tree, &[&main_tip, &head], &message).map_err(|e| e.to_string())?;

        let suite = self.runner.run_suite(&merge_sha);
        self.note_flakes(slug, &merge_sha, &suite);
        if !suite.pass {
            if sync.is_none() && transition == Transition::Accept {
                // Nothing was merged in: this is the code that passed at
                // finish, so the failure isn't the merge's.
                let msg = format!("accept: task/{slug}: {}", suite.summary());
                return self.reject(slug, candidate, Reason::Accept, msg, report);
            }
            self.hub.update_ref_cas(&hub_ref, &head, tip).map_err(|e| e.to_string())?;
            match &sync {
                Some(sync) => {
                    records::record_import(&self.hub, candidate, Actor::Director, transition, Some(slug))?;
                    records::record_import(&self.hub, sync, Actor::Player, Transition::SyncRevision, Some(slug))?;
                    report.imported.push((hub_ref.clone(), candidate.to_string(), transition));
                    report.imported.push((hub_ref, sync.clone(), Transition::SyncRevision));
                }
                None => {
                    records::record_import(&self.hub, candidate, Actor::Director, Transition::SyncRevision, Some(slug))?;
                    report.imported.push((hub_ref, candidate.to_string(), Transition::SyncRevision));
                }
            }
            records::set_pins(&self.hub, slug, &head)?;
            report.merge_failed.push((slug.to_string(), format!("the suite fails with main merged into task/{slug}; back to the subagent\n{}", suite.summary())));
            self.sessions.send(
                slug,
                &format!(
                    "main was merged into task/{slug} to accept it, and the project suite now fails on the merge. Fetch it: git pull --ff-only origin task/{slug}. \
Make the whole suite pass and commit. The pinned files are re-pinned at the merge.\n\n{}",
                    suite.summary()
                ),
            );
            return Ok(());
        }

        self.hub.update_ref_cas(&hub_ref, &head, tip).map_err(|e| e.to_string())?;
        self.hub.update_ref_cas("refs/heads/main", &merge_sha, &main_tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, Actor::Director, transition, Some(slug))?;
        report.imported.push((hub_ref.clone(), candidate.to_string(), transition));
        if let Some(sync) = &sync {
            records::record_import(&self.hub, sync, Actor::Player, Transition::Sync, Some(slug))?;
            report.imported.push((hub_ref, sync.clone(), Transition::Sync));
        }
        records::record_import(&self.hub, &merge_sha, Actor::Player, Transition::MergeAccept, Some(slug))?;
        report.imported.push(("refs/heads/main".to_string(), merge_sha, Transition::MergeAccept));

        // The task branch itself stays in the hub as a permanent record
        // (spec §9); only the subagent's live session and its now-unneeded
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
            .find(|c| self.hub.commit_subject(c).is_some_and(|s| s.starts_with("accept:")))
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
    fn try_resolve(&mut self, slug: &str, tip: &str, theirs: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let task = hub_state.tasks.iter().find(|t| t.slug == slug);
        let Some(conflict) = task.and_then(|t| t.conflict.clone()) else {
            let msg = format!(
                "history: task/{slug} is {} — the only director merge is the resolution of a conflict the player recorded (the player merges main itself)",
                task.map(|t| t.status.as_str()).unwrap_or("unknown")
            );
            return self.reject(slug, candidate, Reason::History, msg, report);
        };
        if theirs != conflict.main {
            let msg = format!("resolve: task/{slug}'s conflict is with main {}: merge exactly that commit, not {}", conflict.main, short(theirs));
            return self.reject(slug, candidate, Reason::Ownership, msg, report);
        }

        let (merged, _) = self.hub.merge_tree_conflicts(tip, theirs).map_err(|e| e.to_string())?;
        let stray: Vec<String> = self.hub.diff_names(&merged, candidate).into_iter().map(|(_, p)| p).filter(|p| !conflict.paths.contains(p)).collect();
        if !stray.is_empty() {
            let msg = format!("resolve: task/{slug}: a resolution may only change the conflicted paths ({}), not {}", conflict.paths.join(", "), stray.join(", "));
            return self.reject(slug, candidate, Reason::Ownership, msg, report);
        }

        records::set_pins(&self.hub, slug, candidate)?;
        self.land(slug, tip, candidate, Transition::Resolve, report)
    }

    // ---- spikes (spec §14) ----

    /// spec §14's post row for a spike: the spike is `ready`; the post commit only adds the director's own files
    /// (`prompt.md`, `questions/1.toml`, optional `baseline/1.toml` and its
    /// scripts, anything else under `tasks/<slug>/` but `claims/` and
    /// `findings/`); the question is valid; and the director's baseline claims
    /// pass on the current code. There is no task check and no `tests.toml`
    /// entry: a spike never merges.
    fn validate_spike_post(&self, slug: &str, tip: &str, candidate: &str, status: Status) -> Result<(), String> {
        if status != Status::Ready {
            return Err(format!("post: task/{slug} is {} — a spike can only be posted while ready", status.as_str()));
        }
        // Only the director's own files, under tasks/<slug>/: nothing outside
        // it (the base must stay `main`'s code), and nothing in the
        // subagent's `claims/` or `findings/`.
        let prefix = format!("tasks/{slug}/");
        for (_, path) in self.hub.diff_names(tip, candidate) {
            let ok = path.strip_prefix(&prefix).is_some_and(|rest| !rest.starts_with("claims/") && !rest.starts_with("findings/"));
            if !ok {
                return Err(format!("post: task/{slug}: a spike's post may only add the director's files under {prefix} (prompt, questions/, baseline/), not {path:?}"));
            }
        }

        let rounds = records::question_rounds(&self.hub, candidate, slug);
        if rounds != vec![1] {
            return Err(format!("post: task/{slug}: a spike's post must add exactly {} (found rounds {rounds:?})", spike::question_path(slug, 1)));
        }
        let question = spike::read_question(&self.hub, candidate, slug, 1).map_err(|e| format!("post: task/{slug}: {e}"))?;
        if question.refines.is_some() {
            return Err(format!("post: task/{slug}: the first question can't refine an earlier round"));
        }

        let baseline = spike::read_baseline(&self.hub, candidate, slug, 1).map_err(|e| format!("post: task/{slug}: {e}"))?;
        for (i, c) in baseline.iter().enumerate() {
            if baseline[..i].iter().any(|o| o.name == c.name) {
                return Err(format!("post: task/{slug}: baseline claim name {:?} is used twice", c.name));
            }
        }
        let failures = spike::failing_at(&self.runner, candidate, &baseline, "on the current code");
        if !failures.is_empty() {
            return Err(format!("post: task/{slug}: baseline claims must pass on the current code\n{}", failures.join("\n")));
        }
        Ok(())
    }

    /// A spike's finish claim (spec §14): a path check, then
    /// `spike::evaluate_finish` runs and reads everything the player will
    /// vouch for. Failures are `claims` strikes. A valid claim is imported,
    /// and the player's verified record of the round goes into the
    /// exploration note on that commit.
    fn finish_spike(&mut self, slug: &str, hub_tip: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        let round = records::spike_round(&self.hub, hub_tip, slug);
        let bad = spike::subagent_path_violations(&self.hub.diff_names(hub_tip, candidate), slug, round);
        if !bad.is_empty() {
            let msg = format!("pins: task/{slug}'s finish claim touches files under tasks/{slug}/ that aren't the subagent's to write: {}", bad.join(", "));
            return self.reject_and_relay(slug, candidate, Reason::Pins, msg, report);
        }

        let Some(base) = records::base_commit(&self.hub, slug) else {
            return Err(format!("task/{slug} is a spike with no recorded base commit"));
        };
        match spike::evaluate_finish(&self.hub, &self.runner, slug, &base, candidate) {
            Err(msg) => {
                let msg = format!("claims: task/{slug}: {msg}");
                self.reject_and_relay(slug, candidate, Reason::Claims, msg, report)
            }
            Ok(ev) => {
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub.update_ref_cas(&hub_ref, candidate, hub_tip).map_err(|e| e.to_string())?;
                records::record_import(&self.hub, candidate, Actor::Subagent, Transition::Finish, Some(slug))?;
                records::record_exploration(
                    &self.hub,
                    candidate,
                    records::ExplorationNote { slug: slug.to_string(), round: ev.round, question: ev.question.question.clone(), answer: ev.answer.clone(), claims: ev.claims, concluded: false, at: 0 },
                )?;
                report.imported.push((hub_ref, candidate.to_string(), Transition::Finish));
                self.sessions.send(slug, &format!("finish accepted — claims verified (answer: {}), awaiting director review; don't commit further", ev.answer));
                Ok(())
            }
        }
    }

    /// A send-back on a tested spike: it adds the next round's question (and
    /// optionally its baseline claims) and nothing else, within
    /// `max_review_rounds`; the new baseline claims must pass on the current
    /// code. It needs no failing test — the new question is the target. The
    /// operator's approval is a process rule the player can't see.
    fn try_spike_send_back(&mut self, slug: &str, tip: &str, candidate: &str, diffs: &[(char, String)], hub_state: &state::HubState, report: &mut TickReport) -> Result<(), String> {
        let n = records::spike_round(&self.hub, tip, slug);
        let next = n + 1;
        let question_file = spike::question_path(slug, next);
        let q_prefix = format!("tasks/{slug}/questions/");

        if !diffs.iter().any(|(st, p)| *st == 'A' && *p == question_file) {
            if !diffs.iter().any(|(st, p)| *st == 'A' && p.starts_with(&q_prefix)) {
                let msg = format!("task/{slug}'s commit doesn't add a question file (expected {question_file})");
                return self.reject(slug, candidate, Reason::Ownership, msg, report);
            }
            let msg = format!("send-back: task/{slug}: expected round {next} to add {question_file}");
            return self.reject(slug, candidate, Reason::Review, msg, report);
        }
        for (st, path) in diffs {
            if *st != 'A' || !spike::send_back_path_ok(path, slug, next) {
                let msg = format!("send-back: task/{slug}: a spike send-back may only add {question_file} and baseline claims for round {next}, not {path:?}");
                return self.reject(slug, candidate, Reason::Review, msg, report);
            }
        }

        let question = match spike::read_question(&self.hub, candidate, slug, next) {
            Ok(q) => q,
            Err(e) => return self.reject(slug, candidate, Reason::Review, format!("send-back: task/{slug}: {e}"), report),
        };
        if let Some(r) = question.refines {
            if r < 1 || r > n {
                let msg = format!("send-back: task/{slug}: {question_file}: refines = {r}, but only rounds 1..={n} exist");
                return self.reject(slug, candidate, Reason::Review, msg, report);
            }
        }
        let baseline = match spike::read_baseline(&self.hub, candidate, slug, next) {
            Ok(b) => b,
            Err(e) => return self.reject(slug, candidate, Reason::Review, format!("send-back: task/{slug}: {e}"), report),
        };
        if let Some(c) = baseline.iter().enumerate().find_map(|(i, c)| baseline[..i].iter().any(|o| o.name == c.name).then_some(c)) {
            let msg = format!("send-back: task/{slug}: baseline claim name {:?} is used twice", c.name);
            return self.reject(slug, candidate, Reason::Review, msg, report);
        }

        // The Nth question is the (N-1)th send-back's; spec §10's cap counts
        // send-backs, so round `n` here is send-back number `n`.
        if n > hub_state.roadmap.limits.max_review_rounds {
            let cause = format!("review rounds exceeded: {n} > {}", hub_state.roadmap.limits.max_review_rounds);
            records::record_escalation(&self.hub, slug, candidate, &cause)?;
            report.escalated.push((slug.to_string(), cause));
            return Ok(());
        }

        if !baseline.is_empty() {
            let Some(base) = records::base_commit(&self.hub, slug) else {
                return Err(format!("task/{slug} is a spike with no recorded base commit"));
            };
            let overlay = spike::overlay_commit(&self.hub, &base, candidate, slug)?;
            let failures = spike::failing_at(&self.runner, &overlay, &baseline, "on the current code");
            if !failures.is_empty() {
                let msg = format!("send-back: task/{slug}: baseline claims must pass on the current code\n{}", failures.join("\n"));
                return self.reject(slug, candidate, Reason::Review, msg, report);
            }
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub.update_ref_cas(&hub_ref, candidate, tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, Actor::Director, Transition::SendBack, Some(slug))?;
        report.imported.push((hub_ref, candidate.to_string(), Transition::SendBack));

        records::set_pins(&self.hub, slug, candidate)?;
        let raw = self.hub.show_file(candidate, &question_file).unwrap_or_default();
        let header = format!(
            "Round {next}: the director sent your spike back with a new question. Fetch it: git pull --ff-only origin task/{slug}. \
Answer it in one new commit adding {} and {}.",
            spike::findings_path(slug, next),
            spike::claims_path(slug, next)
        );
        self.sessions.send(slug, &format!("{header}\n\n{}", spike::question_block(next, &raw)));
        Ok(())
    }

    /// `conclude:` on a tested spike: the branch moves to
    /// `refs/explored/<slug>` (it never merges), the player records the final
    /// answer, and the session ends. The answer is the one the player
    /// verified at the finish; the director and operator agreeing with it is
    /// their conversation.
    fn try_conclude(&mut self, slug: &str, tip: &str, candidate: &str, status: Option<Status>, report: &mut TickReport) -> Result<(), String> {
        if !records::is_spike_at(&self.hub, tip, slug) {
            let msg = format!("conclude: task/{slug} is not a spike — an ordinary task is finished with `accept:`");
            return self.reject(slug, candidate, Reason::Ownership, msg, report);
        }
        if status != Some(Status::Tested) {
            let msg = format!("conclude: task/{slug} is {} — a spike needs a validated finish before it can be concluded", status.map(|s| s.as_str()).unwrap_or("unknown"));
            return self.reject(slug, candidate, Reason::Accept, msg, report);
        }
        let Some(note) = records::exploration(&self.hub, tip) else {
            let msg = format!("conclude: task/{slug} has no verified answer recorded at its tip");
            return self.reject(slug, candidate, Reason::Accept, msg, report);
        };

        self.hub.update_ref(&format!("refs/explored/{slug}"), candidate).map_err(|e| e.to_string())?;
        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub.delete_ref_cas(&hub_ref, tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, Actor::Director, Transition::Conclude, Some(slug))?;
        records::record_exploration(&self.hub, candidate, records::ExplorationNote { concluded: true, ..note })?;
        report.imported.push((format!("refs/explored/{slug}"), candidate.to_string(), Transition::Conclude));

        self.end_task_sessions(slug);
        Ok(())
    }

    // ---- retire (spec §9) ----

    /// spec §9's retire row: allowed only when the task isn't posted or in
    /// flight (pending/ready/orphaned), or is escalated. Moves the branch to
    /// `refs/retired/<slug>` and ends the subagent session.
    fn try_retire(&mut self, slug: &str, tip: &str, candidate: &str, status: Option<Status>, report: &mut TickReport) -> Result<(), String> {
        // A spike the operator gives up on may also be retired once its
        // finish is validated (spec §14); an ordinary tested task may not.
        let tested_spike = status == Some(Status::Tested) && records::is_spike_at(&self.hub, tip, slug);
        let allowed = tested_spike || matches!(status, Some(Status::Pending | Status::Ready | Status::Orphaned | Status::Conflicted | Status::Contested | Status::Escalated));
        if !allowed {
            let msg = format!(
                "retire: task/{slug} is {} — retire is only allowed while pending/ready/orphaned (not posted or in flight), conflicted, contested or escalated",
                status.map(|s| s.as_str()).unwrap_or("unknown")
            );
            return self.reject(slug, candidate, Reason::Retire, msg, report);
        }

        self.hub.update_ref(&format!("refs/retired/{slug}"), candidate).map_err(|e| e.to_string())?;
        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub.delete_ref_cas(&hub_ref, tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, Actor::Director, Transition::Retire, Some(slug))?;
        report.imported.push((format!("refs/retired/{slug}"), candidate.to_string(), Transition::Retire));

        self.end_task_sessions(slug);
        Ok(())
    }

    fn try_replan(&mut self, slug: &str, tip: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        match self.validate_replan(slug, tip, candidate) {
            Ok(()) => {
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub.update_ref_cas(&hub_ref, candidate, tip).map_err(|e| e.to_string())?;
                records::record_import(&self.hub, candidate, Actor::Director, Transition::Replan, Some(slug))?;
                report.imported.push((hub_ref, candidate.to_string(), Transition::Replan));
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
            return Err(format!("plan: task/{slug}'s replan commits may only change {expected_path}"));
        }
        let text = self.hub.show_file(candidate, &expected_path).ok_or_else(|| format!("plan: {expected_path} missing at {candidate}"))?;
        let cfg = config::task_config_from_str(&text).map_err(|e| format!("plan: {expected_path}: {e}"))?;
        let main_tip = self.hub.rev_parse("refs/heads/main").ok_or_else(|| "hub has no main".to_string())?;
        self.validate_task_config(slug, &main_tip, &cfg)
    }

    /// Shared by plan and replan: phase, dependency existence/retirement/
    /// self-reference, and the no-cycles rule over the hub's task graph plus
    /// this candidate.
    fn validate_task_config(&self, slug: &str, main_tip: &str, cfg: &TaskConfig) -> Result<(), String> {
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
            if records::is_explored(&self.hub, dep) || self.task_config_at(&accepted, dep).is_some_and(|c| c.kind == TaskKind::Spike) {
                return Err(format!("plan: task/{slug} depends on spike {dep:?}: nothing may depend on a spike"));
            }
            let exists = self.hub.ref_exists(&format!("refs/heads/task/{dep}")) || accepted.contains_key(dep);
            if !exists {
                return Err(format!("plan: task/{slug} depends on unknown task {dep:?}"));
            }
        }

        if self.creates_cycle(&accepted, slug, &cfg.depends_on) {
            return Err(format!("plan: task/{slug}'s dependencies contain a cycle"));
        }
        Ok(())
    }

    /// The dependency graph reachable from `deps` (as if they belonged to
    /// `slug`), read from each dependency's live branch tip or (once
    /// accepted) `main`. True if that graph reaches `slug` again.
    fn creates_cycle(&self, accepted: &BTreeMap<String, String>, slug: &str, deps: &[String]) -> bool {
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

    fn task_config_at(&self, accepted: &BTreeMap<String, String>, slug: &str) -> Option<TaskConfig> {
        let tip = self.hub.rev_parse(&format!("refs/heads/task/{slug}")).or_else(|| accepted.get(slug).cloned())?;
        let text = self.hub.show_file(&tip, &format!("tasks/{slug}/task.toml"))?;
        config::task_config_from_str(&text).ok()
    }

    fn roadmap_at(&self, commit: &str) -> Result<RoadmapConfig, String> {
        let text = self.hub.show_file(commit, "roadmap.toml").ok_or_else(|| format!("roadmap.toml not found at {commit}"))?;
        config::roadmap_config_from_str(&text)
    }

    // ---- launch (spec §8.3, §9's launch row) ----

    /// Every `Posted` task that has never been launched (no
    /// `refs/dashboard/pins/<slug>` yet — pins are only ever taken at launch
    /// or send-back, and a `Posted` task hasn't been sent back) gets a
    /// subagent clone and a tmux session. A launch failure never retries
    /// itself: whatever went wrong, this method always records *some* trace
    /// of the attempt (pins, at least) before returning, so the task isn't
    /// silently re-attempted every tick forever (spec's deferred "not
    /// relaunched automatically").
    fn launch_subagents(&mut self, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let candidates: Vec<(String, String)> =
            hub_state.tasks.iter().filter(|t| t.status == Status::Posted && !records::has_pins(&self.hub, &t.slug)).map(|t| (t.slug.clone(), t.tip.clone())).collect();
        for (slug, tip) in candidates {
            self.try_launch(&slug, &tip, report)?;
        }
        Ok(())
    }

    fn try_launch(&mut self, slug: &str, tip: &str, report: &mut TickReport) -> Result<(), String> {
        let is_spike = records::is_spike_at(&self.hub, tip, slug);
        if is_spike {
            // A spike has no task check; what must still hold at launch is
            // the director's baseline claims on the current code.
            let round = records::spike_round(&self.hub, tip, slug);
            let baseline = spike::read_baseline(&self.hub, tip, slug, round).map_err(|e| format!("launch: task/{slug}: {e}"))?;
            let failures = spike::failing_at(&self.runner, tip, &baseline, "on the current code");
            if !failures.is_empty() {
                report.errors.push(format!("launch: task/{slug}: the director's baseline claims no longer pass at the tip — not launching\n{}", failures.join("\n")));
                return Ok(());
            }
        } else {
            let result = self.runner.run_task_check(tip, slug).map_err(|e| format!("launch: task/{slug}: {e}"))?;
            if result.pass {
                report.errors.push(format!("launch: task/{slug}: the task check now passes at the tip — not launching a subagent for work that's already done"));
                return Ok(());
            }
            if let Some(miss) = self.evidence_miss(slug, tip) {
                return self.auto_retire(slug, tip, &miss, report);
            }
        }

        // Pins are taken before anything else can fail, so a launch that
        // goes wrong from here on is never silently retried next tick.
        records::set_pins(&self.hub, slug, tip)?;
        if is_spike {
            records::set_base(&self.hub, slug, tip)?;
        }

        let clone_dir = paths::subagent_clone(&self.director, slug);
        if clone_dir.exists() {
            report.errors.push(format!("launch: task/{slug}: {} already exists — refusing to touch a leftover agent clone (spec §5)", clone_dir.display()));
            return Ok(());
        }
        if let Err(e) = self.create_subagent_clone(slug, &clone_dir) {
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
            Ok(agent_cmd) => match self.sessions.launch(slug, &clone_dir, &agent_cmd, &prompt_file) {
                Ok(()) => report.launched.push(slug.to_string()),
                Err(e) => report.errors.push(format!("launch: task/{slug}: {e}")),
            },
            Err(e) => report.errors.push(format!("launch: task/{slug}: {e}")),
        }
        Ok(())
    }

    fn agent_cmd_at_main(&self) -> Result<String, String> {
        let main_tip = self.hub.rev_parse("refs/heads/main").ok_or_else(|| "hub has no main".to_string())?;
        Ok(self.roadmap_at(&main_tip)?.player.agent_cmd)
    }

    /// Ensures the project's one, permanent subagent workspace exists (a
    /// `git clone` of the hub — `paths::subagent_workspace` — created once
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
    fn create_subagent_clone(&self, slug: &str, clone_dir: &Path) -> Result<(), String> {
        self.add_worktree(&format!("task/{slug}"), slug, clone_dir)
    }

    /// `create_subagent_clone`'s mechanics for any agent: a worktree of the
    /// shared workspace at `clone_dir`, on the local branch `branch`, started
    /// at the hub's `task/<slug>`.
    fn add_worktree(&self, branch: &str, slug: &str, clone_dir: &Path) -> Result<(), String> {
        let workspace_dir = paths::subagent_workspace(&self.director);
        if !workspace_dir.join(".git").exists() {
            if let Some(parent) = workspace_dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let hub_str = path_str(&self.hub.dir)?;
            let workspace_str = path_str(&workspace_dir)?;
            let cwd_git = Git::new(workspace_dir.parent().unwrap_or_else(|| Path::new(".")));
            cwd_git.run(&["clone", "--quiet", hub_str, workspace_str]).map_err(|e| format!("git clone (subagent workspace): {e}"))?;
        }

        let workspace = Git::new(&workspace_dir);
        workspace.run(&["fetch", "--quiet", "origin"]).map_err(|e| format!("fetching hub into the subagent workspace: {e}"))?;

        if let Some(parent) = clone_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let clone_str = path_str(clone_dir)?;
        workspace
            .run(&["worktree", "add", "--quiet", "-B", branch, clone_str, &format!("origin/task/{slug}")])
            .map_err(|e| format!("git worktree add: {e}"))?;
        Ok(())
    }

    /// The real, private git-admin directory for `clone_dir`. For a plain
    /// clone this is just `<clone_dir>/.git`, but a worktree's `.git` is a
    /// *file* (pointing at `<workspace>/.git/worktrees/<name>`), so it can't
    /// be joined into further like a directory — this asks git for the real
    /// path instead of assuming the plain-clone shape.
    fn git_dir(clone_dir: &Path) -> Result<PathBuf, String> {
        let out = Git::new(clone_dir).run(&["rev-parse", "--git-dir"]).map_err(|e| e.to_string())?;
        let dir = PathBuf::from(out);
        Ok(if dir.is_absolute() { dir } else { clone_dir.join(dir) })
    }

    /// Removes `slug`'s subagent worktree and its local branch in the
    /// shared workspace repo (`paths::subagent_workspace`), once its task
    /// is finished (accepted or retired). The hub's own `refs/heads/
    /// task/<slug>` (or `refs/retired/<slug>`) stays forever as the
    /// permanent record (spec §8.1) — the worktree was only ever scratch
    /// space for the subagent to work in, so nothing is lost by reclaiming
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
    fn cleanup_subagent_clone(&self, slug: &str) {
        self.remove_worktree(&paths::subagent_clone(&self.director, slug), &format!("task/{slug}"));
    }

    /// Ends every session a finished task (accepted, retired, concluded) still
    /// has, its subagent's and its task agent's (spec §17), and removes both
    /// worktrees.
    fn end_task_sessions(&mut self, slug: &str) {
        self.sessions.kill(slug);
        self.cleanup_subagent_clone(slug);
        if records::agent_launched(&self.hub, slug) {
            self.sessions.kill(&state::agent_key(slug));
            self.remove_worktree(&paths::agent_clone(&self.director, slug), &format!("agent/{slug}"));
        }
    }

    /// `cleanup_subagent_clone`'s mechanics for any agent's worktree and its
    /// local branch.
    fn remove_worktree(&self, clone_dir: &Path, branch: &str) {
        if !clone_dir.exists() {
            return;
        }
        let workspace_dir = paths::subagent_workspace(&self.director);
        if !workspace_dir.join(".git").exists() {
            return;
        }
        let workspace = Git::new(&workspace_dir);
        let Ok(clone_str) = path_str(clone_dir) else {
            eprintln!("dashboard: worktree {} is not valid UTF-8, leaving it in place", clone_dir.display());
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
    /// committed by the subagent.
    fn write_prompt_file(&self, slug: &str, tip: &str, prompt_file: &Path) -> Result<(), String> {
        let prompt_body = self.hub.show_file(tip, &format!("tasks/{slug}/prompt.md")).ok_or_else(|| format!("tasks/{slug}/prompt.md missing at {tip}"))?;
        let pinned = records::pinned_files(&self.hub, slug).unwrap_or_default();
        let pinned_paths: Vec<&str> = pinned.keys().map(|s| s.as_str()).collect();
        let content = if records::is_spike_at(&self.hub, tip, slug) {
            let round = records::spike_round(&self.hub, tip, slug);
            let question = self.hub.show_file(tip, &spike::question_path(slug, round)).unwrap_or_default();
            format!("{}\n\n---\n\n{prompt_body}\n\n{}", spike::spike_preamble(slug, &self.project, &pinned_paths), spike::question_block(round, &question))
        } else {
            let mut preamble = launch_preamble(slug, &self.project, &pinned_paths);
            if let Some(cfg) = records::task_config_at(&self.hub, tip, slug).filter(|c| c.kind == TaskKind::FlakyTestFix) {
                preamble = format!("{preamble}\n\n{}", flaky::preamble(&cfg.fix_paths, &cfg.replace));
            }
            format!("{preamble}\n\n---\n\n{prompt_body}")
        };
        std::fs::write(prompt_file, content).map_err(|e| format!("{}: {e}", prompt_file.display()))
    }

    /// Relaunches `slug`'s subagent session with the same clone and prompt
    /// file it already has (spec's deferred "not relaunched automatically" —
    /// this is the manual trigger, exposed as `dashboard relaunch <slug>`).
    /// Only valid once pins exist (the task was actually launched before),
    /// the session isn't already alive, and the task is still in a state the
    /// subagent may work on.
    pub fn relaunch(&mut self, slug: &str) -> Result<(), String> {
        if !records::has_pins(&self.hub, slug) {
            return Err(format!("relaunch: task/{slug} has no pins — it was never launched"));
        }
        if self.sessions.alive(slug) {
            return Err(format!("relaunch: task/{slug}'s session is already alive"));
        }
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let status = hub_state.tasks.iter().find(|t| t.slug == slug).map(|t| t.status);
        if !matches!(status, Some(Status::Posted | Status::InProgress | Status::InRevision | Status::Contested)) {
            return Err(format!(
                "relaunch: task/{slug} is {} — relaunch only applies while posted, in progress, or in revision",
                status.map(|s| s.as_str()).unwrap_or("unknown")
            ));
        }

        let clone_dir = paths::subagent_clone(&self.director, slug);
        if !clone_dir.exists() {
            return Err(format!("relaunch: task/{slug}'s clone {} is missing", clone_dir.display()));
        }
        let prompt_file = Self::git_dir(&clone_dir)?.join("dashboard-prompt.md");
        if !prompt_file.exists() {
            return Err(format!("relaunch: task/{slug}'s prompt file {} is missing", prompt_file.display()));
        }

        let agent_cmd = self.agent_cmd_at_main()?;
        self.sessions.launch(slug, &clone_dir, &agent_cmd, &prompt_file)
    }

    // ---- subagent import (spec §5, §7, §9) ----

    /// Every task with pins whose status is `Posted`/`InProgress`/
    /// `InRevision` and whose clone dir exists gets a fetch-and-validate
    /// pass. A missing clone (never launched, or a relaunch pending) is
    /// simply skipped — there's nothing to fetch from.
    fn import_subagents(&mut self, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let candidates: Vec<(String, String)> = hub_state
            .tasks
            .iter()
            .filter(|t| matches!(t.status, Status::Posted | Status::InProgress | Status::InRevision) && records::has_pins(&self.hub, &t.slug))
            .map(|t| (t.slug.clone(), t.tip.clone()))
            .collect();

        for (slug, hub_tip) in candidates {
            let clone_dir = paths::subagent_clone(&self.director, &slug);
            if !clone_dir.exists() {
                continue;
            }
            self.import_subagent(&slug, &hub_tip, &clone_dir, report)?;
        }
        Ok(())
    }

    /// `git fetch --no-tags <clone> +refs/heads/task/<slug>:refs/incoming/subagent/<slug>`,
    /// then classifies the result exactly as spec's "Import semantics"
    /// describes: behind/caught-up is a no-op, diverged is a `history`
    /// rejection, a merge commit anywhere in the new range is a `history`
    /// rejection, and otherwise only the newest commit (the finish claim) is
    /// validated as a cumulative change from `hub_tip`.
    fn import_subagent(&mut self, slug: &str, hub_tip: &str, clone_dir: &Path, report: &mut TickReport) -> Result<(), String> {
        let clone_str = path_str(clone_dir)?;
        let incoming_ref = format!("refs/incoming/subagent/{slug}");
        let refspec = format!("+refs/heads/task/{slug}:{incoming_ref}");
        if self.hub.run(&["fetch", "--no-tags", clone_str, &refspec]).is_err() {
            return Ok(()); // the subagent hasn't pushed a commit yet, or the branch is missing
        }
        let incoming = match self.hub.rev_parse(&incoming_ref) {
            Some(sha) => sha,
            None => return Ok(()),
        };

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

        let commits = self.hub.rev_list(&[&format!("{hub_tip}..{incoming}"), "--reverse"]);
        if commits.iter().any(|c| self.hub.parents(c).len() > 1) {
            let msg = format!("history: task/{slug}'s clone contains a merge commit; only the player creates merges");
            self.reject_and_relay(slug, &incoming, Reason::History, msg, report)?;
            return Ok(());
        }

        if records::is_rejected(&self.hub, &incoming) {
            return Ok(());
        }

        self.validate_finish_claim(slug, hub_tip, &incoming, report)
    }

    /// `candidate` (the newest commit fetched, `C` in spec's notation) is
    /// either a block marker, a stray empty commit, or a finish claim —
    /// dispatched exactly as spec's subagent-import bullets describe.
    fn validate_finish_claim(&mut self, slug: &str, hub_tip: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        if self.hub.is_empty_commit(candidate) {
            let subject = self.hub.commit_subject(candidate).unwrap_or_default();
            if subject.starts_with("block:") {
                if !self.hub.diff_names(hub_tip, candidate).is_empty() {
                    let msg = format!("history: task/{slug}'s block: commit isn't a clean reset to the hub tip — reset to hub, then block");
                    return self.reject_and_relay(slug, candidate, Reason::History, msg, report);
                }
                let hub_ref = format!("refs/heads/task/{slug}");
                self.hub.update_ref_cas(&hub_ref, candidate, hub_tip).map_err(|e| e.to_string())?;
                records::record_import(&self.hub, candidate, Actor::Subagent, Transition::Block, Some(slug))?;
                report.imported.push((hub_ref, candidate.to_string(), Transition::Block));
                return Ok(());
            }
            if subject.starts_with("contest:") {
                return self.try_contest(slug, hub_tip, candidate, report);
            }
            let msg = "suite: empty commit is neither a finish claim nor a block:/contest: marker".to_string();
            return self.reject_and_relay(slug, candidate, Reason::Suite, msg, report);
        }

        let pins_violations = records::check_pins(&self.hub, slug, candidate);
        if !pins_violations.is_empty() {
            let msg = format!("pins: task/{slug}'s finish claim changed or removed pinned files (or added a tests.d/ suite fragment): {}", pins_violations.join(", "));
            return self.reject_and_relay(slug, candidate, Reason::Pins, msg, report);
        }

        if records::is_spike_at(&self.hub, hub_tip, slug) {
            return self.finish_spike(slug, hub_tip, candidate, report);
        }

        if let Some(stray) = self.flaky_scope_violations(slug, hub_tip, candidate) {
            let msg = format!(
                "pins: task/{slug} is a flaky-test-fix and may only change its fix_paths (or add new test files), not {}. If the flake is in the product code, make a `block:` commit instead: the director replans it as an ordinary task",
                stray.join(", ")
            );
            return self.reject_and_relay(slug, candidate, Reason::Pins, msg, report);
        }

        let suite = self.runner.run_suite(candidate);
        self.note_flakes(slug, candidate, &suite);
        if !suite.pass {
            let msg = format!("suite: task/{slug}: {}", suite.summary());
            return self.reject_and_relay(slug, candidate, Reason::Suite, msg, report);
        }

        if let Err(problem) = self.validate_flaky_finish(slug, hub_tip, candidate) {
            let msg = format!("suite: task/{slug}: {problem}");
            return self.reject_and_relay(slug, candidate, Reason::Suite, msg, report);
        }

        let hub_ref = format!("refs/heads/task/{slug}");
        self.hub.update_ref_cas(&hub_ref, candidate, hub_tip).map_err(|e| e.to_string())?;
        records::record_import(&self.hub, candidate, Actor::Subagent, Transition::Finish, Some(slug))?;
        report.imported.push((hub_ref, candidate.to_string(), Transition::Finish));
        self.sessions.send(slug, "finish accepted — tests passed, awaiting director review; don't commit further");
        Ok(())
    }

    /// Files every check in `suite` that passed only on a retry into the
    /// flake ledger under `scope`, so the director can plan a hygiene task
    /// with evidence. Best effort: a ledger write never fails a transition.
    fn note_flakes(&self, scope: &str, commit: &str, suite: &checks::SuiteResult) {
        for c in &suite.checks {
            let _ = records::record_flake(&self.hub, scope, commit, c);
        }
    }

    /// For a flaky-test-fix: the paths `candidate` changes outside the task's
    /// `fix_paths`, other than brand-new files under `test_paths`. A flake
    /// whose cause is in the product code isn't this kind of task (it takes
    /// the regular path), so touching anything else is refused. `None` for
    /// any other task, or when nothing strays.
    fn flaky_scope_violations(&self, slug: &str, hub_tip: &str, candidate: &str) -> Option<Vec<String>> {
        let cfg = records::task_config_at(&self.hub, hub_tip, slug).filter(|c| c.kind == TaskKind::FlakyTestFix)?;
        let test_paths = self.hub.show_file(hub_tip, "tests.toml").and_then(|t| config::tests_config_from_str(&t).ok()).map(|c| c.test_paths).unwrap_or_default();
        let stray: Vec<String> = self
            .hub
            .diff_names(hub_tip, candidate)
            .into_iter()
            .filter(|(status, path)| !cfg.fix_paths.contains(path) && !(*status == 'A' && path_under_test_paths(path, &test_paths)))
            .map(|(_, path)| path)
            .collect();
        (!stray.is_empty()).then_some(stray)
    }

    /// A flaky-test-fix's finish, on top of the passing suite: the director's
    /// task check passes (the player, not the subagent, decides the new test
    /// is stable), no test but the replaced ones vanished since the pinned
    /// base, and every replacement exists. Always `Ok` for other tasks.
    fn validate_flaky_finish(&self, slug: &str, hub_tip: &str, candidate: &str) -> Result<(), String> {
        let Some(cfg) = records::task_config_at(&self.hub, hub_tip, slug).filter(|c| c.kind == TaskKind::FlakyTestFix) else { return Ok(()) };

        let check = self.runner.run_task_check(candidate, slug)?;
        if !check.pass {
            let passed = check.exits.iter().filter(|&&e| e == 0).count();
            let tail = check.output_tail.as_deref().map(|t| format!("\n{t}")).unwrap_or_default();
            return Err(format!("the stability check {:?} fails ({passed}/{} runs passed){tail}", check.name, check.runs));
        }

        let list_cmd = self.hub.show_file(candidate, "tests.toml").and_then(|t| config::tests_config_from_str(&t).ok()).and_then(|c| c.list_cmd).ok_or("tests.toml has no list_cmd")?;
        let base = self.hub.rev_parse(&format!("refs/dashboard/pins/{slug}")).ok_or("no pins recorded")?;
        let before = self.runner.run_list(&base, &list_cmd)?;
        let after = self.runner.run_list(candidate, &list_cmd)?;
        let problems = flaky::inventory_problems(&before, &after, &cfg.replace);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("\n"))
        }
    }

    /// Shared by every subagent-side rejection: records it, then re-derives
    /// state and relays a recovery message to the subagent's session unless
    /// this very rejection just escalated the task (spec §10: "the player
    /// stops relaying"). Director rejections never go through here — only
    /// `reject` (no relay), per spec.
    fn reject_and_relay(&mut self, slug: &str, candidate: &str, reason: Reason, msg: String, report: &mut TickReport) -> Result<(), String> {
        let rec = records::record_rejection(&self.hub, slug, candidate, reason, Actor::Subagent, &msg)?;
        report.rejected.push(rec);

        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let escalated = hub_state.tasks.iter().any(|t| t.slug == slug && t.status == Status::Escalated);
        if escalated {
            return Ok(());
        }

        let recovery = match reason {
            Reason::History => format!("run `git fetch origin && git reset --hard origin/task/{slug}`, then redo your work and commit"),
            _ => "fix it with a new commit on top".to_string(),
        };
        self.sessions.send(slug, &format!("{msg}\n\nRecovery: {recovery}"));
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
            .filter(|t| t.agent.is_some() && !matches!(t.status, Status::Accepted | Status::Retired | Status::Explored))
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

    /// `import_subagent`'s shape for a task agent's clone. One difference: the
    /// hub's task branch can move under a commit the player hasn't imported
    /// yet (`main` merged into a ready task, a replan). That isn't the
    /// agent's doing, so it's asked to rebase, once per commit, with no
    /// strike. Rewriting a commit the player already imported from it is
    /// `history`, as for anyone.
    fn import_task_agent(&mut self, slug: &str, hub_tip: &str, clone_dir: &Path, report: &mut TickReport) -> Result<(), String> {
        let clone_str = path_str(clone_dir)?;
        let incoming_ref = format!("refs/incoming/agent/{slug}");
        if self.hub.run(&["fetch", "--no-tags", clone_str, &format!("+refs/heads/agent/{slug}:{incoming_ref}")]).is_err() {
            return Ok(());
        }
        let Some(incoming) = self.hub.rev_parse(&incoming_ref) else { return Ok(()) };
        if self.hub.is_ancestor(&incoming, hub_tip) || records::is_rejected(&self.hub, &incoming) {
            return Ok(());
        }
        if !self.hub.is_ancestor(hub_tip, &incoming) {
            let base = self.hub.run(&["merge-base", hub_tip, &incoming]).unwrap_or_default();
            let rewrote_imported = base.is_empty()
                || self.hub.rev_list(&[&format!("{base}..{hub_tip}")]).iter().any(|c| records::import_note(&self.hub, c).is_some_and(|n| n.actor == Actor::TaskAgent));
            if rewrote_imported {
                let msg = format!("history: agent/{slug} diverged from the hub: a commit the player imported from you was rewritten");
                return self.reject_by(Actor::TaskAgent, slug, &incoming, Reason::History, msg, report);
            }
            if self.agent_rebase_told.get(slug) != Some(&incoming) {
                self.sessions.send(
                    &state::agent_key(slug),
                    &format!(
                        "task/{slug} moved on the hub under your commit (main merged in, or a replan). Nothing was rejected: \
run git fetch origin && git rebase origin/task/{slug}, re-run the task's check if your post is affected, and leave the result committed."
                    ),
                );
                self.agent_rebase_told.insert(slug.to_string(), incoming);
            }
            return Ok(());
        }
        if self.hub.rev_list(&[&format!("{hub_tip}..{incoming}")]).iter().any(|c| self.hub.parents(c).len() > 1) {
            let msg = format!("history: agent/{slug} contains a merge commit; only the player creates merges");
            return self.reject_by(Actor::TaskAgent, slug, &incoming, Reason::History, msg, report);
        }
        self.try_agent_commit(slug, hub_tip, &incoming, report)
    }

    /// The newest commit a task agent made, validated as one cumulative
    /// change from the hub tip: its post while the task is ready, a
    /// send-back while it's tested. Either may touch only tests, the suite
    /// files and `tasks/<slug>/` (with `[check]` the only part of `task.toml`
    /// it may change), and it never commits a marker (spec §17).
    fn try_agent_commit(&mut self, slug: &str, tip: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let Some(task) = hub_state.tasks.iter().find(|t| t.slug == slug) else { return Ok(()) };
        let status = task.status;

        if self.hub.is_empty_commit(candidate) {
            let msg = format!("ownership: task/{slug}: a task agent never commits a marker; accept:/retire: are the director's, so recommend in your report instead");
            return self.reject_by(Actor::TaskAgent, slug, candidate, Reason::Ownership, msg, report);
        }
        let diffs = self.hub.diff_names(tip, candidate);
        if let Err(msg) = self.agent_paths_ok(slug, tip, candidate, &diffs) {
            return self.reject_by(Actor::TaskAgent, slug, candidate, Reason::Ownership, msg, report);
        }

        let step = match status {
            Status::Pending | Status::Ready | Status::Orphaned => {
                if self.hub.blob_hash(candidate, &format!("tasks/{slug}/prompt.md")).is_none() {
                    let msg = format!("ownership: task/{slug} isn't posted yet and this commit adds no tasks/{slug}/prompt.md: your only commit now is the post");
                    return self.reject_by(Actor::TaskAgent, slug, candidate, Reason::Ownership, msg, report);
                }
                self.try_post(slug, tip, candidate, status, &hub_state, Actor::TaskAgent, report)?;
                "post"
            }
            Status::Tested if task.spike.is_none() => {
                self.try_send_back(slug, tip, candidate, &diffs, &hub_state, Actor::TaskAgent, report)?;
                "send-back"
            }
            Status::Contested => {
                self.try_contest_reply(slug, tip, candidate, &diffs, &hub_state, Actor::TaskAgent, report)?;
                "contest reply"
            }
            other => {
                let msg = format!("ownership: task/{slug} is {}: there is nothing for its task agent to commit now", other.as_str());
                return self.reject_by(Actor::TaskAgent, slug, candidate, Reason::Ownership, msg, report);
            }
        };

        let key = state::agent_key(slug);
        if report.imported.iter().any(|(_, sha, _)| sha == candidate) {
            let next = match step {
                "post" => "the subagent is launched",
                "send-back" => "the review is relayed to the subagent",
                _ => "your reply is relayed to the subagent",
            };
            self.sessions.send(
                &key,
                &format!("The player imported your {step} at {}; {next}. Write your report if you haven't yet, then wait: the player pastes the next review request here.", short(candidate)),
            );
        } else if let Some((_, cause)) = report.escalated.iter().find(|(s, _)| s == slug) {
            self.sessions.send(&key, &format!("task/{slug} is escalated ({cause}). Stop here: the director decides with the operator and may ask you about it."));
        }
        Ok(())
    }

    /// Where a task agent may write: `tests.toml`, `tests.d/`, the hub's
    /// `test_paths` (read at `tip`, so a commit can't widen them for itself)
    /// and `tasks/<slug>/`; and in `task.toml` only `[check]`, the rest
    /// being the director's plan.
    fn agent_paths_ok(&self, slug: &str, tip: &str, candidate: &str, diffs: &[(char, String)]) -> Result<(), String> {
        let test_paths = self.hub.show_file(tip, "tests.toml").and_then(|t| config::tests_config_from_str(&t).ok()).map(|c| c.test_paths).unwrap_or_default();
        let task_prefix = format!("tasks/{slug}/");
        for (_, path) in diffs {
            if !(path == "tests.toml" || config::is_suite_fragment(path) || path.starts_with(&task_prefix) || path_under_test_paths(path, &test_paths)) {
                return Err(format!("ownership: task/{slug}: a task agent writes tests, the prompt and reviews, never {path:?}: the code is the subagent's"));
            }
        }
        let toml_path = format!("tasks/{slug}/task.toml");
        if diffs.iter().any(|(_, p)| *p == toml_path) {
            let plan = |c: &str| {
                self.hub.show_file(c, &toml_path).and_then(|t| toml::from_str::<toml::Table>(&t).ok()).map(|mut t| {
                    t.remove("check");
                    t
                })
            };
            if plan(tip) != plan(candidate) {
                return Err(format!("ownership: task/{slug}: in task.toml a task agent may only set [check]; the rest is the director's plan"));
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
                Status::Ready => Some(AgentStep::Post),
                Status::Tested => Some(AgentStep::Review),
                Status::Contested => Some(AgentStep::Contest),
                _ => None,
            };
            if let Some(step) = step.filter(|_| state::task_agent_managed(&hub_state.roadmap, &task.config)) {
                if task.agent.is_none() {
                    self.launch_task_agent(task, step, &hub_state.roadmap, report)?;
                } else if step != AgentStep::Post && records::agent_asked(&self.hub, &task.slug).as_deref() != Some(task.tip.as_str()) {
                    records::set_agent_asked(&self.hub, &task.slug, &task.tip)?;
                    let text = self.agent_step_text(task, step, &hub_state.roadmap);
                    self.sessions.send(&state::agent_key(&task.slug), &text);
                }
            }
            if !matches!(task.status, Status::Accepted | Status::Retired | Status::Explored) {
                self.collect_agent_report(&task.slug, &task.tip, report)?;
            }
        }
        Ok(())
    }

    fn launch_task_agent(&mut self, task: &state::TaskState, step: AgentStep, roadmap: &RoadmapConfig, report: &mut TickReport) -> Result<(), String> {
        let slug = &task.slug;
        // Recorded first, so a launch that fails below is never retried
        // every tick (as for subagents; `dashboard relaunch --agent`).
        records::set_agent_launched(&self.hub, slug, &task.tip)?;
        if step != AgentStep::Post {
            records::set_agent_asked(&self.hub, slug, &task.tip)?;
        }

        let dir = paths::agent_clone(&self.director, slug);
        if dir.exists() {
            report.errors.push(format!("task agent: task/{slug}: {} already exists — refusing to touch a leftover agent clone", dir.display()));
            return Ok(());
        }
        if let Err(e) = self.add_worktree(&format!("agent/{slug}"), slug, &dir) {
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
        match self.sessions.launch(&state::agent_key(slug), &dir, &roadmap.player.task_agent_cmd, &prompt_file) {
            Ok(()) => report.agents_launched.push(slug.clone()),
            Err(e) => report.errors.push(format!("task agent: task/{slug}: {e}")),
        }
        Ok(())
    }

    /// The task agent's prompt: the player's preamble (its role and rules,
    /// the task, the report format), then the step it starts on.
    fn write_agent_prompt(&self, task: &state::TaskState, step: AgentStep, roadmap: &RoadmapConfig, dir: &Path, prompt_file: &Path, relaunched: bool) -> Result<(), String> {
        let report_file = Self::git_dir(dir)?.join("dashboard-report.md");
        let toml = self.hub.show_file(&task.tip, &format!("tasks/{}/task.toml", task.slug)).unwrap_or_default();
        let goal = roadmap.phases.iter().find(|p| p.id == task.config.phase).and_then(|p| p.goal.clone()).unwrap_or_default();
        let mut text = agent_preamble(&task.slug, &self.project, &task.config, &toml, &goal, &report_file);
        if relaunched {
            text.push_str(&format!(
                "\n\nYou were relaunched: an earlier session's context is gone. Rebuild what you need from the repo: tasks/{}/ (task.toml, prompt.md, reviews/) on origin/task/{} and its history.",
                task.slug, task.slug
            ));
        }
        text.push_str(&format!("\n\n---\n\n{}", self.agent_step_text(task, step, roadmap)));
        std::fs::write(prompt_file, text).map_err(|e| format!("{}: {e}", prompt_file.display()))
    }

    /// What the task agent is asked to do now: the post, a review round, or
    /// nothing until the player pastes a step.
    fn agent_step_text(&self, task: &state::TaskState, step: AgentStep, roadmap: &RoadmapConfig) -> String {
        let slug = &task.slug;
        match step {
            AgentStep::Post => {
                let check = if task.config.kind == TaskKind::FlakyTestFix {
                    "- nothing in tests.d/: this is a flaky-test-fix (spec §15), whose [check] and [[evidence]] are already in its plan and stay out of the suite. \
Your post is prompt.md, plus any test files its check needs that don't exist yet;"
                        .to_string()
                } else {
                    format!(
                        "- its [check] in tasks/{slug}/task.toml (unless the plan already has one) and the same check in tests.d/{slug}.toml \
(same name, cmd, runs and min_pass; check names are unique across the whole suite);"
                    )
                };
                format!(
                    "Step: post. task/{slug} is ready: its dependencies are accepted and merged in. Update your branch first: \
git fetch origin && git merge --ff-only origin/task/{slug}. Then make ONE commit on agent/{slug} with:\n\
- tasks/{slug}/prompt.md: task-specific instructions for the subagent only (the player prepends its own preamble on commit \
discipline and the pinned files), including one line telling it every test it adds must be pure;\n\
- the task's test files, under the suite's test_paths;\n\
{check}\n\
- updates to any existing test this task makes stale.\n\
Before committing, run the task's check and confirm it fails, for the reason the task exists: the player rejects a post whose \
check already passes. Then write your report (step: post)."
                )
            }
            AgentStep::Review => {
                let round = task.review_round + 1;
                let max = roadmap.limits.max_review_rounds;
                let base = self.hub.rev_parse(&format!("refs/dashboard/pins/{slug}")).unwrap_or_else(|| format!("origin/task/{slug}~1"));
                let cap = if round > max {
                    format!("\nNo review rounds are left ({max} used): a send-back now escalates the task. Recommend accept, or report unsure.")
                } else if round == max {
                    "\nThis is the last round you may send back.".to_string()
                } else {
                    String::new()
                };
                format!(
                    "Step: review round {round}. The subagent's work on task/{slug} passed the whole project suite at {tip}; the player ran \
it, so don't re-run it. Update your branch: git fetch origin && git merge --ff-only origin/task/{slug}. Read what the subagent \
changed this round: git diff {base} {tip}. Check that it does what the task asked, stays in scope, doesn't game its own test \
(special-cased inputs, monkeypatching, a helper that neuters other tests), adds no impure test, and won't break the tasks after \
it. Then either:\n\
- recommend accept: commit nothing, report verdict: accept;\n\
- send back: ONE commit adding tasks/{slug}/reviews/r{round}.md (what is wrong) and a new or updated test that makes the suite \
fail again, report verdict: sent-back. Before committing it, run that test against the subagent's code as it is and confirm it \
fails, for the reason your review gives: a send-back the suite still passes is a `review` strike. Run it only on the subagent's \
code: never write code of your own to see how the test should pass. A concern you can't turn into a failing test isn't a \
send-back: report verdict: unsure.{cap}",
                    tip = task.tip
                )
            }
            AgentStep::Contest => {
                let n = task.contest_round + 1;
                let reason = task.contest.clone().unwrap_or_default();
                format!(
                    "Step: contest c{n}. The subagent says a test it must pass is wrong (it can't be satisfied, or demands one particular \
implementation instead of behaviour):\n\n{reason}\n\n\
Update your branch: git fetch origin && git merge --ff-only origin/task/{slug}. Judge it from the task and the tests, still \
without writing any implementation. Then reply in ONE commit adding tasks/{slug}/contests/c{n}.md, your answer to the subagent, \
and, if it's right, the fixed tests (tests and tests.d/ only). If the test is right, the reply alone explains how it can be met. \
A reply spends no review round; the subagent may contest {} more time(s) before the task escalates. Then write your report \
(step: contest c{n}, verdict: replied).",
                    roadmap.limits.max_contest_rounds.saturating_sub(n)
                )
            }
            AgentStep::Wait => format!(
                "Step: none right now. task/{slug} is {}: wait here for the player to paste your next step or a question from the director.",
                task.status.as_str()
            ),
        }
    }

    /// Reads and removes the report a task agent left in its clone's git
    /// directory, noting it on the task tip (spec §17) and reporting it.
    fn collect_agent_report(&mut self, slug: &str, tip: &str, report: &mut TickReport) -> Result<(), String> {
        let dir = paths::agent_clone(&self.director, slug);
        if !dir.exists() {
            return Ok(());
        }
        let Ok(file) = Self::git_dir(&dir).map(|d| d.join("dashboard-report.md")) else { return Ok(()) };
        let Ok(text) = std::fs::read_to_string(&file) else { return Ok(()) };
        let _ = std::fs::remove_file(&file);
        let rec = records::record_agent_report(&self.hub, tip, records::AgentReport { verdict: report_verdict(&text), text: text.trim().to_string(), at: 0 })?;
        report.agent_reports.push((slug.to_string(), rec));
        Ok(())
    }

    /// Restarts `slug`'s dead task agent (`dashboard relaunch --agent`) in its
    /// existing clone, with a fresh prompt for where the task is now.
    pub fn relaunch_agent(&mut self, slug: &str) -> Result<(), String> {
        if !records::agent_launched(&self.hub, slug) {
            return Err(format!("relaunch: task/{slug} has no task agent — one is launched automatically once the task is ready"));
        }
        let key = state::agent_key(slug);
        if self.sessions.alive(&key) {
            return Err(format!("relaunch: task/{slug}'s task agent is already alive"));
        }
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let task = hub_state.tasks.iter().find(|t| t.slug == slug).ok_or_else(|| format!("relaunch: no task {slug}"))?;
        if matches!(task.status, Status::Accepted | Status::Retired | Status::Explored) {
            return Err(format!("relaunch: task/{slug} is {} — its task agent is done", task.status.as_str()));
        }
        if !hub_state.roadmap.player.task_agents_enabled() {
            return Err("relaunch: task agents are off (`[player] task_agent_cmd` is empty)".to_string());
        }
        let dir = paths::agent_clone(&self.director, slug);
        if !dir.exists() {
            return Err(format!("relaunch: task/{slug}'s task agent clone {} is missing", dir.display()));
        }
        let step = match task.status {
            Status::Pending | Status::Ready | Status::Orphaned => AgentStep::Post,
            Status::Tested => AgentStep::Review,
            Status::Contested => AgentStep::Contest,
            _ => AgentStep::Wait,
        };
        let prompt_file = Self::git_dir(&dir)?.join("dashboard-prompt.md");
        self.write_agent_prompt(task, step, &hub_state.roadmap, &dir, &prompt_file, true)?;
        if step != AgentStep::Post && step != AgentStep::Wait {
            records::set_agent_asked(&self.hub, slug, &task.tip)?;
        }
        self.sessions.launch(&key, &dir, &hub_state.roadmap.player.task_agent_cmd, &prompt_file)
    }

    /// Pastes the director's `question` into `slug`'s task agent session
    /// (`dashboard ask`); the answer comes back as its next report.
    pub fn ask(&mut self, slug: &str, question: &str) -> Result<(), String> {
        if !records::agent_launched(&self.hub, slug) {
            return Err(format!("ask: task/{slug} has no task agent"));
        }
        let key = state::agent_key(slug);
        if !self.sessions.alive(&key) {
            return Err(format!("ask: task/{slug}'s task agent is dead — `dashboard relaunch {slug} --agent` first"));
        }
        let report_file = Self::git_dir(&paths::agent_clone(&self.director, slug))?.join("dashboard-report.md");
        let file = report_file.display();
        self.sessions.send(
            &key,
            &format!("Question from the director: {question}\n\nAnswer with your report (step: question, verdict: answer or unsure), written to {file}.tmp and moved to {file}."),
        );
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
        let main_tip = self.hub.rev_parse("refs/heads/main").ok_or_else(|| "hub has no main".to_string())?;
        let accepted = records::accepted_tasks(&self.hub, &main_tip);

        for (refname, tip) in self.hub.for_each_ref("refs/heads/task/*") {
            let Some(slug) = refname.strip_prefix("refs/heads/task/") else { continue };

            if !matches!(records::import_note(&self.hub, &tip).map(|n| n.transition), None | Some(Transition::Plan) | Some(Transition::Replan) | Some(Transition::MergeUnblock)) {
                continue;
            }

            if accepted.contains_key(slug) {
                continue;
            }
            let Some(cfg) = self.task_config_at(&accepted, slug) else { continue };
            if !cfg.depends_on.iter().all(|d| accepted.contains_key(d)) {
                continue;
            }
            if self.hub.is_ancestor(&main_tip, &tip) {
                continue;
            }

            match self.hub.merge_tree_write(&tip, &main_tip) {
                Ok(Some(tree)) => match self.hub.commit_tree(&tree, &[&tip, &main_tip], &format!("Merge main into task/{slug}")) {
                    Ok(merge_sha) => match self.hub.update_ref_cas(&refname, &merge_sha, &tip) {
                        Ok(()) => {
                            records::record_import(&self.hub, &merge_sha, Actor::Player, Transition::MergeUnblock, Some(slug))?;
                            report.imported.push((refname.clone(), merge_sha, Transition::MergeUnblock));
                            report.merges.push(format!("task/{slug}: merged main"));
                        }
                        Err(e) => report.merges.push(format!("task/{slug}: ref moved during merge, skipped this tick ({e})")),
                    },
                    Err(e) => report.merges.push(format!("task/{slug}: commit-tree failed: {e}")),
                },
                Ok(None) => report.merges.push(format!("task/{slug}: merge conflict with main, skipped")),
                Err(e) => report.merges.push(format!("task/{slug}: merge-tree failed: {e}")),
            }
        }
        Ok(())
    }
}

// --------------------------------------------------------------------------
// Player state persistence (step 7: `home.root/player-state.json`)
// --------------------------------------------------------------------------

/// Loads `last` from `home.player_state()`. The bool is `first_run`: true
/// when no file was found at all (as opposed to a file that happens to parse
/// to an empty map), which is the "very first run" spec's event emission
/// stays silent for.
fn load_player_state(home: &Home) -> (BTreeMap<String, (Status, bool)>, bool) {
    match std::fs::read_to_string(home.player_state()) {
        Ok(text) => (serde_json::from_str(&text).unwrap_or_default(), false),
        Err(_) => (BTreeMap::new(), true),
    }
}

fn save_player_state(home: &Home, last: &BTreeMap<String, (Status, bool)>) -> Result<(), String> {
    let path = home.player_state();
    let json = serde_json::to_string(last).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))
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
        Status::Ready | Status::Tested | Status::Contested if managed => None,
        Status::Contested => Some((EventKind::Contested, task.contest.clone().unwrap_or_default())),
        Status::Tested if task.spike.is_some() => {
            Some((EventKind::Tested, "spike finish validated — claims passed; review the findings with the operator, then send back a new question or conclude".to_string()))
        }
        Status::Tested => Some((EventKind::Tested, "finish accepted — tests passed, awaiting director review".to_string())),
        Status::Escalated => Some((EventKind::Escalated, task.escalation.clone().unwrap_or_default())),
        Status::Ready if task.spike.is_some() => Some((EventKind::Ready, format!("spike task/{} is planned: post its question when the operator agrees", task.slug))),
        Status::Ready => Some((EventKind::Ready, format!("task/{}'s dependencies are all accepted and merged in", task.slug))),
        Status::Accepted => Some((EventKind::Accepted, format!("task/{} accepted and merged into main", task.slug))),
        Status::Explored => {
            let answer = task.spike.as_ref().and_then(|sp| sp.answer.as_deref()).unwrap_or("?");
            Some((EventKind::Explored, format!("spike {} concluded: {answer}", task.slug)))
        }
        Status::Retired => Some((EventKind::Retired, first_line(task.post_mortem.as_deref().unwrap_or("")))),
        // `conflicted`: emitted from the tick report instead (`emit_events`).
        Status::Pending | Status::Orphaned | Status::Posted | Status::InProgress | Status::InRevision | Status::Conflicted => None,
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
    if path == "roadmap.toml" {
        return true;
    }
    docs_paths.iter().any(|d| {
        let prefix = d.trim_end_matches('/');
        path == prefix || path.starts_with(&format!("{prefix}/"))
    })
}

/// `path` is exactly one of `test_paths` (spec §6.2), or falls under one of
/// them treated as a directory prefix. Used by the send-back path check.
fn path_under_test_paths(path: &str, test_paths: &[String]) -> bool {
    test_paths.iter().any(|t| {
        let prefix = t.trim_end_matches('/');
        path == prefix || path.starts_with(&format!("{prefix}/"))
    })
}

/// `path`'s basename looks like a review note (`tasks/<slug>/reviews/r<N>.md`):
/// used only to tell "no review file at all" apart from "wrong round number"
/// in a send-back rejection message (spec §9).
fn is_review_filename(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or("");
    base.starts_with('r') && base.ends_with(".md")
}

fn path_str(p: &Path) -> Result<&str, String> {
    p.to_str().ok_or_else(|| format!("{} is not valid UTF-8", p.display()))
}

/// The player-written preamble that opens a subagent's prompt file (spec
/// §8.3, §9's launch row): who it is, the commit discipline, the paths it
/// must leave untouched (spelled out, not just described), the `block:`
/// escape hatch, the history rules, and the fact that the player pastes
/// messages into this same session later.
fn launch_preamble(slug: &str, project: &str, pinned_paths: &[&str]) -> String {
    let mut sorted = pinned_paths.to_vec();
    sorted.sort_unstable();
    let pinned_block = if sorted.is_empty() { "  (no pinned files recorded)".to_string() } else { sorted.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n") };
    format!(
        "You are the subagent for task `{slug}` in project `{project}`. Your clone's `origin` remote \
is the hub: fetch from it, and never push anywhere else.\n\n\
Commit only when you believe the whole project suite (every [[check]] in tests.toml and tests.d/*.toml) \
passes. Leave work in progress uncommitted.\n\n\
You may add new tests, but you must not modify, delete, or add to tests.toml or tests.d/, touch anything \
under its test_paths, or touch tasks/{slug}/. The pinned files, which must stay byte-for-byte identical, are:\n\
{pinned_block}\n\n\
If the task can't be done as specified, make an empty commit explaining why instead of forcing it \
through:\n  git commit --allow-empty -m \"block: <short reason>\" -m \"<explanation>\"\n\n\
If instead a test you must pass is wrong (it can't be satisfied, or it demands one particular implementation rather than \
behaviour), reset to the hub tip and contest it with an empty commit naming the test and why; the reply is pasted here:\n  \
git commit --allow-empty -m \"contest: <test name>\" -m \"<why it's wrong>\"\n\n\
Never push, rebase, amend, reset published commits, or create merge commits.\n\n\
The player will paste messages (rejections, director reviews) into this session as things happen — \
follow their recovery instructions."
    )
}

/// What a task agent is asked to do (spec §17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentStep {
    Post,
    Review,
    /// Reply to the subagent's contest (spec §18).
    Contest,
    /// Nothing pending (a relaunch while the subagent works).
    Wait,
}

/// The player-written preamble of a task agent's prompt (spec §17): its
/// role, its rules, the task as planned, and the report it hands back.
fn agent_preamble(slug: &str, project: &str, cfg: &TaskConfig, task_toml: &str, goal: &str, report_file: &Path) -> String {
    let report = report_file.display();
    let brief = if cfg.brief.trim().is_empty() { "(none)".to_string() } else { cfg.brief.trim().to_string() };
    let phase = if goal.is_empty() { cfg.phase.clone() } else { format!("{}: {goal}", cfg.phase) };
    format!(
        "You are the task agent for task `{slug}` in project `{project}`: the director's delegate for this one task. You write its \
post (the subagent's prompt and a currently failing test) and review the subagent's finished work. The subagent that implements \
the task is a different agent you never talk to, and the director reads only your reports.\n\n\
This directory is your worktree, on the local branch agent/{slug}; its origin remote is the hub. Commit on agent/{slug} and never \
push: the player fetches your branch, validates each commit, and pastes its verdict into this session. Never rebase, amend or \
reset a commit the player imported, and never create merge commits.\n\n\
Rules:\n\
- Never write a throwaway implementation: not to prove a test can pass, not to try out a fix, not even in a scratch directory you \
delete afterwards. Implementing is the subagent's job; reason about the tests from the task and the existing code. You may run \
any test you write against the code as it is (the base before a post, the subagent's work before a send-back) to see it fail for \
the right reason, but never against code you wrote to make it pass.\n\
- Test observable behaviour through the task's interface, not internals or one particular implementation. The subagent may \
contest a test it can't satisfy or that over-specifies, and you then have to reply.\n\
- Every test you write or send back must be pure: no effects outside its own sandbox, no shared state, no network, real clock or \
unseeded randomness, no dependence on test order. The prompt.md you write passes this rule on to the subagent in one line.\n\
- Write only tests, tests.toml, tests.d/ and files under tasks/{slug}/, never product code. In task.toml only [check] is yours; \
the rest is the director's plan.\n\
- Never commit accept:, retire: or any other marker: you recommend, the director decides.\n\n\
The task, as the director planned it (phase {phase}):\n\n```toml\n{}\n```\n\n\
Director's brief: {brief}\n\n\
After each step, and after answering a question from the director, write your report: exactly the block below, to {report}.tmp, \
then mv it to {report} (the player reads and removes it). No diffs and no code in it.\n\n\
slug: {slug}\n\
step: post | review r<N> | contest c<N> | question\n\
commit: <sha, or \"none\">\n\
check: <one line: what the task's check asserts and why it fails / now passes>\n\
verdict: posted | accept | sent-back | replied | unsure | answer\n\
why: <two or three sentences>\n\
concerns: <anything cross-task the director should know, or \"none\">\n\n\
The player pastes later steps into this session (review requests, the director's questions, rejections of your commits); wait for \
them between steps.",
        task_toml.trim()
    )
}

/// The `verdict:` of a task agent's report, or `unknown`.
fn report_verdict(text: &str) -> String {
    const KNOWN: [&str; 6] = ["posted", "accept", "sent-back", "replied", "unsure", "answer"];
    text.lines()
        .find_map(|l| l.trim().strip_prefix("verdict:"))
        .and_then(|v| v.split_whitespace().next())
        .map(|v| v.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-').to_ascii_lowercase())
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
    fn report_verdict_reads_the_verdict_line() {
        assert_eq!(report_verdict("slug: s\nverdict: accept\nwhy: ok"), "accept");
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
