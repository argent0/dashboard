# Director / Player / Subagent spec

Working spec for evolving `dashboard` from a stateless `tasks.yaml` renderer
into the enforcement point for a subagent-per-task roadmap loop, with all
roadmap state derived from git. Greenfield: no backward compatibility with
`tasks.yaml`, existing consumer roadmaps, or the current `dashboard` skill.

## 1. Baseline (today)

`dashboard` is a stateless CLI that renders `tasks.yaml` (progress bar,
actionable-now list, blocked/in-revision sections). All orchestration around
it — spawning subagents, judging their work, editing task state — happens by
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
- **Subagent** — an LLM (Claude Code session), one per task, launched by the
  player. Writes code (and may add new tests) until the project suite
  passes, then commits — or blocks.
- **Player** — a process; `dashboard`'s new responsibility. The only writer
  of the canonical repo. Imports and validates every commit, runs every
  test itself, launches subagents, counts strikes, merges accepted work,
  retires dead tasks. Never trusts a narrated claim — only a test it ran
  itself.

## 3. Core philosophy

### 3.1 Evidence over narration

Two failure modes motivate this:

1. A subagent, eager to finish, claims success without it being true.
2. A director rejects work without real grounds.

Both are closed by requiring every claim to be a committed, currently
checkable test:

- The director cannot **post** a task until it has committed a test that
  currently *fails*.
- The subagent cannot **finish** a task until the whole project suite
  *passes*.
- **Sending back** requires a new failing-test commit, not a prose objection.
- **Accepting** is also a commit, not silence.

Nobody writes a status. Status is a pure function of the repo (commits,
branches, pins, check results, rejections) plus whether the subagent's
session is alive. Director and subagent never talk directly; git is the only
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
subagent clone ──┼─(player fetches, validates)─► hub (bare) ─► dashboard renders from hub
                 │                                 ├─ refs/notes/dashboard: cached check results
                 │                                 ├─ refs/rejected/…: rejected commits
                 │                                 └─ refs/retired/…: retired task branches
tmux session  ◄──┘ (player launches the subagent, relays rejections/revisions)
```

- **Hub** — a bare repository owned by the player. The only source of truth.
  Only the player updates its refs.
- **Clones** — the director has one clone; the subagent gets its own clone
  of the hub checked out on its task branch. Separate clones, not
  worktrees: worktrees share refs. Agents fetch from the hub freely but
  never push.
- **Provenance by fetch source.** The player pulls new commits from each
  clone. Commits fetched from the director's clone are the director's;
  commits fetched from the subagent's clone are the subagent's. No tokens or
  author fields are needed to establish identity, and there is nothing to
  leak or forge in commit content.
- **One task at a time.** At most one task is posted, in flight, or
  escalated-but-not-yet-retired. `main` never moves while a subagent is
  working (except director-only paths, §5).

## 5. History rules

Enforced by the player on import; a violating commit is never imported.

- **Fast-forward only.** Every imported ref update must be a descendant of
  the hub's current ref. No rebase, amend, reset of published commits, or
  force push.
- **No merge commits from agents.** The only merge commits are the
  player's own (§11).
- **Path ownership.** A subagent commit must leave every pin (§8.3)
  unchanged. The director commits to a task branch only through the
  transitions in §9.
- **`main` is player-only**, except for director commits touching only
  `roadmap.toml` or the doc paths declared in it (`docs_paths`). The player
  runs the project suite on these like any other `main` commit.

**Marker commits.** A few transitions are events with no content: *block*,
*accept*, *retire*. They are empty commits (`--allow-empty`) whose subject
starts with the transition name (`block: …`, `accept: …`, `retire: …`); the
body carries the explanation. Provenance (§4) says who made it, the empty
diff says it isn't work, and the prefix says which event it is. This is the
only commit-message convention; all other messages are free-form, short
and topical.

**Rejection.** The player leaves the hub branch untouched, stores the
offending commit under `refs/rejected/<slug>/<n>` as evidence, records the
reason so `dashboard` shows it, and relays it to the subagent's tmux session
when the subagent is the author. The agent recovers in its own clone: a
fixing commit on top for content violations (e.g. restoring a pinned file),
or `git reset --hard` to the hub ref for history violations. The player
never modifies an agent's clone. Each rejection is a strike in its reason's
counter (§10).

## 6. Tests

### 6.1 Checks and thresholds

The unit of evidence is a **check**: a shell command plus a threshold.

```toml
[[check]]
name = "unit"
cmd = "cargo test"            # deterministic: runs = 1, min_pass = 1 (defaults)

