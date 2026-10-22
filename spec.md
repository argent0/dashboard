# Director / Player / Worker spec

Working spec for evolving `dashboard` from a stateless `tasks.yaml` renderer
into the enforcement point for a worker-per-task roadmap loop, with all
roadmap state derived from git. Greenfield: no backward compatibility with
`tasks.yaml`, existing consumer roadmaps, or the current `dashboard` skill.

## 1. Baseline (today)

`dashboard` is a stateless CLI that renders `tasks.yaml` (progress bar,
actionable-now list, blocked/in-revision sections). All orchestration around
it — spawning workers, judging their work, editing task state — happens by
hand inside a single Claude Code coordinator session following the
`dashboard` skill's prose. Status is whatever someone typed into the YAML.
This spec replaces that with state computed from git.

## 2. Roles

- **Operator** — the human. Describes the goal to the director in
  conversation; resolves escalations with the director; may set or override
  thresholds for non-deterministic tests.
- **Director** — an LLM (Claude Code session). Designs tasks, sets
  dependencies, writes each task's prompt and failing test, owns the
  project's test suite (including retiring stale tests), reviews finished
  work by accepting it or sending it back with a new failing test, and
  carries out the replanning agreed with the operator after an escalation.
- **Task agent** — an LLM (Claude Code session), one per task, launched by
  the player: the director's delegate for that task, writing its post and
  send-backs and reviewing the finished work (§17).
- **Worker** — an LLM (Claude Code session), one per task, launched by the
  player. Writes code (and may add new tests) inside its task's modules
  until the task's run set (§6.6) passes, then commits — or blocks.
- **Player** — a process; `dashboard`'s new responsibility. The only writer
  of the canonical repo. Imports and validates every commit, runs every
  test itself, launches workers, counts strikes, merges accepted work,
  retires dead tasks. Never trusts a narrated claim — only a test it ran
  itself.

## 3. Core philosophy

### 3.1 Evidence over narration

Two failure modes motivate this:

1. A worker, eager to finish, claims success without it being true.
2. A director rejects work without real grounds.

Both are closed by requiring every claim to be a committed, currently
checkable test:

- The director cannot **post** a task until it has committed a test that
  currently *fails*.
- The worker cannot **finish** a task until every check that can see its
  change (its run set, §6.6) *passes*.
- **Sending back** requires a new failing-test commit, not a prose objection.
- **Accepting** is also a commit, not silence.

Nobody writes a status. Status is a pure function of the repo (commits,
branches, pins, check results, rejections) plus whether the worker's
session is alive. Director and worker never talk directly; git is the only
channel.

### 3.2 Dead ends are normal

Design iteration means some tasks are bound to fail: the approach doesn't
work, the test was wrong, a prerequisite was missing. That is an expected
outcome, not an error. A task that can't be finished is **escalated**, then
**retired** — never forced through, never resumed in place. The operator and
director replan around it (remove tasks, add new ones), and the lesson can
be written into the project's own docs ("we do X this way because Y didn't
work"). Retired tasks stay in the hub as history.

### 3.3 Threat model

All agents run as the same Unix user, so none of this is secure against a
determined adversary. It targets eager or careless LLM behaviour: cheating
must take deliberate effort and must be visible in the history. Real
isolation (separate users, containers) can be layered on later without
changing the model.

## 4. Repository topology

```
director clone ──┐
worker clone ──┼─(player fetches, validates)─► hub (bare) ─► dashboard renders from hub
                 │                                 ├─ refs/notes/dashboard: cached check results
                 │                                 ├─ refs/rejected/…: rejected commits
                 │                                 └─ refs/retired/…: retired task branches
tmux session  ◄──┘ (player launches the worker, relays rejections/revisions)
```

- **Hub** — a bare repository owned by the player. The only source of truth.
  Only the player updates its refs.
- **Clones** — the director has one clone; the worker gets its own clone
  of the hub checked out on its task branch, holding its task's files only
  (§21), and so does the task agent
  (§17). Separate clones, not
  worktrees: worktrees share refs. Agents fetch from the hub freely but
  never push.
- **Provenance by fetch source.** The player pulls new commits from each
  clone. Commits fetched from the director's clone are the director's;
  commits fetched from the worker's clone are the worker's. No tokens or
  author fields are needed to establish identity, and there is nothing to
  leak or forge in commit content.
- **Parallel tasks.** Any number of tasks may be posted and in flight at
  once, so `main` moves under in-flight tasks as others are accepted. The
  player merges the moved `main` into a task itself when it lands the
  task's accept (§11). Only a textual conflict needs the director, who
  takes on that risk by planning tasks side by side: the task goes
  `conflicted`, and the director resolves that specific conflict.

## 5. History rules

Enforced by the player on import; a violating commit is never imported.

- **Fast-forward only.** Every imported ref update must be a descendant of
  the hub's current ref. No rebase, amend, reset of published commits, or
  force push.
- **No merge commits from agents.** The only merge commits are the
  player's own and the director's resolution of a recorded conflict (§11).
- **Path ownership.** A worker commit must leave every pin (§8.3)
  unchanged and stay inside the task's scope. The director commits to a
  task branch only through the transitions in §9.
- **`main` is player-only**, except for director commits touching only
  `roadmap.toml`, `prompts.kdl` (§19), the Lean package `architecture/`
  or the doc paths declared in `roadmap.toml` (`docs_paths`). The player runs the whole suite on these
  like any other `main` commit.

**Marker commits.** A few transitions are events with no content: *block*,
*accept*, *retire*. They are empty commits (`--allow-empty`) whose subject
starts with the transition name (`block: …`, `accept: …`, `retire: …`); the
body carries the explanation. Provenance (§4) says who made it, the empty
diff says it isn't work, and the prefix says which event it is. This is the
only commit-message convention; all other messages are free-form, short
and topical.

**Rejection.** The player leaves the hub branch untouched, stores the
offending commit under `refs/rejected/<slug>/<n>` as evidence, records the
reason so `dashboard` shows it, and relays it to the worker's tmux session
when the worker is the author. The agent recovers in its own clone with a
fixing commit on top for content violations (e.g. restoring a pinned file).
A history violation goes back to the hub ref: the player resets a clean
clone there itself (tree sync, below), and an agent with changes of its own
runs `git reset --hard` to it. Each rejection is a strike in its reason's
counter (§10).

**Tree sync.** In an agent's clone the player writes a worker's sparse
patterns (§21) and syncs the tree at step boundaries: right before it
pastes a message that starts the agent's next step from the hub's
`task/<slug>`, it brings the clone there itself, so the message says the
tree is already at that tip.

| Message | Sync |
| --- | --- |
| task agent: post, review, contest steps | fast-forward |
| worker: review, contest reply, merge failed | fast-forward |
| task agent: the hub moved under its commit (§17) | rebase of its commit |
| worker: held finish released (§20) | rebase of its finish |
| either: a `history` rejection | `reset --hard` |

The player acts only on a clean clone (no change to a tracked file, no
untracked file beyond ignored ones) still on its own branch, and only by a
fast-forward, a rebase without conflicts or a reset. Otherwise, or when the
rebase conflicts (then aborted), it leaves the tree exactly as it was and
the message carries the git command for the agent to run. These messages
answer a commit or a contest the agent made and waits on, so the agent is
idle when its tree moves. The player's git there runs without the clone's
hooks or fsmonitor, and a rebased commit keeps its author, with the player
as committer; it is then imported like any other commit of that agent.

