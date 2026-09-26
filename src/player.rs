// The player (spec.md §4-§5, §9, §11): the only writer of the hub. Each tick
// fetches the director's clone, imports and validates its commits onto the
// hub's refs, and runs the player's own merges. Step 5a covers the skeleton,
// history rules, director `main` (docs-only commits) and `task/*` branches'
// plan/replan transitions, and unblock merges. Step 5b fills in post,
// send-back, accept and retire (see the `// step 5b` dispatch point below);
// step 6 adds subagent import and tmux launch (see `// step 6` in `tick`);
// step 7 adds events.
#![allow(dead_code)]

use crate::checks;
use crate::config::{self, RoadmapConfig, Reason, TaskConfig};
use crate::git::Git;
use crate::paths::Home;
use crate::records::{self, Actor, RejectionRecord, Transition};
use crate::state::{self, Status};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

// --------------------------------------------------------------------------
// Sessions: the runtime tmux side (spec §9's launch/relay), abstracted so
// tests never need a real tmux. Step 6 adds launch and the tmux impl; this
// step only needs `alive` (for `state::derive`) and stubs the rest.
// --------------------------------------------------------------------------

pub trait Sessions {
    fn alive(&self, slug: &str) -> bool;
    fn send(&mut self, slug: &str, message: &str);
    fn kill(&mut self, slug: &str);
}

/// A `Sessions` that never reports anything alive, and records every
/// `send`/`kill` call so tests can assert on them instead of touching tmux.
#[derive(Debug, Default)]
pub struct NullSessions {
    pub sent: Vec<(String, String)>,
    pub killed: Vec<String>,
}

impl Sessions for NullSessions {
    fn alive(&self, _slug: &str) -> bool {
        false
    }

    fn send(&mut self, slug: &str, message: &str) {
        self.sent.push((slug.to_string(), message.to_string()));
    }

    fn kill(&mut self, slug: &str) {
        self.killed.push(slug.to_string());
    }
}

// --------------------------------------------------------------------------
// TickReport
// --------------------------------------------------------------------------

/// What one `Player::tick` call did. `imported` is `(ref, sha, transition)`
/// for every commit the player advanced a hub ref to (director docs commits
/// on main, plans, replans, and the player's own unblock merges land here
/// too via their `Transition::MergeUnblock` note). `merges` carries a short
/// human line per unblock-merge attempt, success or failure (spec §11)  —
/// there's no reason to model it more richly than the CLI's report needs.
#[derive(Debug, Default)]
pub struct TickReport {
    pub imported: Vec<(String, String, Transition)>,
    pub rejected: Vec<RejectionRecord>,
    pub merges: Vec<String>,
}

impl TickReport {
    pub fn is_empty(&self) -> bool {
        self.imported.is_empty() && self.rejected.is_empty() && self.merges.is_empty()
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
        lines.extend(self.merges.iter().cloned());
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
}

impl<S: Sessions> Player<S> {
    /// Opens the player against an already-`dashboard init`-ed home: reads
    /// `dashboard.director` and `dashboard.project` from the hub's config.
    pub fn open(home: Home, sessions: S) -> Result<Self, String> {
        let hub = Git::new(home.hub());
        let director = hub.config_get("dashboard.director").ok_or_else(|| "hub is missing dashboard.director — run `dashboard init`".to_string())?;
        let project = hub.config_get("dashboard.project").ok_or_else(|| "hub is missing dashboard.project — run `dashboard init`".to_string())?;
        let runner = checks::Runner::new(&home);
        Ok(Self { home, hub, director: PathBuf::from(director), project, runner, sessions })
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
        // step 6: subagent import + launch
        self.unblock_merges(&mut report)?;
        Ok(report)
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
        if !suite.pass {
            return Err((Reason::Suite, format!("suite: {}", suite.summary())));
        }
        Ok(())
    }

    // ---- director task/<slug> branches ----

    fn import_task_branches(&mut self, report: &mut TickReport) -> Result<(), String> {
        for (refname, sha) in self.hub.for_each_ref("refs/incoming/director/task/*") {
            let Some(slug) = refname.strip_prefix("refs/incoming/director/task/") else { continue };
            if slug.is_empty() {
                continue;
            }
            self.import_task_branch(slug, &sha, report)?;
        }
        Ok(())
    }

