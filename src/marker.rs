// The director's markers (spec §5, §9, §14, §17): the empty commits
// `stage:`, `unstage:`, `accept:`, `retire:` and `conclude:` on a task
// branch, and the `raise:` commit on `main` (architect-v3.md, "Budget
// increase path"). The player reads a marker by its subject's prefix
// (`Marker::of`) and gates it with `gate`; `dashboard stage|unstage|accept|
// retire|conclude`, run in the director clone, builds the marker on top of
// the hub's tip of `task/<slug>` after the same gate, so an ill-timed one is
// refused there with the player's own reason. `dashboard raise` commits the
// budget edits of the director clone's working tree on top of the hub's
// `main` once `raise::judge` passes them.

use crate::config::{self, Reason};
use crate::fsm::{self, Ctx, Move, Verdict};
use crate::git::Git;
use crate::lean::Exporter;
use crate::records::{self, Actor};
use crate::state::{self, Status};
use crate::{raise, report, spike};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Stage,
    Unstage,
    Accept,
    Retire,
    Conclude,
}

impl Marker {
    pub const ALL: [Marker; 5] = [Marker::Stage, Marker::Unstage, Marker::Accept, Marker::Retire, Marker::Conclude];

    /// The transition's name, which is also the CLI command's.
    pub fn name(self) -> &'static str {
        match self {
            Marker::Stage => "stage",
            Marker::Unstage => "unstage",
            Marker::Accept => "accept",
            Marker::Retire => "retire",
            Marker::Conclude => "conclude",
        }
    }

    /// The subject prefix the player reads.
    pub fn prefix(self) -> &'static str {
        match self {
            Marker::Stage => "stage:",
            Marker::Unstage => "unstage:",
            Marker::Accept => "accept:",
            Marker::Retire => "retire:",
            Marker::Conclude => "conclude:",
        }
    }

    pub fn mv(self) -> Move {
        match self {
            Marker::Stage => Move::Stage,
            Marker::Unstage => Move::Unstage,
            Marker::Accept => Move::Accept,
            Marker::Retire => Move::Retire,
            Marker::Conclude => Move::Conclude,
        }
    }

    /// The marker a commit subject names, if any.
    pub fn of(subject: &str) -> Option<Marker> {
        Marker::ALL.into_iter().find(|m| subject.starts_with(m.prefix()))
    }

    /// The commit message: `<prefix> <head>`, then `body` when there is one.
    pub fn message(self, head: &str, body: &str) -> String {
        let subject = format!("{} {}", self.prefix(), head.trim());
        match body.trim() {
            "" => subject,
            body => format!("{subject}\n\n{body}"),
        }
    }
}

/// The player's gate on a director's marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Allow,
    /// Left alone: the task is retired or explored.
    Skip,
    /// Rejected, under this strike reason, with this message.
    Refuse(Reason, String),
}

/// The gate for the director's `m` on `slug` (`ctx`: its situation, none
/// for a task the hub doesn't know), from `fsm::decide`.
pub fn gate(m: Marker, slug: &str, ctx: Option<Ctx>) -> Gate {
    let reason = match ctx.map(|c| fsm::decide(Actor::Director, m.mv(), c)) {
        Some(Verdict::Allow) => return Gate::Allow,
        Some(Verdict::Ignore | Verdict::Hold) => return Gate::Skip,
        Some(Verdict::Reject(r)) => Some(r),
        None => None,
    };
    let status = ctx.map(|c| c.status);
    let shown = status.map(|s| s.as_str()).unwrap_or("unknown");
    let name = m.name();
    let (fallback, msg) = match m {
        Marker::Stage | Marker::Unstage => {
            let needs = if m == Marker::Stage { Status::Ready } else { Status::Staged };
            let msg = if status != Some(needs) {
                format!("{name}: task/{slug} is {shown} — only a {} task can be {name}d", needs.as_str())
            } else if m == Marker::Stage {
                format!("stage: task/{slug} is a spike — it has no task agent to start")
            } else {
                format!("unstage: task/{slug}'s task agent is already running — retire the task to stop it")
            };
            (Reason::Plan, msg)
        }
        Marker::Accept => {
            let msg = if reason.is_some() && status == Some(Status::Tested) {
                format!("accept: task/{slug} is a spike: it never merges into main (finish it with `conclude:`)")
            } else {
                format!("accept: task/{slug} is {shown} — it must be tested before it can be accepted")
            };
            (Reason::Accept, msg)
        }
        Marker::Conclude => {
            let msg = if reason == Some(Reason::Ownership) {
                format!("conclude: task/{slug} is not a spike — an ordinary task is finished with `accept:`")
            } else {
                format!("conclude: task/{slug} is {shown} — a spike needs a validated finish before it can be concluded")
            };
            (Reason::Accept, msg)
        }
        Marker::Retire => {
            let msg = format!(
                "retire: task/{slug} is {shown} — retire is only allowed while pending/ready/orphaned (not posted or in flight), conflicted, contested or escalated"
            );
            return Gate::Refuse(Reason::Retire, msg);
        }
    };
    Gate::Refuse(reason.unwrap_or(fallback), msg)
}