## 6. Tests

### 6.1 Checks and thresholds

The unit of evidence is a **check**: a shell command plus a threshold,
declared in Lean (§6.2).

```lean
check unit where
  cmd = "cargo test -p database"     -- deterministic: runs = 1, min_pass = 1 (defaults)
  covers = [database]
  files = ["engine/crates/database/tests"]

check «summary-grounding» where
  cmd = "pytest tests/llm/test_summary.py"
  covers = [summary]
  files = ["tests/llm/test_summary.py"]
  runs = 10
  min_pass = 8
```

The player runs `cmd` `runs` times; the check **passes** when at least
`min_pass` runs exit 0, and **fails** otherwise. A deterministic test is the
degenerate case `runs = 1, min_pass = 1`. This makes non-deterministic tests
(e.g. LLM-backed) yield a deterministic verdict the whole model can rely on.

Thresholds are task-dependent. The director sets them when it writes the
check; the operator may set or override them (the director commits the
operator's choice). They live in pinned files, so the worker can never
lower a threshold to get a pass.

A threshold check can still fail by chance on a correct implementation.
That shows up as a `suite` strike (§10), whose limit can be set higher than
the others to absorb it.

### 6.2 The project suite

The suite is part of the architecture's Lean package (`architect-v3.md`),
in three kinds of file:

- `architecture/Tests.lean` (director-owned): the project's checks, plus
  `test_paths`, the files that could defeat a test without belonging to one
  (fixtures, `conftest.py`, test config), and `list_cmd` (§15).
- `architecture/Tests/<slug>.lean`: the checks a task's post adds (§6.3),
  written by the director or its task agent.
- `architecture/Tests/<slug>.worker.lean`: the checks the task's worker
  adds (§6.6).

Per-task files keep parallel tasks (§4) apart: each writes its own, and two
tasks only conflict in the suite when they really change the same check.

Every check declares:

- `covers` — the modules (and roots) its test exercises. Lean resolves each
  name against the manifest, so a check covering an undeclared module is a
  Lean error. `covers = all` marks a global check (`dashboard arch check`,
  budgets, lints): it runs in every run set, in the full worktree.
- `files` — the check's own test code: what pins protect, what scope
  allows the check's author to write, and what a trim deletes.

Each module's `test_cmd` is an implicit check named after the module,
covering exactly that module. Check names share one Lean namespace, so a
duplicate is a Lean error.

**The suite passes** when every check passes. Once a task is merged its
checks and test files are project tests, not "that task's tests". The
invariant is that the project suite passes at every commit on `main`; the
player keeps it while running only run sets (§6.6).

**Each check proves its `covers`.** A check (other than `covers = all`)
runs in a view built like a module's (`architect-v3.md`): the covered
modules' files, their APIs' files and `impl` files, `shared`, `test_paths`
and the check's own `files`, plus the `linked` symlinks, all inside the jail
(§20). A test that reaches into a module outside its `covers` finds nothing
there and fails like any failing check. For a worker's own check that is a
`suite` strike; for a pinned check the worker contests it (§18).

### 6.3 The task check

The task check is the thing that must fail at post and is the task's reason
to exist. The post commit declares it, once, in
`architecture/Tests/<slug>.lean`: the checks of that file are the task check,
so while the task runs it is part of the suite. The player reads it from
there wherever it needs it: at post at least one of them must fail, and at
launch the task is held while all of them pass. `task.toml` stays the
director's plan and carries no `[check]` (a flaky-test-fix's stability
`[check]` is the one exception, §15, since it stays out of the suite).
Whether those checks stay after accept is the director's call (§6.5); the
post commit in the branch's history keeps the record of what the task asked.
Every check in that file covers only modules in the task's `modules`.

### 6.4 Stale tests

A task may legitimately change behaviour an existing test asserts. Deciding
that a test is stale — rather than inconvenient — is a design decision and
belongs to the director, never the worker:

- **Foreseen:** the director updates or deletes the stale test (and its
  declaration) in the task's post commit, visible in
  the diff. A stale test outside the task's run set (§6.6) needs a task
  whose `modules` reach it.
- **Found mid-task:** the worker cannot touch pinned tests, so it
  **blocks**, explaining the conflict. That escalates the task (§10); the
  director and operator replan, typically retiring it and posting a
  replacement whose post commit fixes the stale test.

### 6.5 Trimming before accept

A task's tests drive its work; only some of them guard behaviour the
application depends on. Those are the ones worth keeping in the suite, where
every later task runs them. The rest (scaffolding for one step, a case a
broader test now covers) cost suite time for nothing once the task lands.

The director decides which stay. The task agent's review report recommends
keep or drop for each check the task added, the worker's included (§17);
before `accept:` the director (or the task agent, at its request) makes a
**trim** commit on the `tested` task that:

- deletes or edits the `files` of the dropped checks (absent on `main`),
  and
- removes their entries from `architecture/Tests/<slug>.lean` and
  `architecture/Tests/<slug>.worker.lean` (or a whole file).

Everything else stays: tests already on `main`, `tasks/<slug>/task.toml`,
and the dropped tests themselves (with the post's task check, the record of
what the task asked) in the branch's history. The run set must still
pass at the trim. The task stays `tested`, re-pinned at the trim; accept
then lands the trimmed branch. A task with nothing to drop needs no trim.

### 6.6 Scoped runs

A task runs only the checks that can see its change.

- **Modules.** `task.toml` declares `modules = [...]`, the modules and roots
  the task changes. The director sets it at plan and may change it by
  replan until the post; from the post on it is pinned. A worker that needs
  another module blocks (§7), and the director replans. An ordinary task
  always names its modules. A spike (§14), a flaky-test-fix (§15) and a
  test-speedup (§15.1) need none: the last two work on the suite itself and
  run the whole of it, and a spike runs no suite.
- **Run set.** The player computes it in Lean: a generated runner applies
  `Dashboard.impacted` to the task's `modules`, so an undeclared name is a
  `plan` rejection. The run set is every check whose `covers` meets
  `modules`, plus every `covers = all` check. A commit that touches `shared`
  runs the whole suite. The result is cached by the architecture tree and
  the module list.
- **Worker checks.** The worker may add tests wherever the project keeps
  them, declared in `architecture/Tests/<slug>.worker.lean` with `covers`
  within `modules`. Their `files` and that file stay the worker's to edit
  until the trim. A worker check joins the run set as soon as it is declared.
- **Where run sets apply.** Every task-level judgement uses the task's run
  set: finish, send-back and trim (§9).
  Landing an accept runs `impacted(the task's modules ∪ the modules main
  changed since the branch last saw it)` on the merge result (§11).
- **Where the whole suite runs.** At player start, on `main` (the
  `verify_main` run, §20), and on every director commit to `main`. A
  failing `main` holds work like an over-budget one until it is fixed
  (§20).
- **Why a run set is enough.** Scope (§8.3) keeps a task's changes inside
  its modules, and each check's view (§6.2) keeps its test inside its
  `covers`, so a check outside the run set sees the same files before and
  after the task. What a view leaves out (toolchain, `linked` caches, the
  host) is what the start run catches.