    fn import_task_branch(&mut self, slug: &str, incoming: &str, report: &mut TickReport) -> Result<(), String> {
        // A retired slug: the director's local branch is stale (they haven't
        // deleted it in their own clone yet). Ignore it entirely — not even
        // a rejection, since there's nothing to say.
        if records::is_retired(&self.hub, slug) {
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
                self.hub.rev_list(&[&format!("{hub_tip}..{incoming}"), "--reverse"])
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
            if self.hub.parents(&c).len() > 1 || rejected_ancestors.iter().any(|r| self.hub.is_ancestor(r, &c)) {
                // Rejection scope is the slug even before the branch exists
                // (a plan attempt that used a merge commit): these are
                // strikes on the slug the moment it's later planned, since
                // strikes are just rejection records keyed by scope string.
                let msg = format!("history: task/{slug} commit {} is a merge commit; only the player creates merges", short(&c));
                let rec = records::record_rejection(&self.hub, slug, &c, Reason::History, Actor::Director, &msg)?;
                report.rejected.push(rec);
                rejected_ancestors.push(c);
                continue;
            }

            match self.hub.rev_parse(&hub_ref) {
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

        let expected_path = format!("tasks/{slug}/task.toml");
        let diffs = self.hub.diff_names(main_tip, candidate);
        if diffs.len() != 1 || diffs[0].0 != 'A' || diffs[0].1 != expected_path {
            return Err(format!("plan: task/{slug}'s commits may only add {expected_path}"));
        }

        let text = self.hub.show_file(candidate, &expected_path).ok_or_else(|| format!("plan: {expected_path} missing at {candidate}"))?;
        let cfg = config::task_config_from_str(&text).map_err(|e| format!("plan: {expected_path}: {e}"))?;
        self.validate_task_config(slug, main_tip, &cfg)
    }

    fn try_existing(&mut self, slug: &str, tip: &str, candidate: &str, report: &mut TickReport) -> Result<(), String> {
        let hub_state = state::derive(&self.hub, &|s| self.sessions.alive(s))?;
        let pending_family = hub_state
            .tasks
            .iter()
            .find(|t| t.slug == slug)
            .map(|t| matches!(t.status, Status::Pending | Status::Ready | Status::Orphaned))
            .unwrap_or(false);

        let prompt_path = format!("tasks/{slug}/prompt.md");
        let has_prompt = self.hub.blob_hash(candidate, &prompt_path).is_some();
        let diffs = self.hub.diff_names(tip, candidate);
        let task_toml_path = format!("tasks/{slug}/task.toml");
        let only_task_toml = diffs.len() == 1 && diffs[0].1 == task_toml_path;

        if pending_family && !has_prompt && only_task_toml {
            match self.validate_replan(slug, tip, candidate) {
                Ok(()) => {
                    let hub_ref = format!("refs/heads/task/{slug}");
                    self.hub.update_ref_cas(&hub_ref, candidate, tip).map_err(|e| e.to_string())?;
                    records::record_import(&self.hub, candidate, Actor::Director, Transition::Replan, Some(slug))?;
                    report.imported.push((hub_ref, candidate.to_string(), Transition::Replan));
                }
                Err(msg) => {
                    let rec = records::record_rejection(&self.hub, slug, candidate, Reason::Plan, Actor::Director, &msg)?;
                    report.rejected.push(rec);
                }
            }
            return Ok(());
        }

        // step 5b: post, finish, block, send-back, accept, retire, and any
        // other commit shape on an existing branch (prompt.md added,
        // reviews/, accept:/retire:/block: markers, commits during in-flight
        // states) all dispatch here. Until that lands, every one of them is
        // rejected as ownership so nothing is silently accepted.
        let msg = "unsupported transition (not yet implemented)".to_string();
        let rec = records::record_rejection(&self.hub, slug, candidate, Reason::Ownership, Actor::Director, &msg)?;
        report.rejected.push(rec);
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

    // ---- unblock merges (spec §11) ----

    /// For every pending-family task branch whose deps are all accepted but
    /// whose tip doesn't yet contain some dep's merge, merges `main` into
    /// the branch in the bare hub (no worktree needed for a tree-level
    /// merge).
    fn unblock_merges(&mut self, report: &mut TickReport) -> Result<(), String> {
        let main_tip = self.hub.rev_parse("refs/heads/main").ok_or_else(|| "hub has no main".to_string())?;
        let accepted = records::accepted_tasks(&self.hub, &main_tip);

        for (refname, tip) in self.hub.for_each_ref("refs/heads/task/*") {
            let Some(slug) = refname.strip_prefix("refs/heads/task/") else { continue };

            if !matches!(records::import_note(&self.hub, &tip).map(|n| n.transition), None | Some(Transition::Plan) | Some(Transition::Replan) | Some(Transition::MergeUnblock)) {
                continue;
            }

            let Some(cfg) = self.task_config_at(&accepted, slug) else { continue };
            if cfg.depends_on.is_empty() {
                continue;
            }
            if !cfg.depends_on.iter().all(|d| accepted.contains_key(d)) {
                continue;
            }
            let needs_merge = cfg.depends_on.iter().any(|d| match accepted.get(d) {
                Some(merge_sha) => !self.hub.is_ancestor(merge_sha, &tip),
                None => false,
            });
            if !needs_merge {
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
    fn tick_report_display_no_changes() {
        let report = TickReport::default();
        assert_eq!(report.to_string(), "no changes");
    }
}
