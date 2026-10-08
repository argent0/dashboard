//! The task state machine's status gate, in one place: given who commits
//! (`Actor`), what they are doing (`Move`) and the task's situation
//! (`Ctx`), is it allowed, rejected under which strike `Reason`, or
//! ignored?
//!
//! A task's status is *derived* (`state::derive_task`: tip note, dependencies,
//! liveness, records), so this is not that derivation. It is only the gate
//! the player's import paths apply before looking at a commit's content
//! (`try_existing`, `try_marker`, `try_agent_commit`, `import_workers`, and
//! the status checks inside `try_accept`/`try_retire`/…). Content checks
//! (pins, suite, paths, a post's tests) stay where they are. The `Reason` of
//! a `Reject` is the one the player records for a commit that fails the gate.
//!
//! `decide` matches exhaustively on `Actor`/`Move` with no wildcard on
//! either, so a new move or actor fails to compile until it is decided; the
//! tests walk every `Status × Actor × Move × flags` cell and pin who may do
//! what where. Not wired into the player yet: the tests are the contract the
//! dispatch has to keep.

use crate::config::Reason;
use crate::records::Actor;
use crate::state::Status;

/// What a commit is trying to do, by intent rather than by its shape. Planning
/// is not here: it needs no branch (`Ctx` has none), see `may_plan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// Adds `tasks/<slug>/prompt.md` and the task's check.
    Post,
    /// Changes only `tasks/<slug>/task.toml`.
    Replan,
    SendBack,
    ContestReply,
    Stage,
    Unstage,
    Accept,
    Retire,
    Conclude,
    /// A director's merge resolving a recorded conflict.
    Resolve,
    /// A worker's finish, its `block:` marker and its `contest:` marker.
    Finish,
    Block,
    Contest,
    /// Anything else: a marker with another subject, or an unclassifiable commit.
    Other,
}

impl Move {
    pub const ALL: [Move; 14] = [
        Move::Post,
        Move::Replan,
        Move::SendBack,
        Move::ContestReply,
        Move::Stage,
        Move::Unstage,
        Move::Accept,
        Move::Retire,
        Move::Conclude,
        Move::Resolve,
        Move::Finish,
        Move::Block,
        Move::Contest,
        Move::Other,
    ];
}

pub const ACTORS: [Actor; 3] = [Actor::Director, Actor::TaskAgent, Actor::Worker];

pub const STATUSES: [Status; 14] = [
    Status::Pending,
    Status::Orphaned,
    Status::Ready,
    Status::Staged,
    Status::Posted,
    Status::InProgress,
    Status::Tested,
    Status::InRevision,
    Status::Conflicted,
    Status::Contested,
    Status::Escalated,
    Status::Accepted,
    Status::Explored,
    Status::Retired,
];

/// The task a commit lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx {
    pub status: Status,
    /// Task agents are on for this task (`state::task_agent_managed`):
    /// posts, send-backs and contest replies are theirs, not the director's.
    pub managed: bool,
    pub spike: bool,
    /// A task agent was launched for it (`refs/dashboard/agent/<slug>`).
    pub agent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Refused, recorded as a strike under this reason.
    Reject(Reason),
    /// Not looked at at all: the branch is gone, or the sender is not being
    /// listened to at this status.
    Ignore,
    /// Neither imported nor struck: the sender did nothing wrong, the task
    /// just isn't where the move makes sense (a task agent's post after the
    /// director sent the task back to `pending`). Judged again each tick, so
    /// it imports once the task is back.
    Hold,
}

/// A new task may be planned only where no branch exists yet; everything
/// else goes through `decide`.
pub fn may_plan(branch_exists: bool) -> bool {
    !branch_exists
}

/// The statuses where a task is still the director's to shape: before a post.
fn pending_family(s: Status) -> bool {
    matches!(
        s,
        Status::Pending | Status::Ready | Status::Staged | Status::Orphaned
    )
}