- **Tools.** In any clone, `dashboard scope` prints the task's modules,
  the paths its author may write (§8.3), and its run set; `dashboard test`
  runs that run set the way the player does (views, jail, the hub's check
  cache), so an agent sees the player's verdict before committing. With
  `[player] runs = "queued"` the player runs it in its queue; otherwise the
  command runs it itself, in its own worktree and views beside the
  player's. A branch with no plan runs the whole suite. `dashboard test`
  takes check names and `dashboard build` compiles the task's modules in
  their views (§21).

## 7. Worker commits

The worker commits only when it believes it is done. Every worker
commit is therefore one of:

- a **finish claim** — any non-empty commit. The player checks it (§9);
  it either becomes `tested` or is rejected.
- a **block** — a `block:` marker commit whose body explains why the task
  can't be finished as specified (stale test, missing prerequisite, wrong
  approach, needs a human decision).
- a **contest** — a `contest:` marker commit: a test it must pass is wrong
  (unsatisfiable, or over-specified); the task goes back to the test's
  author instead of escalating (§18).

Work in progress stays uncommitted in the worker's clone.

## 8. Task layout

### 8.1 Branches and names

One branch per task, `task/<slug>`, where `<slug>` is a human-chosen,
stable name fixed at branch creation. Future tasks are branches too: the
director creates a task's branch as soon as it plans the task, so the whole
roadmap — not just in-flight work — is visible in the hub. A retired task's
branch moves to `refs/retired/<slug>`; its slug is never reused.

### 8.2 Files

```
roadmap.toml                   on main: title, phases (id, title, goal, hidden),
                               docs_paths, [limits] (§10)
prompts.kdl                    on main: every prompt the player sends (§19)
architecture/Tests.lean        project suite: checks, test_paths, list_cmd (§6.2)
architecture/Tests/<slug>.lean the checks a task's post adds: its task check (§6.3)
architecture/Tests/<slug>.worker.lean   the checks the task's worker adds
tasks/<slug>/task.toml         title, phase, milestone, depends_on, modules, brief (a flaky-test-fix adds its [check], §15)
tasks/<slug>/prompt.md         director → worker instructions
tasks/<slug>/reviews/r<N>.md   director's send-back notes, round N
```

When a task is accepted, `tasks/<slug>/` lands on `main` and stays there as
a permanent, immutable record of what the task asked for and how it was
reviewed. Blocks, acceptances and retirements are marker commits (§5), not
files.

### 8.3 Pins and scope

Pins are computed by the player as late as possible: immediately before
the worker starts work — at launch, and again before relaying a
send-back. Until then the director may keep refining tests freely.

The player records git's blob hash (`git rev-parse <tip>:<file>`) of every
file that exists at that moment under:

- `architecture/` (the manifest, the APIs and every suite file), except the
  task's own `Tests/<slug>.worker.lean`,
- the `files` of every check other than the worker's own, and `test_paths`
  (the whole test tree),
- `tasks/<slug>/`.

Every worker commit must leave each pinned file present and byte-identical
— mechanically checked, no trust involved.

**Scope** is what the worker's commits may change at all, derived from the
task's `modules` (§6.6) at the hub tip, so a commit can't widen its own:

- the paths of the task's modules and roots (an API's `impl` files lie
  there), and `shared`,
- `architecture/Tests/<slug>.worker.lean` and the `files` its checks
  declare, new or existing, anywhere the project keeps tests.

A pinned file changed is a `pins` strike; any other path outside scope, or
a worker check whose `covers` leaves `modules`, is a `scope` strike. The
worker's prompt names `dashboard scope` (§6.6), which prints exactly this
area and the working tree it sits in (§21).

Pins and scope close tampering with the tests themselves, and work outside
the task's modules, not every way of beating a test (e.g. special-casing
the code under test, or a new file that monkeypatches). Catching that is
the director's review job.

## 9. Transitions

Each transition is a commit the player validates before importing. A
failed validation is a rejection with the listed reason (§10).

| Transition | Commit (author) | Player checks | Rejection reason |
|---|---|---|---|
| **plan** | new branch `task/<slug>` from `main`, adding `tasks/<slug>/task.toml` (director) | slug unused (incl. retired); `depends_on` slugs exist and aren't retired; no cycles; `modules` non-empty for an ordinary task, each a module or root of `main`'s architecture (§6.6) | `plan` |
| **replan** | edits `tasks/<slug>/task.toml` on a not-yet-posted branch (director) | same as plan | `plan` |
| **post** | adds `prompt.md`, the test files, `architecture/Tests/<slug>.lean` holding the task check (§6.3), stale-test updates (director, or its task agent §17) | all `depends_on` accepted and merged into the branch (§11); `Tests/<slug>.lean` has at least one check, each with `covers` within `modules`; task check **fails** (one of its checks fails) | `post` |
| **launch** | — (player) | task check still **fails** at the tip (one of its checks fails); records pins; starts `tmux` session running `claude` in a fresh worker clone with `prompt.md` | — |
| **finish** | code, optionally new tests (worker) | pins unchanged; inside scope (§8.3); run set (§6.6) **passes** | `pins`, `scope`, `suite` |
| **block** | `block:` marker (worker) | — | — (escalates) |
| **contest** / **reply** | `contest:` marker (worker); the reply (task agent or director) | §18 | §18 |
| **send back** | adds `reviews/r<N>.md` + new/updated tests (director, or its task agent §17) | task is `tested`; run set now **fails**; re-pins; relays the note | `review` |
| **trim** | deletes/edits the `files` of checks the task added, removes their entries from `architecture/Tests/<slug>.lean` and `<slug>.worker.lean`; leaves `tasks/<slug>/` alone (director, or its task agent §17) | task is `tested`; touches nothing else; both files only lose entries; run set **passes**; re-pins; stays `tested` (§6.5) | `accept` |
| **accept** | `accept:` marker (director) | task is `tested`; the player lands it (§11), merging `main` in first if it moved: a conflict leaves it `conflicted`, a suite failure caused by the merge sends it back to the worker | `accept` |
| **resolve** | merge of the recorded `main` commit into the task tip (director) | task is `conflicted`; merges exactly the `main` commit the conflict was recorded against; differs from git's own merge only on the recorded paths; re-pins; the player then lands the accept (§11) | `history`, `ownership` |
| **retire** | `retire:` marker, body = post-mortem (director) | task is not posted or in flight, or is conflicted, contested or escalated; player moves the branch to `refs/retired/<slug>` and ends the worker session | `retire` |

Any agent commit that breaks the history rules (§5) is rejected with reason
`history`, whatever transition it was meant to be; a director commit that
fits no transition is rejected with reason `ownership`.

The worker's tmux session persists across send-backs, so the same
worker, with its context, handles them. It ends when the task is accepted
or retired.

Check results (check, commit, per-run exit codes) are cached as git notes
on `refs/notes/dashboard` in the hub, so rendering never re-runs tests and
the cache can be wiped and recomputed.

## 10. Strikes and escalation

Each rejection reason has its own independent strike counter per task, and
its own limit in `roadmap.toml` under `[limits.strikes]`:

| Reason | Actor | Meaning | Default limit |
|---|---|---|---|
| `history` | either | rebase, amend, force push, merge commit | 2 |
| `ownership` | director, task agent | commit that fits no transition, or touches paths it may not | 2 |
| `pins` | worker | finish claim modified or deleted a pinned file | 2 |
| `scope` | worker | finish claim touching a path outside the task's scope, or declaring a check that covers a module outside `modules` (§8.3) | 2 |
| `suite` | worker | finish claim where the task's run set fails | 2 |
| `plan` | director | invalid plan/replan (bad slug, bad dependency, cycle) | 2 |
| `post` | director, task agent | post whose task check passes, or whose dependencies aren't met | 2 |
| `review` | director, task agent | send-back where the run set still passes — an objection without evidence | 2 |
| `accept` | director | accept while the task isn't `tested` | 2 |
| `retire` | director | retire of a task that is in flight and not escalated | 2 |
| `claims` | worker | a spike's finish claim whose findings or claims don't validate (§14) | 2 |