/// The player's rejection of a `conclude:` on a spike with no verified
/// answer at its tip.
pub fn no_answer(slug: &str) -> String {
    format!("conclude: task/{slug} has no verified answer recorded at its tip")
}

/// A marker committed in the director clone, for the player to import.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub commit: String,
    pub message: String,
}

/// What the director hands a marker command: `answer` for `conclude`,
/// `why` for the body.
#[derive(Debug, Clone, Default)]
pub struct Args {
    pub answer: Option<String>,
    pub why: Option<String>,
}

/// Fetches the hub into `director` (its `hub` remote) and commits marker
/// `m` on `task/<slug>`, on top of the hub's tip, once the player's gate
/// allows it there. The body is `args.why`; an accept without one takes
/// the task agent's latest accept report's why, a retire and a conclude
/// need one. A conclude's answer is the player's verified one; a given
/// answer must be it.
pub fn run(hub: &Git, director: &Git, m: Marker, slug: &str, args: &Args) -> Result<Outcome, String> {
    director
        .run(&["fetch", "--quiet", "hub"])
        .map_err(|e| format!("fetching the hub: {e}"))?;
    let hub_state = state::derive(hub, &|_| false)?;
    match gate(m, slug, Ctx::of(&hub_state, slug)) {
        Gate::Allow => {}
        Gate::Skip => {
            let status = hub_state.tasks.iter().find(|t| t.slug == slug).map_or("unknown", |t| t.status.as_str());
            return Err(format!("{}: task/{slug} is {status}: the player ignores markers on it", m.name()));
        }
        Gate::Refuse(_, msg) => return Err(msg),
    }
    let tip = hub
        .rev_parse(&format!("refs/heads/task/{slug}"))
        .ok_or_else(|| format!("the hub has no task/{slug}"))?;
    let why = args.why.as_deref().map(str::trim).filter(|w| !w.is_empty());
    let (head, body) = match m {
        Marker::Stage | Marker::Unstage => (slug.to_string(), why.unwrap_or_default().to_string()),
        Marker::Accept => {
            let body = match why {
                Some(w) => w.to_string(),
                None => accept_why(hub, &tip).ok_or_else(|| {
                    format!("accept: no task agent's accept report on task/{slug} to take the why from: pass --why")
                })?,
            };
            (slug.to_string(), body)
        }
        Marker::Retire => {
            let body = why.ok_or("retire needs --why: the post-mortem, the only record of why the task died")?;
            (slug.to_string(), body.to_string())
        }
        Marker::Conclude => {
            let note = records::exploration(hub, &tip).ok_or_else(|| no_answer(slug))?;
            let question = spike::read_question(hub, &tip, slug, note.round)?;
            if let Some(given) = args.answer.as_deref().map(str::trim) {
                if !question.allows(given) {
                    let mut allowed = question.answers.clone();
                    allowed.push(config::INCONCLUSIVE.to_string());
                    return Err(format!(
                        "conclude: {given:?} is no answer to round {}'s question; allowed: {}",
                        note.round,
                        allowed.join(", ")
                    ));
                }
                if given != note.answer {
                    return Err(format!(
                        "conclude: the player verified {:?} in round {}; a conclude records that answer (send back with a new question to pursue {given:?})",
                        note.answer, note.round
                    ));
                }
            }
            let body = why.ok_or("conclude needs --why: why the answer stands, as agreed with the operator")?;
            (note.answer.clone(), body.to_string())
        }
    };
    let message = m.message(&head, &body);
    let commit = director
        .run(&["commit-tree", &format!("{tip}^{{tree}}"), "-p", &tip, "-m", &message])
        .map_err(|e| e.to_string())?;
    director.advance_branch(&format!("task/{slug}"), &tip, &commit, m.name())?;
    Ok(Outcome { commit, message })
}

