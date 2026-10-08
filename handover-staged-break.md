# Handover: break the `staged` flow, end to end

Status: `staged` is implemented and committed (`616e9f6`). `cargo test` and
`scripts/e2e.sh` pass. Your job is to find a way to make it misbehave with
the real binary and real `tmux`. **Stop at the first finding** and report it
(failure scenario, evidence, which file/line); don't fix it unless asked.

`handover-staged.md` is the superseded design (staged = *held back*). Ignore it.

## The design as built

- A `ready` task **never starts on its own.** The director releases it with an
  empty `stage: <why>` commit on `task/<slug>`; the status becomes `staged` and
  `run_task_agents` launches the task agent (`src/player.rs`, `Status::Staged
  => AgentStep::Post`).
- `unstage:` puts a `staged` task back to `ready`, but is refused (`plan`
  rejection) once a task agent exists (`task.agent.is_some()`), so it is almost
  unreachable with agents on.
- Only a `ready` non-spike task can be staged; only a `staged` one unstaged.
  Anything else is a `plan` rejection (`Player::try_stage`). A task agent's
  marker is `ownership`. Director posts on a managed task stay `ownership`.
- A task agent may post only while `staged` (`try_agent_commit`); at any other
  status its commit falls through to the `_` arm and is rejected.
- Status derivation: `state::staged_at` walks `git rev-list --first-parent
  <tip>`, skipping `merge_unblock` and `replan` notes, to the last `stage` /
  `unstage` (or plan/none = not staged). It only runs for a task that would
  otherwise be `ready`, so `pending`/`orphaned` outrank it.
- `unblock_merges` keeps merging `main` into `staged`/`unstaged` branches
  (the tip-note gate includes `Stage` and `Unstage`).
- Events: a managed `ready` task emits `ready` ("waits for a `stage:`"); no
  event for `staged`. `owes_work` for a task agent is `staged`/`tested`/
  `contested`.
- Files: `src/state.rs` (`staged_at`, `Status::Staged`), `src/player.rs`
  (`try_stage`, `try_marker`, `run_task_agents`, `validate_post`,
  `owes_work`, `status_event`, `unblock_merges`), `src/render.rs` (Staged
  list), `tests/staged.rs`, `scripts/e2e.sh` (step 7b).

## Already exercised (don't repeat)

- plan -> ready -> no launch across ticks -> `stage:` -> launch, with a
  `ready` event.
- staged survives `main` moving twice (the `merge_unblock` tip) and a replan.
- `stage:` twice, on a pending task, on a spike; `unstage:` on ready; `unstage:`
  with the agent running; `unstage:` with agents off (works).
- a task agent's `stage:` is `ownership`.
- an agent launched, then its post is imported only after rebasing onto the
  stage commit (the first post, made before the rebase, gets the usual
  rebase request with no strike).
- a dependent task waits `ready` after its dependency is accepted, then
  `stage:` launches it.
- full e2e with a fake task agent and a fake worker.

## Attacks not tried yet

Pick whichever looks most likely to break; each is a real-binary scenario.

1. **Same-push sequences.** `stage:`, `unstage:`, `stage:` pushed together in
   one tick (before any agent exists, e.g. with agents off, then turn agents
   on). Does the walk and `try_stage`'s `status` argument (read once per
   candidate) stay consistent as each commit is imported in turn?
2. **Agents off -> on.** Stage tasks while `task_agent_cmd = ""`, then commit
   a roadmap that turns agents on. Do staged tasks launch? Do `ready` ones
   stay still? Reverse: agents on -> off with a live staged agent.
3. **Stale director clone.** The player merges `main` into a staged branch;
   the director stages/unstages from a clone that lacks that merge. Is the
   rejection a sensible `history` one, and does the next valid commit recover?
4. **Replan to an unmet dependency while staged**, with the agent already
   running: status goes `pending`; the agent's post must be rejected; when the
   dependency lands the task returns as `staged` and the *existing* agent
   should be able to post. Also: dependency retired while staged
   (`orphaned`), then a replan that drops it.
5. **Retire a staged task with a live agent** (and with a dead one): session
   and worktree cleanup, status, `refs/dashboard/agent/<slug>`.
6. **Launch failure / dead agent while staged.** Bad `task_agent_cmd`: agent
   recorded as launched but dead. `dashboard relaunch <slug> --agent` at
   `staged` (step `Post`), the relaunch cap, `agent_died` hint, then
   `unstage:` (should be refused: an agent exists even though dead).
7. **Player restart** between `stage:` import and launch, and with a
   persisted `last` status of `staged` in `player-state.json`: spurious or
   missing events? `staged -> ready` can't happen with an agent, so check
   `resumed`/status-event paths don't misfire.
8. **Non-empty `stage:` commit** (a file in it) and a `stage:` commit that
   is a merge commit: must not be imported as a marker.
9. **Parallelism.** Several staged tasks at once, one accepted while another
   is staged; accept's `sync`/`merge_unblock` interplay with the first-parent
   walk (a `Sync` note on a pending-family tip should never occur, but
   confirm `staged_at` returns `false` rather than misreading it).
10. **Agents off, director posts.** `stage:` then the director's own post
    at `staged` (`validate_post` allows it): `staged -> posted`, worker launch.
11. **Rendering.** `dashboard status` with staged tasks in hidden phases, in
    a phase with other statuses, narrow `--width`; the Overall counts
    (`LIVE_STATUS_ORDER` now has 12 entries).
12. **`dashboard wait` hints** for `ready` on a managed vs spike vs unmanaged
    task: the hint text is static per kind, check it still reads right.

## How to run

```
export TMPDIR=$HOME/.cache/dash-test-tmp     # /tmp quota; see the memory note
cargo build
cargo test --test staged --test task_agent   # fast sanity
scripts/e2e.sh target/debug/dashboard > target/e2e.log 2>&1
```

For a real-binary scenario, copy the shape of `scripts/e2e.sh` (throwaway
director repo, `dashboard --home $H init`, `dashboard player --once` as the
tick, `dashboard status --no-color` to assert). Practical notes learned:

- Use an idle script (`#!/bin/sh\nsleep 300`) as both `agent_cmd` and
  `task_agent_cmd` when you only need sessions to exist; then play the agent
  yourself by committing in `<director>/.dashboard/agents/<slug>` (branch
  `agent/<slug>`) and the worker in `.dashboard/workers/<slug>`.
- After the player moves the hub tip (stage, merge_unblock), an agent's
  unimported commit needs `git fetch origin && git rebase origin/task/<slug>`
  before it is judged (the first attempt only earns a rebase request).
- The director must `git fetch hub task/<slug> && git merge --ff-only
  FETCH_HEAD` before committing on a branch the player has moved.
- The first tick after `init` is a baseline: it emits no events. Tick once
  before the change you want an event for.
- Two `plan` strikes escalate a task (default limit 2): don't chain rejected
  markers on one task in a single scenario.
- Rejections are in `refs/rejected/<scope>/<n>` with a note under
  `refs/notes/dashboard-rejections` on the hub (`$H/hub.git`).
- Kill leftover tmux sessions: `tmux ls | grep ^dash-<project>-`.

## Report format

One finding: the scenario (commands or test), observed vs expected, the hub
refs/notes that prove it, and the code location. Don't commit repro tests
unless asked; if you add one, say it asserts the buggy behavior.