Separately, `[limits] max_review_rounds` (default 2) caps legitimate
send-backs, and `[limits] max_contest_rounds` (default 2) caps a worker's
contests (§18).

A task is **escalated** when any strike counter reaches its limit, a
send-back would exceed `max_review_rounds`, a contest would exceed
`max_contest_rounds`, or the worker blocks. The
player stops relaying to the worker, and the dashboard shows the cause.
Counters never reset over a task's life. Rejections of director commits to
`main` (not tied to a task) are shown but not counted.

**Resolution.** The operator and director discuss the escalation in their
own conversation — that's outside the system. The outcome is carried out by
the director as ordinary commits:

- **retire** the escalated task (always — an escalated task is never
  resumed in place), and any other tasks the new plan drops;
- **plan** replacement tasks, and **replan** pending tasks whose
  `depends_on` pointed at retired ones;
- optionally record the lesson in the project docs (a `docs_paths` commit
  on `main`), so the dead end informs future work.

A new task may point its prompt at `refs/retired/<slug>` to salvage code or
context from a dead end; the worker copies what it needs into ordinary
commits.

## 11. Merges (player-only)

- **Unblocking a pending task.** When every `depends_on` of a pending task
  is accepted (trivially, when it has none), the player merges `main` into
  `task/<slug>` so the director's post commit builds on the dependencies'
  code — and keeps doing so every time `main` moves, until the task is
  posted, so the post can update any pinned test that landed on `main`
  after the task first became ready. A pending branch only contains
  `tasks/<slug>/`, so this never conflicts.
- **Acceptance.** The director's `accept:` asks the player to land the
  task; the player does every merge itself:
  1. If `main` moved since the branch last saw it (other tasks were
     accepted, §4), the player merges `main` into `task/<slug>` (a *sync*
     commit). On a textual conflict it stops: the task is `conflicted`, and
     the player records, on the branch tip, the exact `main` commit it
     tried and the conflicted paths. This is no strike, and the director is
     notified (a `conflict` event).
  2. It merges the branch into `main` with `--no-ff` and a `Task: <slug>`
     trailer, after running the landing run set (§6.6: the task's modules
     plus those `main` changed since the branch last saw it) on the merge
     result. If it fails because of what the merge brought in (a semantic
     conflict), `main` doesn't move: the task goes back to the worker,
     `in_revision`, re-pinned at the sync, with the failure relayed as the
     evidence and no review round spent. Once it is `tested` again, the
     director reviews and accepts again. A failure with nothing merged in is
     an `accept` rejection, as before.

  `main` only moves this way (and via director-only paths), so "accepted"
  is readable from `main` alone, and the suite passes at every commit on
  `main`.
- **Conflict resolution (director).** The one merge commit an agent may
  make, and only on a `conflicted` task. It is targeted at the recorded
  conflict: the director merges exactly the recorded `main` commit (not a
  newer one) onto the hub's task tip, and resolves the recorded paths and
  nothing else. The player recomputes git's own merge of the two parents
  and refuses (`ownership`) a resolution that merges another commit or
  differs from that merge outside the recorded paths, so the director
  resolves conflicts but writes no code. The player then re-pins and
  carries on landing the accept from step 1: if `main` has moved again it
  merges that in too, which may record a new conflict.

## 12. Derived status

| Status | Derived from |
|---|---|
| `pending` | branch has `task.toml` but no validated post |
| `orphaned` | pending, and some `depends_on` is retired — needs a replan |
| `ready` | pending, and every `depends_on` accepted — the director may post |
| `staged` | `ready`, released by the director's latest `stage:` marker (§9): the task agent is launched and posts. Resolved by walking the branch's first-parent history past the player's `merge_unblock` merges and replans, so `main` moving doesn't un-stage it |
| `posted` | validated post, no live worker session yet |
| `in_progress` | worker session alive and working (runtime, not git) |
| `tested` | latest worker commit validated as passing (or a trim on top of it, §6.5), awaiting director |
| `in_revision` | latest director commit is a send-back or a contest reply (§18), or landing an accept merged `main` in and the suite failed (§11); round N = count of `reviews/r*.md` |
| `contested` | latest worker commit is a `contest:` marker: the task agent (or director) must reply (§18) |
| `conflicted` | landing an accept couldn't merge `main` in; the tip carries the conflict (the `main` commit and paths) for the director to resolve (§11) |
| `escalated` | a strike limit reached, review rounds exceeded, or a `block:` marker; shown with its cause |
| `accepted` | `Task: <slug>` merge commit on `main` |
| `explored` | a concluded spike: branch lives under `refs/explored/<slug>` (§14) |
| `retired` | branch lives under `refs/retired/<slug>`; shown with its post-mortem |

The dashboard renders these from the hub: progress bar (over live tasks;
retired ones counted separately), per-phase breakdown (from
`roadmap.toml`), milestones, `ready` tasks (the new "actionable now"), the
in-flight tasks with their per-reason strike counts and review round (and,
for a conflicted one, the conflict),
escalated (with cause), orphaned, recent rejections, and retired tasks with
their post-mortems. A task's latest check results show as its run set
(`run set (N): …`).

## 13. Parking lot