/// The why of the task agent's latest review verdict on `tip`'s
/// first-parent history when it is an accept: a newer `unsure` or
/// send-back leaves none.
pub fn accept_why(hub: &Git, tip: &str) -> Option<String> {
    let review = |v: &str| matches!(v, "accept" | "unsure" | "sent-back");
    hub.rev_list(&["--first-parent", tip, "^refs/heads/main"])
        .into_iter()
        .find_map(|c| records::agent_reports(hub, &c).into_iter().rev().find(|r| review(&r.verdict)))
        .filter(|r| r.verdict == report::Verdict::Accept.as_str())
        .and_then(|r| report::why_of(&r.text).map(str::to_string))
}

/// The subject of a raise: `raise(<slug>): <first line>` or `raise: …`,
/// read back by `raise::parse_reason`.
pub fn raise_message(slug: Option<&str>, reason: &str) -> String {
    let reason = reason.trim();
    let (first, rest) = reason.split_once('\n').unwrap_or((reason, ""));
    let subject = match slug {
        Some(s) => format!("raise({s}): {}", first.trim()),
        None => format!("raise: {}", first.trim()),
    };
    match rest.trim() {
        "" => subject,
        body => format!("{subject}\n\n{body}"),
    }
}

/// A raise committed in the director clone.
#[derive(Debug, Clone)]
pub struct Raised {
    pub commit: String,
    pub raises: Vec<raise::Raise>,
}