pub fn decide(actor: Actor, mv: Move, ctx: Ctx) -> Verdict {
    use Verdict::{Allow, Hold, Ignore, Reject};
    let Ctx {
        status,
        managed,
        spike,
        agent,
    } = ctx;
    match actor {
        Actor::Worker => match mv {
            Move::Finish | Move::Block | Move::Contest => {
                if matches!(
                    status,
                    Status::Posted | Status::InProgress | Status::InRevision
                ) {
                    Allow
                } else {
                    Ignore
                }
            }
            Move::Post
            | Move::Replan
            | Move::SendBack
            | Move::ContestReply
            | Move::Stage
            | Move::Unstage
            | Move::Accept
            | Move::Retire
            | Move::Conclude
            | Move::Resolve
            | Move::Other => Reject(Reason::Ownership),
        },
        // A task agent posts while staged, sends back while tested, replies
        // to a contest while contested; never a marker, never the plan.
        Actor::TaskAgent => match mv {
            Move::Post if status == Status::Staged && managed => Allow,
            // The director's plan moved the task out from under a running
            // agent (a replan onto an unmet dependency, a retired one): not
            // the agent's mistake, and it may come back to `staged`.
            Move::Post
                if matches!(status, Status::Pending | Status::Ready | Status::Orphaned)
                    && managed
                    && agent =>
            {
                Hold
            }
            Move::SendBack if status == Status::Tested && managed && !spike => Allow,
            Move::ContestReply if status == Status::Contested && managed => Allow,
            Move::Post
            | Move::Replan
            | Move::SendBack
            | Move::ContestReply
            | Move::Stage
            | Move::Unstage
            | Move::Accept
            | Move::Retire
            | Move::Conclude
            | Move::Resolve
            | Move::Finish
            | Move::Block
            | Move::Contest
            | Move::Other => Reject(Reason::Ownership),
        },
        Actor::Director => {
            if matches!(status, Status::Retired | Status::Explored) {
                return Ignore;
            }
            match mv {
                Move::Post => {
                    if !pending_family(status) {
                        Reject(Reason::Ownership)
                    } else if managed {
                        Reject(Reason::Ownership)
                    } else if status == Status::Ready || (status == Status::Staged && !spike) {
                        Allow
                    } else {
                        Reject(Reason::Post)
                    }
                }
                // Once an agent exists the plan is frozen for it (its prompt is
                // written at launch), as for `unstage:`: retire to change it.
                Move::Replan => {
                    if status == Status::Staged && agent {
                        Reject(Reason::Plan)
                    } else if pending_family(status) {
                        Allow
                    } else {
                        Reject(Reason::Ownership)
                    }
                }
                Move::SendBack => {
                    if status == Status::Tested && (spike || !managed) {
                        Allow
                    } else {
                        Reject(Reason::Ownership)
                    }
                }
                Move::ContestReply => {
                    if status == Status::Contested && !managed {
                        Allow
                    } else {
                        Reject(Reason::Ownership)
                    }
                }
                Move::Stage => {
                    if status == Status::Ready && !spike {
                        Allow
                    } else {
                        Reject(Reason::Plan)
                    }
                }
                Move::Unstage => {
                    if status == Status::Staged && !agent {
                        Allow
                    } else {
                        Reject(Reason::Plan)
                    }
                }
                Move::Accept => {
                    if status == Status::Tested && !spike {
                        Allow
                    } else {
                        Reject(Reason::Accept)
                    }
                }
                Move::Conclude => {
                    if !spike {
                        Reject(Reason::Ownership)
                    } else if status == Status::Tested {
                        Allow
                    } else {
                        Reject(Reason::Accept)
                    }
                }
                Move::Retire => {
                    let allowed = (status == Status::Tested && spike)
                        || matches!(
                            status,
                            Status::Pending
                                | Status::Ready
                                | Status::Staged
                                | Status::Orphaned
                                | Status::Conflicted
                                | Status::Contested
                                | Status::Escalated
                        );
                    if allowed {
                        Allow
                    } else {
                        Reject(Reason::Retire)
                    }
                }
                Move::Resolve => {
                    if status == Status::Conflicted {
                        Allow
                    } else {
                        Reject(Reason::History)
                    }
                }
                Move::Finish | Move::Block | Move::Contest | Move::Other => {
                    Reject(Reason::Ownership)
                }
            }
        }
        // The player's own commits are not judged by this gate.
        Actor::Player => Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Verdict::{Allow, Hold, Ignore, Reject};

    /// Every flag combination for `status`, spikes never managed.
    fn ctxs(status: Status) -> Vec<Ctx> {
        let mut v = Vec::new();
        for managed in [false, true] {
            for spike in [false, true] {
                if spike && managed {
                    continue; // `task_agent_managed` is false for a spike
                }
                for agent in [false, true] {
                    v.push(Ctx {
                        status,
                        managed,
                        spike,
                        agent,
                    });
                }
            }
        }
        v
    }

    fn every_cell() -> Vec<(Actor, Move, Ctx)> {
        let mut v = Vec::new();
        for a in ACTORS {
            for m in Move::ALL {
                for s in STATUSES {
                    for c in ctxs(s) {
                        v.push((a, m, c));
                    }
                }
            }
        }
        v
    }

    /// `STATUSES` lists every status: adding a variant stops this compiling.
    #[test]
    fn statuses_is_exhaustive() {
        for s in STATUSES {
            match s {
                Status::Pending
                | Status::Orphaned
                | Status::Ready
                | Status::Staged
                | Status::Posted
                | Status::InProgress
                | Status::Tested
                | Status::InRevision
                | Status::Conflicted
                | Status::Contested
                | Status::Escalated
                | Status::Accepted
                | Status::Explored
                | Status::Retired => {}
            }
        }
        let mut sorted: Vec<_> = STATUSES.iter().map(|s| s.as_str()).collect();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), STATUSES.len());
        for m in Move::ALL {
            match m {
                Move::Post
                | Move::Replan
                | Move::SendBack
                | Move::ContestReply
                | Move::Stage
                | Move::Unstage
                | Move::Accept
                | Move::Retire
                | Move::Conclude
                | Move::Resolve
                | Move::Finish
                | Move::Block
                | Move::Contest
                | Move::Other => {}
            }
        }
    }

    /// The statuses at which `actor` may do `mv` under some flags, as the
    /// table stands: the whole state machine, one line per (actor, move).
    fn allowed_at(actor: Actor, mv: Move) -> Vec<&'static str> {
        STATUSES
            .iter()
            .filter(|&&s| ctxs(s).into_iter().any(|c| decide(actor, mv, c) == Allow))
            .map(|s| s.as_str())
            .collect()
    }

    #[test]
    fn who_may_do_what_where() {
        let expect: &[(Actor, Move, &[&str])] = &[
            (Actor::Director, Move::Post, &["ready", "staged"]),
            (
                Actor::Director,
                Move::Replan,
                // not `staged` once an agent exists (any flags allow one)
                &["pending", "orphaned", "ready", "staged"],
            ),
            (Actor::Director, Move::SendBack, &["tested"]),
            (Actor::Director, Move::ContestReply, &["contested"]),
            (Actor::Director, Move::Stage, &["ready"]),
            (Actor::Director, Move::Unstage, &["staged"]),
            (Actor::Director, Move::Accept, &["tested"]),
            (
                Actor::Director,
                Move::Retire,
                &[
                    "pending",
                    "orphaned",
                    "ready",
                    "staged",
                    "tested",
                    "conflicted",
                    "contested",
                    "escalated",
                ],
            ),
            (Actor::Director, Move::Conclude, &["tested"]),
            (Actor::Director, Move::Resolve, &["conflicted"]),
            (Actor::Director, Move::Finish, &[]),
            (Actor::Director, Move::Block, &[]),
            (Actor::Director, Move::Contest, &[]),
            (Actor::Director, Move::Other, &[]),
            (Actor::TaskAgent, Move::Post, &["staged"]),
            (Actor::TaskAgent, Move::SendBack, &["tested"]),
            (Actor::TaskAgent, Move::ContestReply, &["contested"]),
            (Actor::TaskAgent, Move::Replan, &[]),
            (Actor::TaskAgent, Move::Stage, &[]),
            (Actor::TaskAgent, Move::Unstage, &[]),
            (Actor::TaskAgent, Move::Accept, &[]),
            (Actor::TaskAgent, Move::Retire, &[]),
            (Actor::TaskAgent, Move::Conclude, &[]),
            (Actor::TaskAgent, Move::Resolve, &[]),
            (Actor::TaskAgent, Move::Other, &[]),
            (
                Actor::Worker,
                Move::Finish,
                &["posted", "in_progress", "in_revision"],
            ),
            (
                Actor::Worker,
                Move::Block,
                &["posted", "in_progress", "in_revision"],
            ),
            (
                Actor::Worker,
                Move::Contest,
                &["posted", "in_progress", "in_revision"],
            ),
            (Actor::Worker, Move::Post, &[]),
            (Actor::Worker, Move::Accept, &[]),
            (Actor::Worker, Move::Other, &[]),
        ];
        let mut failures = Vec::new();
        for (a, m, want) in expect {
            let got = allowed_at(*a, *m);
            if got != *want {
                failures.push(format!("{a:?} {m:?}: got {got:?}, want {want:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        // Every (actor, move) pair not listed is never allowed.
        for a in ACTORS {
            for m in Move::ALL {
                if !expect.iter().any(|(ea, em, _)| *ea == a && *em == m) {
                    assert!(
                        allowed_at(a, m).is_empty(),
                        "{a:?} {m:?} is allowed somewhere but not in the table"
                    );
                }
            }
        }
    }

    #[test]
    fn nothing_is_allowed_that_a_flag_forbids() {
        for (a, m, c) in every_cell() {
            if decide(a, m, c) != Allow {
                continue;
            }
            // Posts, send-backs and contest replies belong to the task agent
            // when one is on; the director's are only for unmanaged tasks
            // (or a spike's send-back).
            if a == Actor::Director && c.managed {
                assert!(
                    !matches!(m, Move::Post | Move::SendBack | Move::ContestReply),
                    "director {m:?} allowed on a managed task: {c:?}"
                );
            }
            if a == Actor::TaskAgent {
                assert!(c.managed, "task agent {m:?} allowed on an unmanaged task: {c:?}");
            }
            // A spike never merges and is never staged; only a spike concludes.
            if c.spike {
                assert!(
                    !matches!(m, Move::Accept | Move::Stage),
                    "{m:?} allowed on a spike: {c:?}"
                );
            } else {
                assert!(m != Move::Conclude, "conclude allowed on a task: {c:?}");
            }
            // Unstaging is refused once an agent exists.
            if m == Move::Unstage {
                assert!(!c.agent, "{c:?}");
            }
        }
    }

    #[test]
    fn finished_tasks_take_nothing() {
        for (a, m, c) in every_cell() {
            if a == Actor::Player {
                continue;
            }
            match c.status {
                Status::Retired | Status::Explored => {
                    if a == Actor::Director {
                        assert_eq!(decide(a, m, c), Ignore, "{a:?} {m:?} {c:?}");
                    }
                    assert_ne!(decide(a, m, c), Allow, "{a:?} {m:?} {c:?}");
                }
                Status::Accepted => {
                    assert_ne!(decide(a, m, c), Allow, "{a:?} {m:?} {c:?}");
                }
                _ => {}
            }
        }
    }

    /// The reason a gate failure is recorded under: pinned per move, so a
    /// change of strike category is a visible edit here.
    #[test]
    fn gate_failures_use_the_moves_own_reason() {
        let at = |status, managed, spike, agent| Ctx {
            status,
            managed,
            spike,
            agent,
        };
        let d = |m, c| decide(Actor::Director, m, c);
        assert_eq!(d(Move::Accept, at(Status::Posted, false, false, false)), Reject(Reason::Accept));
        assert_eq!(d(Move::Accept, at(Status::Tested, false, true, false)), Reject(Reason::Accept));
        assert_eq!(d(Move::Retire, at(Status::Posted, false, false, false)), Reject(Reason::Retire));
        assert_eq!(d(Move::Stage, at(Status::Staged, true, false, true)), Reject(Reason::Plan));
        assert_eq!(d(Move::Unstage, at(Status::Staged, true, false, true)), Reject(Reason::Plan));
        assert_eq!(d(Move::Post, at(Status::Pending, false, false, false)), Reject(Reason::Post));
        assert_eq!(d(Move::Post, at(Status::Ready, true, false, false)), Reject(Reason::Ownership));
        assert_eq!(d(Move::Resolve, at(Status::Tested, false, false, false)), Reject(Reason::History));
        assert_eq!(d(Move::Conclude, at(Status::Ready, false, true, false)), Reject(Reason::Accept));
        assert_eq!(d(Move::Conclude, at(Status::Tested, false, false, false)), Reject(Reason::Ownership));
        // A replan is refused once the agent exists, allowed before.
        assert_eq!(d(Move::Replan, at(Status::Staged, true, false, true)), Reject(Reason::Plan));
        assert_eq!(d(Move::Replan, at(Status::Staged, false, false, false)), Allow);
        assert_eq!(d(Move::Replan, at(Status::Pending, true, false, true)), Allow);
        // A task agent's post after the task left `staged` is held, not struck.
        for status in [Status::Pending, Status::Ready, Status::Orphaned] {
            assert_eq!(
                decide(Actor::TaskAgent, Move::Post, at(status, true, false, true)),
                Hold
            );
        }
        assert_eq!(
            decide(Actor::TaskAgent, Move::Post, at(Status::Posted, true, false, true)),
            Reject(Reason::Ownership)
        );
    }
}
