# Handover: wire the player's dispatch to `fsm::decide`

Status: `src/fsm.rs` exists and is tested, but the player still decides most
of its status gates inline. Two decisions already go through it (a director's
replan, a task agent's hold). Your job is to move the rest, so `fsm` is the
one place that says who may do what at which status, and the tests in
`fsm.rs` are a real contract instead of a description.

Don't add a static JSON of the machine to the skill or the docs (decided):
the Rust module is the single source of truth.

## What exists

`src/fsm.rs`: `decide(actor, move, ctx) -> Verdict`.

- `Actor`: `Director`, `TaskAgent`, `Worker` (`Player` is always `Allow`).
- `Move`: `Post, Replan, SendBack, ContestReply, Stage, Unstage, Accept,
  Retire, Conclude, Resolve, Finish, Block, Contest, Other`. By intent, not
  by commit shape. Planning has no branch, so it is `may_plan`.
- `Ctx { status, managed, spike, agent }`: `managed` is
  `state::task_agent_managed`, `agent` is `task.agent.is_some()`
  (`refs/dashboard/agent/<slug>` exists). A spike is never managed.
- `Verdict`: `Allow`, `Reject(Reason)` (the strike category), `Ignore`
  (not looked at, no strike), `Hold` (not imported, no strike, judged again
  each tick).
- `decide` has no wildcard arm on `Actor` or `Move`; the tests walk every
  `Status x Actor x Move x flags` cell: `who_may_do_what_where` (the table of
  allowed statuses per actor and move), `nothing_is_allowed_that_a_flag_forbids`,
  `finished_tasks_take_nothing`, `gate_failures_use_the_moves_own_reason`,
  `statuses_is_exhaustive`.
- `Player::fsm_ctx(&hub_state, slug) -> Option<fsm::Ctx>` builds the context.

Already wired:

- `try_existing`, pending-family arm: a replan asks
  `decide(Director, Replan, ctx)`; `Reject(Plan)` when the task is `staged`
  and an agent exists.
- `try_agent_commit`, the `other =>` arm: `decide(TaskAgent, Post, ctx) ==
  Hold` leaves the commit unimported and tells the agent once per commit
  (`agent_hold_told`, template `task-agent.hold`). Practically unreachable
  today (a staged task can no longer go back to pending); it is a guard.

## What is not wired (each is an inline status check today)

Replace each with a `decide` call, keeping the message text and the strike
reason it produces now (the `gate_failures_use_the_moves_own_reason` test
pins the reasons; if the code and the table disagree, one of them is wrong:
decide which and say so).

| Where (`src/player.rs`) | Gate today | `Move` |
| --- | --- | --- |
| `try_existing`, pending-family arm | managed post -> `ownership`; prompt.md -> `try_post` | `Post` |
| `try_existing`, `Tested`/`Contested`/`Conflicted`/`other`/`None` arms | managed -> `ownership`; spike -> spike send-back | `SendBack`, `ContestReply`, `Other` |
| `try_marker` | subject prefix -> `try_accept`/`try_retire`/`try_conclude`/`try_stage` | `Accept`, `Retire`, `Conclude`, `Stage`, `Unstage` |
| `try_accept`, `try_retire`, `try_conclude`, `try_stage` | their own `status != ...` checks | same |
| `validate_post`, `validate_spike_post` | `status != Ready && != Staged` (and the spike's `Ready` only) | `Post` |
| `process_task_commits` -> `try_resolve` | the task must have a recorded conflict, else `history` | `Resolve` |
| `import_workers` | only `Posted`/`InProgress`/`InRevision` are listened to | `Finish`, `Block`, `Contest` |
| `try_agent_commit` | `Staged` / `Tested` (non-spike) / `Contested` / `other` | `Post`, `SendBack`, `ContestReply` |

Also the status lists that mean the same thing without being gates; fold
them in or leave them, but make the choice deliberately:
`owes_work` (`src/player.rs`, task agent: `Staged | Tested | Contested`;
worker: `Posted | InProgress | InRevision`), `run_task_agents` (the
`AgentStep` match), `Player::relaunch` (the worker's statuses),
`agent_step` (the `AgentStep` per status).

## Cells in the table I was least sure of (verify while wiring)

- A director post/replan at a status after posting (`tested`, `posted`, ...)
  is shape-dispatched in the player (a `prompt.md` commit at `tested` goes to
  `try_send_back`). The table says `Reject(Ownership)` for it. Check what the
  player really does; if the answer differs, either the table is wrong or the
  shape dispatch has a hole. Write the failing case first.
- A worker move outside `posted`/`in_progress`/`in_revision` is `Ignore`
  (`import_workers` filters the tasks). Confirm there is no rejection path
  for e.g. a worker pushing at `contested`.
- `Retire` at `tested` is `Allow` only for a spike; `Conclude` on a
  non-spike is `Reject(Ownership)` but at a wrong status on a spike it is
  `Reject(Accept)`.
- Retired and explored: `Ignore` for the director. Accepted: the table gives
  `Reject(...)` for every director move; the player has no special case
  except the `Some(other)` ownership arm.
- A `Resolve` is a merge commit, caught before `try_existing`
  (`process_task_commits`); it never reaches the status match.

## Method

1. For each row above, first add a test (in `fsm.rs` if it is a pure cell, in
   `tests/` if it needs the player) that pins today's behaviour. Run it
   against the unmodified code. If it fails, you found a table error or a
   bug: report which.
2. Then replace the inline check with `decide` and keep the test green.
3. Delete the inline check only when its test passes through `decide`.
4. After every group: `cargo test --no-fail-fast`, and `scripts/e2e.sh
   target/debug/dashboard > target/e2e.log 2>&1` (it is the only test that
   uses real tmux).
5. A `Verdict::Hold` is the only new kind of outcome the player handles
   (`try_agent_commit`); if another row needs one, add the template to
   `src/prompts.kdl` and `src/prompts.rs` (`TEMPLATES`), as `hold` did.

## Don't

- Don't fold content checks (pins, suite, paths, a post's tests,
  `agent_paths_ok`) into `fsm`; it is only the status/actor gate.
- Don't change a strike reason silently. `Reason` limits default to 2
  (`ownership` and `plan` included), so changing one changes when a task
  escalates.
- Don't make `decide` read the hub. `Ctx` is the whole input; the caller
  builds it (`fsm_ctx`).

## How to run

```
export TMPDIR=$HOME/.cache/dash-test-tmp     # /tmp quota; see the memory note
cargo build
cargo test --lib fsm                          # the table
cargo test --test staged --test task_agent    # fast sanity
cargo test --no-fail-fast > target/test-all.log 2>&1
scripts/e2e.sh target/debug/dashboard > target/e2e.log 2>&1
```

## Report

Per row: wired / left inline (why), any table cell you changed and the
behaviour change it implies, the tests added, and the final `cargo test`
and e2e results. Update the repo layout list in `CLAUDE.md` (`src/fsm.rs`)
and the "Decisions" text if a gate's behaviour changes.