[[check]]
name = "summary-grounding"
cmd = "pytest tests/llm/test_summary.py"
runs = 10
min_pass = 8
```

The player runs `cmd` `runs` times; the check **passes** when at least
`min_pass` runs exit 0, and **fails** otherwise. A deterministic test is the
degenerate case `runs = 1, min_pass = 1`. This makes non-deterministic tests
(e.g. LLM-backed) yield a deterministic verdict the whole model can rely on.

Thresholds are task-dependent. The director sets them when it writes the
check; the operator may set or override them (the director commits the
operator's choice). They live in pinned files, so the subagent can never
lower a threshold to get a pass.

A threshold check can still fail by chance on a correct implementation.
That shows up as a `suite` strike (§10), whose limit can be set higher than
the others to absorb it.

### 6.2 The project suite

`tests.toml` at the repo root declares the project suite:

- `[[check]]` entries (as above) — every check the project must pass.
- `test_paths` — the files and directories that make up the test tree,
  including anything that could defeat a test without touching it
  (fixtures, `conftest.py`, test config). The tool can't tell which files
  are tests in an arbitrary language, so the project declares it.

**The suite passes** when every check passes. Once a task is merged its
checks and test files are project tests, not "that task's tests". The
invariant is that the project suite passes at every commit on `main`.

### 6.3 The task check

Each task's `task.toml` declares the task's own check (same shape as
`[[check]]`): the thing that must fail at post and is the task's reason to
exist. In the post commit the director also adds it to `tests.toml`, so on
completion it is part of the suite.

### 6.4 Stale tests

A task may legitimately change behaviour an existing test asserts. Deciding
that a test is stale — rather than inconvenient — is a design decision and
belongs to the director, never the subagent:

- **Foreseen:** the director updates or deletes the stale test (and its
  `tests.toml` entry) in the task's post commit, visible in the diff.
- **Found mid-task:** the subagent cannot touch pinned tests, so it
  **blocks**, explaining the conflict. That escalates the task (§10); the
  director and operator replan, typically retiring it and posting a
  replacement whose post commit fixes the stale test.

## 7. Subagent commits

The subagent commits only when it believes it is done. Every subagent
commit is therefore one of:

- a **finish claim** — any non-empty commit. The player checks it (§9);
  it either becomes `tested` or is rejected.
- a **block** — a `block:` marker commit whose body explains why the task
  can't be finished as specified (stale test, missing prerequisite, wrong
  approach, needs a human decision).

Work in progress stays uncommitted in the subagent's clone.

## 8. Task layout

### 8.1 Branches and names

One branch per task, `task/<slug>`, where `<slug>` is a human-chosen,
stable name fixed at branch creation. Future tasks are branches too: the
director creates a task's branch as soon as it plans the task, so the whole
roadmap — not just in-flight work — is visible in the hub. A retired task's
branch moves to `refs/retired/<slug>`; its slug is never reused.

### 8.2 Files

```
roadmap.toml                   on main: title, phases (id, title, goal),
                               docs_paths, [limits] (§10)