/// `dashboard raise`: commits the edits in `director`'s working tree, with
/// `main` checked out at the hub's `main`, as a budget raise on top of it,
/// once `raise::judge` passes them as the player will. The index and the
/// working tree end up at the new commit.
pub fn raise(hub: &Git, director: &Git, slug: Option<&str>, reason: &str, exporter: &Exporter) -> Result<Raised, String> {
    if reason.trim().is_empty() {
        return Err("raise needs --reason: the measured overage, discussed with the operator".to_string());
    }
    director
        .run(&["fetch", "--quiet", "hub"])
        .map_err(|e| format!("fetching the hub: {e}"))?;
    let main = hub.rev_parse("refs/heads/main").ok_or("the hub has no main")?;
    let head = director.run(&["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    let at = director.rev_parse("HEAD");
    if head != "refs/heads/main" || at.as_deref() != Some(main.as_str()) {
        return Err(
            "a raise goes on main at the hub's tip: `git checkout main && git merge --ff-only hub/main` (your edits carry over), then raise".to_string(),
        );
    }
    let files: Vec<String> = director
        .run(&["diff", "--name-only", "HEAD"])
        .map_err(|e| e.to_string())?
        .lines()
        .map(str::to_string)
        .collect();
    if files.is_empty() {
        return Err(format!("nothing to raise: edit the budget fields in {}/ first", config::ARCH_DIR));
    }
    let message = raise_message(slug, reason);
    let commit = commit_worktree(director, &main, &files, &message)?;
    let tree = director
        .rev_parse(&format!("{commit}:{}", config::ARCH_DIR))
        .ok_or_else(|| format!("{}: no tree at the raise", config::ARCH_DIR))?;
    let new = exporter
        .export_tree(director, &tree)
        .map(|e| raise::Source::from_export(&e))
        .map_err(|e| format!("the edited architecture: {e}"))?;
    let old = records::arch_source_at(hub, &main, Some(exporter))?;
    let (subject, body) = message.split_once("\n\n").unwrap_or((&message, ""));
    let raises = raise::judge(old.as_ref(), &new, subject, body, &files, 0)?;
    if raises.is_empty() {
        return Err("the edits raise no budget: commit them as an ordinary main commit".to_string());
    }
    director
        .run(&["update-ref", "-m", "dashboard raise", "refs/heads/main", &commit, &main])
        .map_err(|e| e.to_string())?;
    director.run(&["reset", "--quiet"]).map_err(|e| e.to_string())?;
    Ok(Raised { commit, raises })
}

/// The commit of `files` as they are in `git`'s working tree on top of
/// `base`, built in a private index so the real one stays as it is.
fn commit_worktree(git: &Git, base: &str, files: &[String], message: &str) -> Result<String, String> {
    let git_dir = git
        .run(&["rev-parse", "--absolute-git-dir"])
        .map_err(|e| e.to_string())?;
    let index = format!("{git_dir}/dashboard-raise.index");
    let env = [("GIT_INDEX_FILE", index.as_str())];
    let result = (|| {
        git.run_with_env(&["read-tree", base], &env).map_err(|e| e.to_string())?;
        let mut args = vec!["update-index", "--add", "--remove", "--"];
        args.extend(files.iter().map(String::as_str));
        git.run_with_env(&args, &env).map_err(|e| e.to_string())?;
        let tree = git.run_with_env(&["write-tree"], &env).map_err(|e| e.to_string())?;
        git.run(&["commit-tree", &tree, "-p", base, "-m", message])
            .map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&index);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(status: Status, spike: bool, agent: bool) -> Option<Ctx> {
        Some(Ctx { status, managed: !spike, spike, agent })
    }

    #[test]
    fn subjects_name_their_marker() {
        for m in Marker::ALL {
            assert_eq!(Marker::of(&m.message("x", "")), Some(m));
        }
        assert_eq!(Marker::of("block: x"), None);
        assert_eq!(Marker::Retire.message("s", " why \n"), "retire: s\n\nwhy");
    }

    #[test]
    fn the_gate_says_why_with_the_players_words() {
        assert_eq!(gate(Marker::Accept, "s", ctx(Status::Tested, false, true)), Gate::Allow);
        let Gate::Refuse(r, msg) = gate(Marker::Accept, "s", ctx(Status::InProgress, false, true)) else { panic!() };
        assert_eq!(r, Reason::Accept);
        assert!(msg.contains("is in_progress — it must be tested"), "{msg}");
        let Gate::Refuse(_, msg) = gate(Marker::Accept, "s", ctx(Status::Tested, true, false)) else { panic!() };
        assert!(msg.contains("is a spike"), "{msg}");
        let Gate::Refuse(r, msg) = gate(Marker::Stage, "s", ctx(Status::Staged, false, false)) else { panic!() };
        assert_eq!(r, Reason::Plan);
        assert!(msg.contains("only a ready task can be staged"), "{msg}");
        let Gate::Refuse(_, msg) = gate(Marker::Unstage, "s", ctx(Status::Staged, false, true)) else { panic!() };
        assert!(msg.contains("already running"), "{msg}");
        let Gate::Refuse(r, msg) = gate(Marker::Conclude, "s", ctx(Status::Tested, false, true)) else { panic!() };
        assert_eq!(r, Reason::Ownership);
        assert!(msg.contains("not a spike"), "{msg}");
        let Gate::Refuse(r, msg) = gate(Marker::Retire, "s", None) else { panic!() };
        assert_eq!(r, Reason::Retire);
        assert!(msg.contains("is unknown"), "{msg}");
        assert_eq!(gate(Marker::Retire, "s", ctx(Status::Retired, false, false)), Gate::Skip);
    }

    #[test]
    fn a_raise_message_reads_back() {
        let msg = raise_message(Some("t1"), "the parser grew\nby 12%");
        let (subject, body) = msg.split_once("\n\n").unwrap();
        assert_eq!(raise::parse_reason(subject, body), Some((Some("t1".into()), "the parser grew\nby 12%".into())));
        assert_eq!(raise_message(None, "x"), "raise: x");
    }
}