Decided by the implementation plan (see CLAUDE.md's "Decisions on the
spec's parking lot" for the detail behind each pointer):

- **CLI shape**: `dashboard init` / `status` (default) / `player [--once]`
  / `relaunch <slug>` / `wait` — CLAUDE.md's CLI section.
- **Hub/clone locations on disk, and how the hub is initialised**: XDG
  (`$XDG_DATA_HOME/dashboard/<project>/`), created by `dashboard init` —
  CLAUDE.md's On-disk layout section.
- **The director must not poll**: `dashboard wait` blocks on an
  append-only event log until the player has something director-relevant
  to say — CLAUDE.md's Decisions section.
- **Auto-relaunch**: deferred. A dead worker session is shown and emitted
  as an event, never restarted automatically; `dashboard relaunch <slug>`
  is the manual trigger — CLAUDE.md's Decisions section.
- **Scope and suite subsetting**: a task declares `modules`; the worker
  stays inside them (§8.3) and every judgement runs only the checks that
  cover them (§6.6). The whole suite runs at player start.
- **Parallel tasks**: allowed; the player merges `main` into a task when
  landing its accept, and the director resolves only recorded conflicts
  (§4, §11).
- **How a worker is launched**: `[player] agent_cmd` in `roadmap.toml`
  (default `claude --dangerously-skip-permissions`), run in a `tmux`
  session per task, prompt.md passed as its first message — CLAUDE.md's
  Decisions section.
- **Task agents**: launched and mediated by the player too, with their own
  `[player] task_agent_cmd` (§17).
- **Test purity**: enforced in part by the player, not only by prompts:
  every check runs jailed and must leave its worktree clean (§20).

Still not decided:

- Timeout/crash handling beyond manual relaunch: what the player does if
  the worker session hangs, crashes, or never commits, beyond leaving
  that to the operator via `dashboard relaunch`.
- Whether the `dashboard` skill is rewritten as the director's playbook or
  replaced entirely (CLAUDE.md's Installing section: it must happen before
  `cargo install --path .` propagates this version to consumers).

## 14. Spikes

A task of `kind = "spike"` (in `task.toml`) answers a question instead of
shipping code. It exists for exploration, where nobody can yet write the
failing check §3.1 asks of a post. The rule that every claim is checkable
stays; what changes is who writes the evidence and what "done" means. A spike
never merges into `main`, so the invariant that the suite passes at every
`main` commit is untouched.

**Initiation.** Operator only: the director may suggest a spike but not start
one. The player can't tell whether the operator was asked, so this is a
process rule; if a director is seen posting spikes unasked, add an enforced
authorization. Nothing may depend on a spike (a plan isn't built over a
yet-unknown answer): a spike has no `depends_on`, no task `[check]`, isn't a
milestone, and a `depends_on` that names a spike is a `plan` rejection. It
never counts toward progress. It can run beside other tasks (§4); having
nothing to merge, it never conflicts.

**Roles.**

- *Operator*: decides a spike is worth running, approves every question and
  every send-back, reads the findings with the director, decides when it is
  done.
- *Director*: writes the question with the operator and commits the post;
  reviews the findings against the pinned question; sends back with a new
  question or commits `conclude:`. Plans nothing that depends on the spike.
- *Player*: validates the post, pins the question files and the base commit,
  runs every claim against the right code version, requires `findings`, never
  merges the branch, never requires the project suite.
- *Worker*: explores with no path restrictions (nothing merges), then
  delivers the findings and the claims. It can't touch the pinned files, so
  it can't reword the question.

**Files.** A spike is a sequence of *rounds*; round 1 is the original
question and each send-back opens the next. Nothing is overwritten.

```
tasks/<slug>/task.toml, prompt.md   director, pinned at launch
tasks/<slug>/questions/<n>.toml     director, one per round, append-only
tasks/<slug>/baseline/<n>.toml      director's baseline claims for round n
tasks/<slug>/claims/<n>.toml        worker: answer + claims for round n
tasks/<slug>/findings/<n>.md        worker: free-form findings, round n
```

`questions/<n>.toml` has `question`, `why` (prose), `answers` (the allowed
answers; `inconclusive` is always allowed) and an optional `refines` (the
earlier round it sharpens). `claims/<n>.toml` has a top-level `answer` and
`[[claim]]` entries.

**Claims** are commands the player runs itself from the repo root (`sh -c`,
exit 0 passes, optional `runs`/`min_pass` as in §6.1, and a `timeout`
defaulting to 600 s after which the run is killed and fails). A command may
print a last line `METRICS {json}`; the player stores that object in the
result. Two kinds:

- *baseline*: describes the code as it is now. The director supplies them in
  `baseline/<n>.toml` (pinned, verified to pass at post/send-back time); the
  worker may add its own. Run against the **base commit** with
  `tasks/<slug>/` overlaid, they must pass.
- *spike*: describes what the exploration achieved. Only the worker writes
  them, with `supports = "<answer>"`. They must pass on the branch tip and
  **fail** on the base-with-overlay. A claim that passes without the spike
  (`cmd = "true"`) is rejected.

The **base commit** is the tip at launch (the code of `main` plus the
director's post), recorded once in `refs/dashboard/base/<slug>`; the overlay
copies `tasks/<slug>/` from the candidate onto it, so claim scripts under
`tasks/<slug>/` exist there but the spike's code doesn't.

**Transitions** (additions to §9):

| Transition | Commit | Player checks | Reason |
|---|---|---|---|
| **post** | director adds `prompt.md`, `questions/1.toml`, optional `baseline/1.toml` (+scripts); only under `tasks/<slug>/`, nothing in `claims/` or `findings/` | spike is `ready`, question valid, baseline claims pass | `post` |
| **launch** | — | baseline claims still pass; pins only `tasks/<slug>/` (the project's tests are the worker's to change); sets the base | — |
| **finish** | worker: code, `findings/<n>.md`, `claims/<n>.toml` | pins; only `claims/`/`findings/` of this round under `tasks/<slug>/`; findings non-empty; `answer` allowed; every claim runs and passes as above; a non-`inconclusive` answer has a passing spike claim with `supports` equal to it. Any failing claim rejects the whole finish | `pins`, `claims` |
| **send back** | director adds `questions/<n+1>.toml` (+ optional baseline); operator approves | adds nothing else; `refines` in range; baseline claims pass on the base; within `max_review_rounds`. No failing test needed. Re-pins | `ownership`, `review` |
| **conclude** | `conclude:` marker, spike is `tested` | moves the branch to `refs/explored/<slug>` (like retire), ends the session | `accept`, `ownership` |

`accept:` on a spike is rejected (`accept`); `retire:` is also allowed on a
`tested` spike. A `block:` escalates it as usual. The player reads
`findings/<n>.md` only to require that it exists and isn't empty; the director
and operator judge whether the claims really support the answer and whether
the question was answered rather than sidestepped.

**Where the answer lives.** In notes, not a commit message: the player never
writes commits on a spike branch. Every validated finish gets a
`refs/notes/dashboard-explorations` note `{slug, round, question, answer,
claims: {name: {kind, passed, metrics}}}`; the `conclude:` commit gets the
final `{concluded: true, round, answer}`. `status` reads these, so the answer
shows next to the spike (`explored: yes (round 2)`) without parsing markdown.

**Not in v1.** A sandbox for the worker's paths; anything depending on a
spike or enforcing `derived_from`; operator authorization the player can check;
verifying quotes in the findings; size caps and time budgets.

## 15. Flaky-test fixes

A task of `kind = "flaky-test-fix"` replaces a flaky test with a stable one,
the one case with no honest failing check at post: the test is green most of
the time, and the pinned tree stops the worker editing it. Wrong or
missing assertions are not this path; their fix is a new test in an ordinary
task.

`task.toml` adds `fix_paths` (plain files, no wildcards), `[[replace]]`
(`old`, `new` test names) and `[[evidence]]` (a command run `runs` times on
`main`, `min_pass <= passes <= max_pass < runs`). `[check]` is required and
proves the replacement stable. `architecture/Tests.lean` needs `list_cmd`, printing one
test per line.

- *Pins*: only `fix_paths` are unpinned; `tasks/<slug>/` stays pinned.
- *Post*: the §6.3 rules, except the `[check]` must not be in the suite (no
  `architecture/Tests/<slug>.lean`; a stress check would slow every later suite). The evidence must reproduce
  the flake, else the post is an `evidence` rejection.
- *No repro*: `evidence` has a strike limit of 1. The player retires the task
  itself (§9 allows no other retirer): it moves the branch to
  `refs/retired/<slug>` under a marker commit whose body is `evidence did not
  reproduce (k/N)`. Results are cached by commit, so a retry is a new slug
  with a larger `runs`. The evidence is re-run at launch.
- *Scope*: a finish may change only `fix_paths` (and add new test files).
  A flake whose cause is in the product code isn't this kind of task: it
  takes the regular path (an ordinary task with a deterministic failing
  test, or a spike first). Touching anything else is a `pins` strike, and
  the worker is told to `block:` instead so the director replans it.
- *Finish*: the suite passes, the `[check]` passes on the branch, no test
  from the pinned base other than an `old` one is missing from the `list_cmd`
  inventory, and every `new` exists. Any miss is a `suite` strike.
- *Not enforced*: weakened assertions or a helper that neuters other tests;
  the task agent's review reads the diff for that.

### 15.1 Test speed-ups

A suite that is already over its `[budget]` (a check killed at `check_timeout`,
or the total past `suite_fail`) can't be fixed by an ordinary task: the test
tree is pinned. A task of `kind = "test-speedup"` is the fix. Its plan names
the slow checks, `[[slow]] check = "<suite check name>"`, and has no `[check]`
(its evidence is main's recorded timings) and adds nothing to the suite;
`architecture/Tests.lean` needs `list_cmd`.

- **Post.** The evidence is read from `main`'s cached suite run, never
  re-measured: `main` must be over its budget, every named check must exist
  and be slow there (killed, or a recorded wall time of at least 10% of
  `suite_fail`), and every check killed on `main` must be named, since the
  suite can't pass until each is faster. Otherwise it is an `evidence`
  rejection (limit 1) and the player retires the task, as for a flake that
  doesn't reproduce; the launch re-checks it. The post itself needs no passing
  or failing suite, so it imports while `main` is over budget.
- **Scope** (the inverse of the usual pins). The suite files under
  `architecture/`, the named checks' `files` and the `test_paths` tree are
  unpinned. The worker may change those paths, and in the
  suite definition only a check's `cmd`, `retry_failed` and `retry_on`.
  Changing `runs`, `min_pass`, `covers`, `files`, `test_paths`, `list_cmd`,
  adding or removing a check, any non-test path or `roadmap.toml` (so
  `[budget]`) is a `pins` strike.
- **Finish.** The suite must pass: a check still killed is a `budget` strike,
  and this kind is never held (it is the fix, not the held work). If the
  branch's suite is still over `suite_fail` while `main` is over budget, a
  ratchet against `main`'s recorded `millis` applies: every named check
  strictly faster; no other check slower than on `main` by more than 25% plus
  1s (constants in `checks.rs`, not config: wall time jitters); the total no
  higher than `main`'s; a check `main` lacks within `suite_fail` on its own.
  Once the branch is under `suite_fail` the normal rule applies.
- **Removals are visible, not forbidden.** The player diffs the test
  inventory (`list_cmd`) against the pinned base and lists the changed `cmd`s
  (a `cmd` turned into `true` removes tests the inventory can't see). That
  text is the `speedup_finish` event and is noted on the finish commit
  (`refs/notes/dashboard-speedups`), where the task agent's review prompt
  reads it; the director decides at `accept`.
- **The task agent** posts `prompt.md` alone, as for a flaky-test-fix.

## 16. Check retries and the flake ledger

A `[[check]]` may set `retry_failed = N` (default 0) and `retry_on`, a list
of regexes. When the check fails, the player re-runs it, all its `runs`, up
to N more times; it passes if any attempt does, and failing every attempt is
still one `suite` strike. With `retry_on`, only a failure whose output
matches is retried, so any other failure is a strike at once. Both fields are
director-owned and pinned with the suite.

Retries never apply to a task check (§6.3) or to evidence (§15),
and the worker's verdict stays plain: it is never told of them.

Every attempt is in the cached result. A check that passed only on a retry
goes in the *flake ledger* (`refs/flakes/<scope>/<n>`, notes under
`refs/notes/dashboard-flakes`): the check, the retries used and the failing
outputs. `status` shows `passed on retry k/N`, `flaky: N` per task and a
list of recent entries, so the director can plan a hygiene task, e.g. a
`flaky-test-fix`, from evidence instead of anecdote.

## 17. Task agents

The director holds the cross-task view; everything about one task (its
prompt and failing test, reading its diff, its send-backs) belongs to that
task's **task agent**, an LLM session acting as the director's delegate for
that task alone. Task agents are launched and mediated by the player, like
workers, so the operator can watch one at work (`tmux attach`), every
commit it makes is validated and attributed to it, and its verdicts are
recorded in the hub instead of living only in the director's context.

**Configuration.** `[player] task_agent_cmd` in `roadmap.toml` (default
`claude --dangerously-skip-permissions`, like `agent_cmd`; a separate key so
the two roles can run different commands or models). An empty value turns
task agents off: the director posts and sends back itself (§9 as written).
A spike (§14) never has a task agent: its question is the director's, written
with the operator.

**Lifecycle.**

- *Launch*: a `ready` task never starts on its own. The director releases
  it with an empty `stage: <why>` commit on its branch; when an
  agent-managed task is `staged` the player
  creates the task agent's clone and starts its session with the post step;
  a task found `tested` with no task agent gets one with the review step.
  Launched once, recorded in `refs/dashboard/agent/<slug>`; like a worker,
  a dead task agent is reported (`agent_died`), never restarted
  automatically: `dashboard relaunch <slug> --agent` restarts it in the same
  clone, with a fresh prompt for where the task is now. Only a `ready`
  non-spike task can be staged and only a `staged` one unstaged (back to
  `ready`, and only while no task agent exists); anything else is a `plan`
  rejection, and a task agent's marker is `ownership`.
- *Review*: each time the task becomes `tested` at a new tip, the player
  pastes a review request for the next round into the task agent's session
  (once per tip, recorded in `refs/dashboard/agent-asked/<slug>`).
- *Questions*: `dashboard ask <slug> "<question>"` pastes the director's
  question into the session and notes it as waiting for an answer; the
  answer comes back as a report.
- *End*: the session is killed and the clone removed when the task is
  accepted, retired or auto-retired. An escalated task keeps its task agent,
  so the director can ask it what went wrong.

**Clone.** Another worktree of the workers' shared workspace (CLAUDE.md),
at `<director>/.dashboard/agents/<slug>/`, with a full working tree (§21),
on the local branch
`agent/<slug>` (the worker's worktree has `task/<slug>`). The player
fetches `agent/<slug>` into `refs/incoming/agent/<slug>`; provenance by fetch
source (§4) makes its commits the task agent's (`actor: task_agent`).

**Prompt.** The player writes it from `prompts.kdl` (§19): the task agent's role and rules, the task
as planned (`task.toml`, the phase goal, and the director's `brief`, a free
text field of `task.toml` that is the director's channel to it), how to
report, and the current step. The rules: never write a throwaway
implementation (implementing is the worker's; it may run any test it
writes against the code as it is, to see it fail for the right reason, but
never against code it wrote); test observable behaviour, not one
implementation (§18); every test it writes is pure, and its `prompt.md`
passes that on; it writes no product code; it never commits a marker. It
is also told its tests are worked out with the worker, not handed over
once: contests (§18) and send-backs bounce them back and forth through the
hub, and it treats each exchange as collaboration, conceding when the
worker is right. The
review step also asks it to run a send-back's new test on the worker's
code first, so a send-back the suite still passes doesn't cost a `review`
strike.

**What the player accepts from a task agent.** Only the newest commit of a
fetch is validated, as one cumulative change from the hub tip:

| Task status | Commit | Validated as | Rejection reason |
|---|---|---|---|
| `ready` | adds `prompt.md`, the test files, `architecture/Tests/<slug>.lean` (the task check) | the post row of §9 | `post`, `evidence` |
| `tested` | adds `reviews/r<N>.md` + new/updated tests | the send-back row of §9 | `review` |
| `tested` | leaves `tasks/<slug>/` alone: removes tests the task added | the trim row of §9, only when the director asks | `accept` |
| `contested` | adds `contests/c<N>.md` + optionally fixed tests | the reply row of §18 | `ownership` |
| any | touches a path outside its scope: `architecture/Tests/<slug>.lean`, the `files` of checks in the task's run set, the APIs whose both ends are in `modules`, the hub's `test_paths`, `[player] lock_files` and `tasks/<slug>/`; changes `task.toml` (the director's plan); any other status | — | `ownership` |
| any | an empty (marker) commit | — | `ownership` |
| any | merge commit, or rewrites a commit imported from it | — | `history` |

Scope is read at the hub tip, so a commit can't widen its own, with one
exception: the commit may add new files under the `files` it declares in
`architecture/Tests/<slug>.lean`, so a test can live inside a module's paths
(a Rust crate's `tests/`) and stay in the module's view. Existing files there
stay the worker's.
Rejections count against the task's strike counters like anyone's (§10) and
are relayed into the task agent's session with a recovery hint. A hub tip
that moved under an unimported commit (an unblock merge, a director replan)
is not the agent's fault: the player rebases its commit onto the new tip
(tree sync, §5), or asks it to, once per commit, with no strike.

While task agents are on, a director post or send-back on an agent-managed
task is an `ownership` rejection: those are the task agent's. Plan, replan,
trim, accept, retire and conflict resolution stay the director's.

**Reports.** After each step and each question, the task agent runs, from
its worktree:

    dashboard report [verdict] --why "…" [--check "…"] [--suite "…"] [--concerns "…"]

The player notes in the clone's git directory what waits for a report
(`dashboard-step.json`, and `dashboard-question.json` for the director's
question) and which verdicts close it:

| Waiting | Noted when | Verdicts |
|---|---|---|
| `post` | the post step is pasted (launch, relaunch) | `unsure` |
| `post` + commit | its post is imported | `posted` |
| `review r<N>` | the review request is pasted | `accept` (with `--suite`), `unsure` |
| `review r<N>` + commit | its send-back is imported | `sent-back` |
| `contest c<N>` | the contest is pasted | `unsure` |
| `contest c<N>` + commit | its reply is imported | `replied` |
| `trim` + commit | its trim is imported (it answers the question) | `trimmed` |
| `question` | `dashboard ask` | `answer`, `unsure` |

A step closed by a commit takes its verdict from the import, so the agent
leaves it out; a named verdict closes the question first, then the step.
Any other verdict, an empty `--why`, `accept` without `--suite` (each test
the task added, keep or drop, §6.5) or `--suite` on another verdict exits 1
naming what waits. The command writes `dashboard-report.json` atomically;
the player takes it, closes the step it names, and renders the block the
director reads (`slug`, `step`, `commit`, `check`, `verdict`, `why`,
`suite`, `concerns`) with slug, step and commit from its own note. A report
no waiting step allows (written by hand, or overtaken by a new step) is
dropped and the agent told. The player notes the block on the task tip
(`refs/notes/dashboard-agent-reports`, a list) and emits an `agent_report`
event carrying it. `status` shows each task agent's session, liveness and
latest verdict. The director trims what it drops (§6.5) and accepts on an
`accept` verdict, asks (`dashboard ask`) or takes an `unsure` to the
operator; the decision stays the director's.

**Events.** `agent_launched`, `agent_report`, `agent_died`. For an
agent-managed task, `tested` and `contested` are not emitted: they're the task
agent's to act on, and the director hears its report instead. `ready` is
emitted: it waits for the director's `stage:`.

## 18. Contests

A task agent writes its tests without an implementation to check them
against (§17), so a test can be wrong: impossible to satisfy, or written
around one implementation instead of the behaviour the task asks for. A
`block:` would escalate, and the task would be retired and replanned for what
is often a small fix to the test. A **contest** sends it back the other way
instead.

| Transition | Commit (author) | Player checks | Rejection reason |
|---|---|---|---|
| **contest** | `contest:` marker naming the test, body = why it's wrong (worker) | a clean marker on the hub tip (as `block:`); not a spike; within `max_contest_rounds`, else the task escalates and the marker isn't imported | `history`, `suite` |
| **reply** | adds `tasks/<slug>/contests/c<N>.md` and optionally fixed tests (task agent; the director when task agents are off) | task is `contested`; only the reply and the task agent's scope (§17); re-pins; relays the reply to the worker | `ownership` |

- While `contested` (§12) the worker waits; the player fetches nothing from
  it. The task agent is asked to reply (once per contest), still without
  writing any implementation. Without task agents, the director gets a
  `contested` event.
- The reply has no suite requirement: it may keep the test and explain how
  it can be met, or fix it. It spends no review round; the task returns to
  `in_revision`.
- `[limits] max_contest_rounds` (default 2) caps contests per task, counted
  as the `contests/c*.md` replies at the tip plus the new one. One more
  escalates, with the worker's reason in the cause.
- A contested task may be retired (§9).
- `block:` stays the escape for everything else: a missing prerequisite, a
  wrong approach, a stale test (§6.4), a decision only a human can make.

## 19. Prompts

Every text the player gives an agent comes from `prompts.kdl` on `main`:
the worker's and spike worker's preambles, the flaky-test-fix
paragraph, the task agent's preamble and steps, and every message it pastes
into a session (relayed reviews and contest replies, rejections and their
recovery hints, finish acknowledgements, rebase requests, the director's
questions). The rejection reasons themselves stay the player's own
diagnostics; only the text around them is a template.

- Each template is a named string under its section (`worker`, `spike`,
  `task-agent`) with `{name}` placeholders, each template allowing only its
  own set. What fills them is the hub's: the slug, the pinned files,
  prompt.md, a review, a check summary.
- `max-bytes` caps the static text of every prompt the player delivers:
  the templates that make it up (a task agent's launch is its preamble,
  the relaunch note and its largest step), with placeholders removed. What
  the agents and the director write (prompt.md, reviews, contest replies,
  the brief) and check output aren't counted: the cap is on the player's
  padding, not on what the agents say to each other.
- The parts are capped too, optionally: `max-bytes=N` on a section limits
  each of its templates, and on a template replaces its section's limit
  for that one template. Same counting.
- The file is required and director-owned. `dashboard init` commits a
  starter when `main` has none, and validates one that's there. A `main`
  commit whose copy doesn't parse, misses or adds a template, uses another
  template's placeholder or exceeds `max-bytes` is an `ownership`
  rejection, so `main` always carries a valid one. The player reads it from
  `main`'s tip whenever it writes a prompt or sends a message, so an edit
  applies to the next one; prompt files already written stay as they were.

## 20. Check isolation

Prompts ask every test to be pure; the player enforces the part it can
guarantee.

- **The jail.** With `[sandbox] enabled` (the default, in `roadmap.toml`:
  director-owned, so no agent can loosen it), every check, claim, evidence
  and `list_cmd` runs under bubblewrap: the host filesystem read-only, the
  check's worktree and the director's `writable` paths read-write, a private
  `/tmp` (`TMPDIR=/tmp`), and loopback only unless `network = true`. A test
  that writes outside its sandbox or reaches the network just fails, like any
  failing check.
- **A clean worktree.** After each run the player looks at `git status` in
  the worktree, and for a check that runs in a view (§6.2) in the view too
  (a throwaway git repo carrying the worktree's `.gitignore` files); a
  changed tracked file or a new untracked one (ignored build output aside)
  means the check is impure. In a view, the architecture's
  `view_rewritable` files (under `shared`, e.g. a workspace lock that
  cargo trims to the crates present) may change: each view is rebuilt per
  check, so that rewrite stays inside it. It fails whatever its exit code,
  its remaining runs are skipped, and it isn't retried. An impure check can
  never stand in for a required failure: a post whose task check is impure is
  a `post` strike, a send-back whose suite fails only impurely a `review`
  strike, a launch is held, a spike claim that fails on the base impurely is
  a `claims` failure, and impure evidence retires the task as a miss.
- **Preflight.** `dashboard init` and the player (at start, and again
  whenever `[sandbox]` changes) first prove the jail holds on this host:
  bwrap runs, the working directory is writable, a directory outside it
  isn't, `/tmp` is private and, with the network off, only `lo` exists. On a
  failure `init` creates nothing and the player runs no tick: checks never
  run unjailed by accident. `enabled = false` turns the jail (not the
  clean-worktree check) off, e.g. on a host without bubblewrap.
- **Hitting the walls.** A failing run whose output shows the jail refused
  it (`Read-only file system`; with the network off, an unreachable network
  or a failed name lookup) is impure too, the same as a leak. It's a
  heuristic on libc's and curl's messages, but such a failure never means
  "fails for the task's reason", so it can't satisfy a post either.
- **The impurity ledger.** Every impure check the player sees is recorded
  under its task (or `_main`), like the flake ledger: `status` shows
  `impure: N` per task and the latest entries, with what each left behind
  or which wall it hit.
- **The cache.** A cached result records the policy it ran under, and the
  version of the impurity detector, and is only reused under the same.

**An impure `main`.** Impurity that's already on `main` (older than
this check, or surfacing under a new policy) is nobody's current work, so
it mustn't strike anyone. The player runs `main`'s suite under the current
sandbox at start and every tick (a cache hit until main, the policy or the
leak check changes). Impure checks there are reported to the director once
(`main_impure`) and shown in `status`; the player keeps running, since only
a task can fix a test on `main`. Until then, a suite that fails *only* on
checks impure both on `main` and on the commit at hand is main's problem:
a director commit on `main` imports anyway (it may be the `[sandbox]` fix),
and a worker's finish or a director's accept is *held* — neither imported
nor rejected, no strike, said once. When those checks are clean on `main`,
a held accept is simply re-landed; a held finish's branch still carries the
old check, so the player merges `main` into it (a `sync_revision`,
re-pinned) and rebases the worker's finish onto that (tree sync, §5), or
asks the worker to. A launch held for
an impure task check is reported once per tip, not every tick.

**An over-budget `main`.** The same rule covers a `main` that is already over
its `[budget]`, which is nobody's current work either. `verify_main` also
records how `main`'s suite is over (the checks killed at `check_timeout`, the
total past `suite_fail`, every check's `millis`); it is reported once per main
tip (`main_over_budget`) and shown in `status`. A suite that fails *only* on
checks killed on both `main` and the commit at hand is held like an impure one
(a director commit on `main` imports, so the limit can be raised; a finish or
accept is held, no strike; a send-back on such a failure is no evidence). A
passing finish over `suite_fail` is held only if every check `main` has is
within the ratchet's tolerance of its time there and the checks `main` lacks
fit `suite_fail` on their own; totals are never compared, so slow tests can't
hide in main's excess, and a check that got slower is a `budget` strike. A
held finish is released, as for impurity, once the new policy has `main` under
budget (one tick after the director's commit, as `[budget]` is read from
`main`'s tip). A `test-speedup` (§15.1) is judged by its ratchet, not held.
Checks are matched by name.

**A failing `main`.** The same rule covers a check that fails on `main`
(a check whose run set never ran it, a change in the toolchain or host).
`verify_main` records the failing checks; a failing `main` is reported once
per main tip (`main_failing`) and shown in `status` ("Failing on main"). A
suite that fails *only* on checks that also fail on `main` is held like an
impure one: a director commit on `main` imports (it may be the fix), a
finish or accept is held with no strike, and a send-back on such a failure
is no evidence. A commit that adds a failure of its own is judged as usual.
Held work is released once those checks pass on `main`.

Not enforced: determinism and order independence (a clock, unseeded
randomness, state shared through a `writable` path). The task agent's review
still reads for those.

## 21. The task workspace

A worker's working tree holds its task's files and the files its checks
read; everything else is absent. Git's sparse checkout keeps it so in the
worker's worktree. The clone's objects hold the whole history
(`git show origin/task/<slug>:<path>` reads any file), so the working tree
is the boundary, and the boundary is what the worker's tools see.

**Contents**, computed at the hub tip the pins are taken at (§8.3):

- *Writable*, the scope of §8.3: the paths of the task's `modules` and
  roots (inline tests included), `shared`,
  `architecture/Tests/<slug>.worker.lean` and the `files` its checks
  declare.
- *Read-only*, pinned: `architecture/` whole, so the worker's tools read
  the same Lean package the player reads; `tasks/<slug>/`; the files of
  the APIs of each of `modules` (generated or consumed, every API they
  ref, with their `impl` files); the `files` of the checks in the task's
  run set; `test_paths`; every `.gitignore`; the agent config the worker's
  agent reads, `.claude/` and `CLAUDE.md`.
- *Absent*: other modules' and roots' sources, and so their inline tests,
  and the `files` of every check outside the run set. An inline test
  outside the scope stays out of every run because its source is out of
  the tree.

**Patterns follow the pins.** The player recomputes the patterns whenever
it pins (launch, send-back, contest reply, trim, a merge of `main`, a
resolution), from the tip the pins are taken at. A modified or untracked
file whose path leaves the patterns stays on disk (git warns and leaves
it). The player writes a worker's sparse patterns and nothing else in its
clone (§5).

**Declaring a check opens its files.** `git add -A` in a sparse tree
stages only paths inside the patterns. `dashboard build` and
`dashboard test` read `architecture/Tests/<slug>.worker.lean` from the
working tree and add the `files` of its checks to the patterns before they
run, so a new test file is in the worker's next commit. The worker prompt
says: declare the check, then write its files; within one `dashboard test`
either order works.

**Two verbs** assemble the views, so the player stays toolchain-neutral:

- `dashboard build [node…]` snapshots the working tree (as `dashboard test`
  does) and, for each of the task's `modules`, or the named ones, which
  must be among them, runs its `build_cmd` in the module's view
  (`architect-v3.md`) built from the snapshot, generators first, stopping
  at the first failure. A root builds in a full worktree of the snapshot:
  roots have no view and link the modules they call. A build compiles only
  and runs no tests; its output streams to the caller. It runs locally, in
  the CLI's own worktree, even with `[player] runs = "queued"`, and
  records no verdict. A node without `build_cmd` is reported as having
  none. `build_cmd` gets `DASHBOARD_MODULE` and `DASHBOARD_ROOT` like
  `test_cmd`.
- `dashboard test [check…]` runs the run set (§6.6), or only the named
  checks, which must be in the run set; any other name is an error listing
  the allowed names. The local and the queued path both take the names.

`dashboard scope` prints the working tree's contents as a third section
beside the writable paths and the run set.

**Full working tree** for every other session: task agents (§17), spikes
(§14), flaky-test-fixes (§15) and test-speedups (§15.1).