tests.toml                     project suite: [[check]] entries, test_paths
tasks/<slug>/task.toml         title, phase, milestone, depends_on, check
tasks/<slug>/prompt.md         director → subagent instructions
tasks/<slug>/reviews/r<N>.md   director's send-back notes, round N
```

When a task is accepted, `tasks/<slug>/` lands on `main` and stays there as
a permanent, immutable record of what the task asked for and how it was
reviewed. Blocks, acceptances and retirements are marker commits (§5), not
files.

### 8.3 Pins

Pins are computed by the player as late as possible: immediately before
the subagent starts work — at launch, and again before relaying a
send-back. Until then the director may keep refining tests freely.

The player records git's blob hash (`git rev-parse <tip>:<file>`) of every
file that exists at that moment under:

- `tests.toml` and every entry of its `test_paths` (the whole test tree),
- `tasks/<slug>/`.

Every subagent commit must leave each pinned file present and byte-identical
— mechanically checked, no trust involved. New files are allowed: the
subagent may add tests (which become project tests on merge).

Pins close tampering with the tests themselves, not every way of beating
them (e.g. special-casing the code under test, or a new file that
monkeypatches). Catching that is the director's review job.

## 9. Transitions

Each transition is a commit the player validates before importing. A
failed validation is a rejection with the listed reason (§10).

| Transition | Commit (author) | Player checks | Rejection reason |
|---|---|---|---|
| **plan** | new branch `task/<slug>` from `main`, adding `tasks/<slug>/task.toml` (director) | slug unused (incl. retired); `depends_on` slugs exist and aren't retired; no cycles | `plan` |
| **replan** | edits `tasks/<slug>/task.toml` on a not-yet-posted branch (director) | same as plan | `plan` |
| **post** | adds `prompt.md`, the task check and its test files, `tests.toml` entry, stale-test updates (director) | no other task in flight or escalated; all `depends_on` accepted and merged into the branch (§11); task check **fails** | `post` |
| **launch** | — (player) | task check still **fails** at the tip; records pins; starts `tmux` session running `claude` in a fresh subagent clone with `prompt.md` | — |
| **finish** | code, optionally new tests (subagent) | pins unchanged; project suite **passes** | `pins`, `suite` |
| **block** | `block:` marker (subagent) | — | — (escalates) |
| **send back** | adds `reviews/r<N>.md` + new/updated tests (director) | task is `tested`; project suite now **fails**; re-pins; relays the note | `review` |
| **accept** | `accept:` marker (director) | task is `tested`; player merges (§11) | `accept` |
| **retire** | `retire:` marker, body = post-mortem (director) | task is not posted or in flight, or is escalated; player moves the branch to `refs/retired/<slug>` and ends the subagent session | `retire` |

Any agent commit that breaks the history rules (§5) is rejected with reason
`history`, whatever transition it was meant to be; a director commit that
fits no transition is rejected with reason `ownership`.

The subagent's tmux session persists across send-backs, so the same
subagent, with its context, handles them. It ends when the task is accepted
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
| `ownership` | director | commit that fits no transition, or touches paths it may not | 2 |
| `pins` | subagent | finish claim modified or deleted a pinned file | 2 |
| `suite` | subagent | finish claim where the project suite fails | 2 |
| `plan` | director | invalid plan/replan (bad slug, bad dependency, cycle) | 2 |
| `post` | director | post whose task check passes, or whose dependencies aren't met | 2 |
| `review` | director | send-back where the suite still passes — an objection without evidence | 2 |
| `accept` | director | accept while the task isn't `tested` | 2 |
| `retire` | director | retire of a task that is in flight and not escalated | 2 |

Separately, `[limits] max_review_rounds` (default 2) caps legitimate
send-backs.

A task is **escalated** when any strike counter reaches its limit, a
send-back would exceed `max_review_rounds`, or the subagent blocks. The
player stops relaying to the subagent, and the dashboard shows the cause.
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
context from a dead end; the subagent copies what it needs into ordinary
commits.

## 11. Merges (player-only)

- **Unblocking a pending task.** When every `depends_on` of a pending task
  is accepted, the player merges `main` into `task/<slug>` so the director's
  post commit builds on the dependencies' code. A pending branch only
  contains `tasks/<slug>/`, so this never conflicts.
- **Acceptance.** The player merges `task/<slug>` into `main` with `--no-ff`
  and a `Task: <slug>` trailer, after running the project suite on the
  merge result. `main` only moves this way (and via director-only paths),
  so "accepted" is readable from `main` alone, and the suite passes at every
  commit on `main`.

## 12. Derived status

| Status | Derived from |
|---|---|
| `pending` | branch has `task.toml` but no validated post |
| `orphaned` | pending, and some `depends_on` is retired — needs a replan |
| `ready` | pending, and every `depends_on` accepted — the director may post |
| `posted` | validated post, no live subagent session yet |
| `in_progress` | subagent session alive and working (runtime, not git) |
| `tested` | latest subagent commit validated as passing, awaiting director |
| `in_revision` | latest director commit is a send-back; round N = count of `reviews/r*.md` |
| `escalated` | a strike limit reached, review rounds exceeded, or a `block:` marker; shown with its cause |
| `accepted` | `Task: <slug>` merge commit on `main` |
| `retired` | branch lives under `refs/retired/<slug>`; shown with its post-mortem |

The dashboard renders these from the hub: progress bar (over live tasks;
retired ones counted separately), per-phase breakdown (from
`roadmap.toml`), milestones, `ready` tasks (the new "actionable now"), the
in-flight task with its per-reason strike counts and review round,
escalated (with cause), orphaned, recent rejections, and retired tasks with
their post-mortems.

## 13. Parking lot (explicitly not yet decided)

- Timeout/crash handling: what the player does if the subagent session
  hangs, crashes, or never commits.
- Scope enforcement beyond pins: whether the player restricts which
  non-test paths a subagent commit may touch.
- Suite runtime: the full suite (including multi-run checks) runs at every
  finish and merge; may need caching or subsetting for large projects.
- Parallel tasks, and with them acceptance conflicts (`main` moving under an
  in-flight task, given agents can't merge or rebase).
- CLI shape (e.g. `dashboard` renders, `dashboard player` runs the loop),
  hub/clone locations on disk, and how the hub is initialised.
- Whether the `dashboard` skill is rewritten as the director's playbook or
  replaced entirely.
