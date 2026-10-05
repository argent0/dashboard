# Feature requests from the `personal-assistant` project

Written 2026-10-01 after running roughly 20 tasks through the loop, six of them
real-device milestones on an Android phone driven by a grok worker. Everything
here is a **suggestion**: if one cuts against the player's philosophy (single
writer, never trusts narration, status is a pure function of the hub), drop it
or reshape it. The pains are real; the proposed fixes are only one way to
address them.

Evidence comes from observed behaviour and from reading `spec.md` §4, §5 and §9.
The player source was not read, so "the player does X" below is inferred from
behaviour unless a section is cited.

Roughly in priority order.

---

## 1. A way to say "wait for the operator" before launching a worker

**Pain.** Posting a task immediately launches the worker. For a task that
drives hardware (here: a physical phone with a lock screen, app-lock timeouts
and wireless adb), nothing tells the player the phone must be ready. The worker
starts, the phone is locked or adb has dropped, and the run fails for
environmental reasons. Retiring and replanning that costs far more than a
"not yet" would.

**Suggestion.** An optional `task.toml` field, e.g. `needs_operator = true`,
meaning the post is accepted and the check is verified failing as usual, but the
player does not launch the worker until the operator runs something like
`dashboard go <slug>`. Status could show it as `posted (waiting for operator)`.

**Philosophy fit.** The gate is an explicit operator action recorded in the
hub/state, not an agent's claim, so it should not weaken "never trusts
narration".

---

## 2. A resumable `blocked` outcome for environmental blocks

**Pain.** An escalated task is always retired and never resumed in place. That
is right when the approach or a test was wrong. It is wasteful when the block
was environmental (phone locked, adb dropped, api-proxy down): the code and
test were fine, yet v2 cost a retire, a plan, a post and a replan before the
same work could be tried again.

**Suggestion.** A block kind the worker can mark (e.g. `block(env): ...`) that
parks the task as `paused` and lets the operator `dashboard resume <slug>`
once the environment is fixed, without burning a task slug or the one-in-flight
slot's history. Retire stays the answer for design/test blocks.

**Philosophy fit / caveat.** This softens "an escalated task is never resumed in
place". The rule exists to stop a flailing worker from being resumed over and
over, so it probably needs a cap (e.g. one resume per task, or only when the
operator, not the agent, classifies the block as environmental).

---

## 3. Escalate automatically when a posted task's session is dead

**Pain.** If the worker's tmux session dies (here: the operator killed it on
purpose), the task sits `posted` with a dead session. `retire` is rejected for
posted tasks (§9), so the only exit was `dashboard relaunch` (which restarts an
agent with `--always-approve`, here against a real phone) followed by killing it
within a second and writing the `block:` commit by hand in the worker's clone.

**Suggestion.** Any of: (a) `dashboard abandon <slug>` for the operator, which
escalates the task with a recorded reason; (b) auto-escalate after N minutes of
`dead` session with no relaunch; (c) let `relaunch --no-agent` restore the clone
and state without starting the agent.

**Philosophy fit.** All three keep the player as the only writer and make the
transition an explicit, logged event instead of a hand-made commit in someone
else's clone.

---

## 4. Do not count a strike when the divergence is the player's own doing

**Pain.** The player merges `main` into task branches. The director's local copy
of that branch then diverges from the hub, and the director's next commit is
rejected as `history` and counts a strike. This happened twice (both on
milestone tasks that were planned early and sat while `main` moved).

**Suggestion.** Options: (a) don't merge `main` into pending (unposted) task
branches until they are posted; (b) when the director's commit fails only because
its parent is an older hub tip of the same branch, offer a fast-forward-and-replay
path instead of a strike; (c) at least make `git fetch hub` plus the exact reset
command part of the rejection message (it already hints at `git reset --hard`).

**Caveat.** Option (a) may conflict with how readiness is computed; (c) is the
cheapest.

---

## 5. A dry-run for director commits

**Pain.** The rules are learned by being rejected:
- the commit that adds a file counts as a finish claim, so `block:` must be an
  empty commit (a file-carrying "block" burned a `suite` strike and an extra
  hardware run);
- `retire:` is only valid for pending/ready/orphaned/escalated tasks;
- `main` accepts only `roadmap.toml` and `docs_paths`.

**Suggestion.** `dashboard lint [<ref>]` (or `dashboard check-commit`) that
classifies what the player would make of the commits ahead of the hub, and says
"this would be rejected as ownership/history/etc." without burning a strike.
Pure read of what the player already knows; no new writer.

---

## 6. Show import status

**Pain.** After a director commit, nothing says whether the player has seen it.
Imports took anywhere from about 20 to 90+ seconds, and `dashboard wait`
returned older events in the meantime. Once a replanned task still showed
`ready` while the replan was unimported, which is exactly the moment a director
might post the wrong task.

**Suggestion.** In `dashboard status`, show the director cursor against the
director clone's head (e.g. "player has imported up to 6e7c1e9; 2 commits
pending"), and/or emit an `imported` event per director commit.

---

## 7. Publish the worker's evidence on block, not only the commit

**Pain.** The player imports a worker's finish or block claim and nothing
else. Evidence the worker gathered in uncommitted or unpublished state (an
accessibility-tree proof committed locally as `2a0ecbc`) was invisible to the
hub and reachable only by reading the worker's tmux pane. The workaround was
to put all evidence in the empty `block:` commit's message body, which works but
is a convention rather than something the player supports.

**Suggestion.** Let a block carry an attachment (a file under a reserved path
such as `blocks/<slug>.md` that the player imports on a block claim without
treating it as a finish). Or at minimum document the "evidence in the block
body" convention in the worker preamble the player prepends.

---

## 8. A sanctioned path for work the operator did outside the loop

**Pain.** The operator rewrote a scraper directly (because the app's UI had
changed so much that driving a worker was slower than doing it). Landing it
meant planning a task whose only content was "cherry-pick these two commits",
running the whole post/finish/review cycle, and carefully keeping the observations
doc off the cherry-pick because it was already on `main`.

**Suggestion.** An operator-initiated "adopt" path, e.g.
`dashboard adopt <slug> --from <ref>`, which treats an existing local branch as
the worker's finish claim (player runs the full suite as it would for any
finish; review and accept proceed as usual).

**Philosophy fit.** The suite still gates it, so nothing is trusted on the
operator's word. The risk is that it becomes a way around task checks, so it
should probably still require a planned and posted task whose check fails first.

---

## 9. Lower priority / ideas

- **Verification tasks whose proof is a file** (`test -f x.md && grep -q CONFIRMED`)
  can be passed by any worker that writes the word. The player can't judge a
  hardware claim, so the review step carries the whole weight. A structured
  verification doc (required sections, required command-output excerpts) that
  the check validates would raise the bar a little without the player judging
  truth.
- **`dashboard status --json`** for scripting, if it doesn't exist.
- **A `ready` event is reused** for "dependencies accepted" and re-emitted after
  replans; an event id on the task state would make it easier to tell a fresh
  `ready` from a repeat.
- **Surface the worker's `agent_cmd` safety posture** in `status` (here
  `--always-approve`), since it is the one setting that decides whether a
  mistaken launch can touch real hardware.

---
---

# Added from the `ai-gpu` project (2026-10-01)

Written after driving a 14-task Rust roadmap (SeedVR2 video upscale) through the
loop with grok workers, director task agents and one operator-gated host
milestone. Same caveat as above: the player source was not read; everything is
inferred from behaviour and the CLI. The loop worked well overall: nothing was
accepted on narration, the suite gate was strict, and a real defect was caught
only because the milestone run was real (see 10). Observed counts: 4 director
rejections in the run (1 `history` on a post, 1 `history` on a replan, 1 `plan`,
1 `ownership`), 1 escalation (self-inflicted, see 1), 1 retire of a task that
needed no work (see 14).

## Confirmations of items above

**#4 (strikes from the player's own merges) is more general than posts.** It hit
a post (`upscale-workflow`), a replan (`upscale-host-qualify2`) and a `plan`
commit (`upscale-lease-floor`). Two things to add:
- The `plan` rejection said "commits may only add tasks/<slug>/task.toml", but the
  commit did only that. The real cause was a stale base (the player had imported a
  `main` commit between my `git fetch hub` and my commit, so the diff against the
  new `hub/main` looked like it also changed `docs/...`). The message misdiagnoses
  a race as a content violation. Naming the race ("your branch is based on
  <old sha>, hub/main is <new sha>") would have saved a guess.
- Task agents (separate sub-agents that write the post commit) lose this race the
  most, because their window between fetch and commit is minutes long. A
  fast-forward-and-replay path (option b) would help them most.

**#6 (import status) also bit through replayed events.** `dashboard wait` printed
`director_rejected` events for commits that had already been fixed and
re-imported, long after the fact, and they read as fresh problems each time.
A rejection event that says "(already superseded by <sha>)" once a later commit
on the same ref has been imported would remove the false alarms. Separately, a
replan that had not been imported yet left the task showing `ready` while its
real dependency list was longer: the same hazard as #6, with the wrong task
postable in that window.

**#1 (wait for the operator) was the cause of the escalation.** I posted an
operator-gated milestone whose check needs a results file the director/operator
produces from a real GPU run. The post launched the worker immediately; it
correctly refused to invent numbers and used `block:`, the task escalated, and the
only exit was retire + replan + a new slug. `needs_operator = true` (or any
"posted but not launched" state) would have turned that into a pause. The
operator-gated pattern is common for hardware/host milestones; it is worth
supporting directly rather than relying on the director to sequence perfectly.

**#2 (resumable env block) matches a second case.** A worker's build failed with
a disk-quota error in the temp filesystem (see 11); it reported `unsure` instead of
blocking, so no strike was spent, but a blocked version would have needed a retire.

## 10. A pattern that worked: the check recomputes the verdict from raw numbers

For the operator-gated milestone the test was written so the recorded GO/NO-GO
is *derived* by the check from a JSON file of raw measurements (the GO rule lives
in the test), and the check also verifies that every number in the doc block
appears verbatim. A doc that claims GO over failing numbers fails the check, and
the worker cannot judge or round. This is the verification-doc idea from #9
made concrete and it worked. It is worth documenting as the recommended shape for
milestone tasks, along with the director rule it implies: produce the raw
measurements file *before* posting (see #1).

Related, and a caution for the docs: "accepted" means "the suite is green", not
"the feature works". All 11 accepted tasks had passing, reviewed checks, and the
real run still stored the *input* file as the "upscale" output (a selection bug in
code the fake-driven tests could not reach) and held a 7977 MiB GPU lease against
a 10639 MiB peak. Both were found only by the real-host milestone. Nothing for the
player to change, but a short note in the README that milestone tasks are the
only place real-world defects are caught would set expectations correctly.

## 11. Disk: a worker clone's `target/` reached 4.5 GB

One worker clone under `.dashboard/workers/<slug>/` built its own cargo
`target/` (4.5 GB for a medium crate). Several clones over a roadmap, plus the
player's own scratch worktree for checks, add up quickly. The temp filesystem of
the director session hit its quota during this run and a worker was stranded.
Suggestions: (a) let `[player]` set per-project env for launched sessions, e.g.
`env = { CARGO_TARGET_DIR = "..." }`, so clones can share a build cache;
(b) delete a clone's build output when its task is accepted or retired (clones
appear to be removed on accept already; confirm for retire); (c) mention disk in
the preamble so the worker avoids creating large artefacts.

## 12. Agent stall detection (an idle session is not a dead session)

With the default `agent_cmd` (`claude --dangerously-skip-permissions`) the first
launch of a clone stalled indefinitely on Claude Code's "Do you trust this folder?"
prompt. The session was alive, so there was no `session_died`, and `status` showed
`in_progress (alive)` the whole time; I only found it by capturing the tmux pane.
Suggestions: (a) a stall signal when a launched agent has produced no commit and no
pane output change for N minutes (surfaced in `status`/`wait` as `idle`, not
auto-killed); (b) `dashboard init` / first player start could warn when
`agent_cmd` was not set explicitly (the default may not run unattended on a fresh
clone); (c) document, next to `agent_cmd`, that the command must be able to start
without any interactive prompt, with a working example for the common agents
(e.g. `grok --model <m> --reasoning-effort medium --always-approve`, which other
projects here already use).

## 13. Heartbeat looks dead while the player is busy running a suite

`dashboard wait` reported "player doesn't seem to be running" while the player was
busy running the project suite (it was healthy and imported the commit a few
seconds later). A heartbeat written only between ticks reads as dead whenever a
check runs for more than ~30 s. Write the heartbeat from a separate timer/thread,
or record "busy: running <check> since <ts>" so `wait` can say "player busy" instead.
Also: `status`/`wait` could print the player's pid and project name. Another
project's player (started days earlier, different cwd) was running on this machine,
and the only way to tell them apart was `/proc/<pid>/cwd`.

## 14. Smaller things

- **`main` ownership rejection should name the offending commit.** It said main
  "touches `docs/spike-still-detail.md`" but not which commit. My own `git add docs`
  had swept in two of the operator's untracked files; a `git show --stat` per
  commit was needed to find it. Include the sha, and ideally say whether the file
  was added or modified.
- **A task that turns out to need no work has no honest outcome.** (`upscale-chain`:
  the behaviour it was to add already worked, so every possible failing check
  already passed and the player correctly refuses such a post.) Retiring with a
  post-mortem worked, and its dependent was replanned, but status then shows a
  retired task as if it were a failure. A `retire` reason class (e.g. `satisfied`)
  that renders differently from a dead end would make the status honest.
- **Phase goals are prose and go stale.** Three phase goals in `roadmap.toml` name a
  task slug (`upscale-host-qualify`) that was later retired and replaced; nothing
  flags the dangling reference. A lint for slugs mentioned in goals that are
  retired/unknown would catch it (fits under `dashboard lint`, #5).
- **`wait` returns replayed history after a long gap** (see #6). `wait --since
  <seq>` or an `--only-new-since-last-wait` mode would help directors that run
  `wait` in a background command and read its output later.

---
---

# Added from an appraisal of the process (2026-10-01)

Moved here from the README's "Appraisal of the process", where each
shortcoming was listed as a limitation. Restated as requests. What the
appraisal said the process gets right (claims must be checkable, pins stop
test tampering, status derives from the hub, failure is budgeted, flaky
checks get a deterministic verdict) is the baseline: none of these should
weaken it.

## 15. A `spike` task kind for exploratory work (v1)

**Status: implemented** as designed below (spec §14, `src/spike.rs`, tests
in `tests/spike.rs`, a scenario in `scripts/e2e.sh`). Deviation: the
director's post may add any file under `tasks/<slug>/` except the
worker's `claims/` and `findings/` (baseline scripts live under
`baseline/`). The open question about `METRICS` is settled as a last
stdout line.

**Pain.** Everything must be a failing check up front. That suits well-scoped
tasks but not design spikes, where nobody knows yet what "working" looks like
and escalate-and-retire is the only escape hatch. The fix is to change what
the worker produces (an answer to a question, backed by runnable claims)
rather than relax the rules for normal tasks.

**Shape of v1.** A spike answers one pinned question. It is initiated by the
operator, discussed by the operator and director, and never merges into
`main`. Nothing may depend on it: no plan is built over a yet unknown answer.
Keeping the operator in the loop is what lets v1 skip budgets, dependency
rules and most of the cheat-proofing.

### Roles

- **Operator.** The only initiator. Decides a spike is worth running (the
  director may suggest one, e.g. after a `block:` on an unknown), approves
  every question and every send-back, reads the findings with the director,
  and decides when the spike is concluded. These approvals are process rules,
  not player checks: the player can't tell whether the operator was asked.
  If a director is ever seen posting or sending back spikes unasked, add an
  enforced authorization then (e.g. `dashboard spike <slug>`).
- **Director.** Writes the question with the operator and commits the post:
  `task.toml` with `kind = "spike"`, `questions/1.toml`, `prompt.md`, and
  baseline claims on the current code (see Claims). Reviews the findings
  against the pinned question, discusses them with the operator, and either
  sends back with a new question (the next round) or commits the
  `conclude:` marker. Lessons that should outlive the spike go to `main`
  through an ordinary `docs_paths` commit.
  The director plans no tasks that depend on a spike.
- **Player.** Validates the post, including that the director's baseline
  claims pass on `main`. Pins the question files and the base commit,
  launches the worker, and on a finish claim requires `findings/<n>.md`
  and runs every claim against the right code version. It never merges the
  branch and doesn't require the project suite. On conclude it moves the
  branch to `refs/explored/<slug>`, next to `refs/retired/`, and keeps it
  forever. Status shows the task as a spike.
- **Worker.** Explores freely, with no path restrictions: it may change any
  file on its branch, since nothing merges and a sandbox would only make the
  codebase harder for it to understand. It writes `findings/<n>.md` and its
  own claims. It cannot touch the pinned files (`task.toml`, `questions/`,
  `prompt.md`, the director's baseline claims), so it can't reword the
  question to fit what it found.

### Layout and rounds

A spike is a sequence of **rounds**. Round 1 is the original question; each
send-back opens the next round with a new, usually sharper, question. Nothing
is overwritten, so the evolution of the question is a file listing, and the
player can read it:

```
tasks/<slug>/
  task.toml, prompt.md       director, pinned at launch
  questions/<n>.toml         director, one per round, append-only
  baseline/<n>.toml          director's baseline claims for round n
  claims/<n>.toml            worker's claims for round n (+ scripts)
  findings/<n>.md            worker's findings for round n
```

The player rejects a commit that edits an earlier round's `questions/` or
`baseline/` file, or that adds round `n+1` without a send-back. A question
may name the round it sharpens (`refines = 1`), which status can draw as a
chain. Each round's findings and claims are the answer to that round's
question only; earlier rounds stay as evidence.

### The question (pinned)

```toml
# tasks/<slug>/questions/1.toml, committed by the director in the post
question = "Can we stream a 1 GB input through X without buffering?"
why = "Decides whether we use X or the chunked fallback"
answers = ["yes", "no", "partial"]   # "inconclusive" is always allowed
refines = 0                          # round it sharpens; omitted in round 1
```

`answers` makes the question falsifiable: the director and operator have to
agree on what a useful answer looks like before anything starts. `why` is
prose for the humans; nothing is enforced from it. Each round has its own
`answers`, so a refinement can ask for a different set.

### Claims (what the player verifies)

A claim is a command the player runs itself in a scratch worktree, so nothing
rests on the worker's report. The contract is the one checks already have:
exit 0 is a pass, anything else a fail, under a per-claim timeout. A claim may
print one last line `METRICS {"throughput": 2.1}`; the player stores that JSON
in the result note so findings can quote it. Two kinds:

- **Baseline claims** describe the code as it is now ("current code streams
  at 1x"). The director supplies them in `baseline/<n>.toml` (pinned) and the
  player verifies at post/send-back time that they pass on `main`, the spike
  counterpart of "a post must have a failing check". The worker may add
  baseline claims in its own `claims/<n>.toml`. The player runs every
  baseline claim against the **base commit** (where the branch forked from
  `main`, pinned at launch so results are reproducible), with
  `tasks/<slug>/` overlaid so the scripts exist there. They must **pass**.
- **Spike claims** describe what the exploration achieved ("we can stream at
  2x"). Only the worker writes them. The player runs them against the
  branch tip, where they must **pass**, and again against the base commit
  with the overlay, where they must **fail**. A claim that passes without the
  spike (`cmd = "true"`) is rejected.

```toml
# tasks/<slug>/claims/1.toml, worker-written
answer = "yes"                       # from questions/1.toml, or "inconclusive"

[[claim]]
name = "streams-at-2x"
kind = "spike"
supports = "yes"                     # which answer this claim backs
cmd = "tasks/stream-x/claims/throughput.sh --expect-min 2x"
timeout = 600
```

`runs`/`min_pass` work as in `tests.toml`, for noisy measurements. Negative
results are legitimate: a spike claim can assert that the attempt hit a wall
(its script lives in the overlay, so it still fails on the base commit).
Results go through the existing check-result cache, keyed by tree, so a
rejection is reproducible and an unchanged claim isn't re-run.

### Findings (free-form in v1)

`findings/<n>.md` is for the director and operator. The player requires that
it exists and isn't empty, and nothing more: no front matter, headings or
quotes are parsed or checked in v1. It may quote claims and their recorded
metrics, but the quotes are not verified. The worker's answer to the
question lives in the claims file instead, where it can be checked (see
below).

### Finish, send-back and conclude

A finish claim is rejected, counting a `claims` strike, unless all of these
hold:

1. `findings/<n>.md` exists for the current round and isn't empty.
2. `claims/<n>.toml` parses and declares an `answer` that is one of the
   current question's `answers`, or `inconclusive`.
3. Every baseline claim passes on the base commit.
4. Every spike claim passes on the branch tip and fails on the base commit.
5. For any answer other than `inconclusive`, at least one passing spike claim
   has `supports` equal to that answer. `inconclusive` needs no claim, but any
   claims it includes are held to rules 3-4: one that doesn't validate is
   rejected, not ignored.

The answer stands only if its claims run and pass. A claim whose command
can't start, times out or exits non-zero counts as failing, and so does a
claims file with no runnable claim behind a non-`inconclusive` answer. Any
failing claim rejects the whole finish, not only the claim that supports the
answer, and the worker has to fix it before the answer is accepted.

The project suite is not required. The player runs and verifies the claims,
but does not judge whether they really support the answer, whether the
findings are right, or whether the question was answered rather than
sidestepped; that is the director's review, with the operator.

A **send-back** adds `questions/<n+1>.toml` (and optionally
`baseline/<n+1>.toml`), and the operator approves it. The player re-pins the
new files and counts a round against `max_review_rounds`. A spike the operator
gives up on is retired as usual. The only automatic limit besides rounds is
the `claims` strike limit: no time budget in v1, because the
operator can stop a spike at any moment.

### Where the answer is stored

In notes, like the rest of the player's derived records, not in a commit
message. The player never writes commits on a spike branch: it stays exactly
as the worker and director left it, and a spike's tip has no player merge
to put a message on. A message would also be free text the player would have
to parse back.

- On every **validated finish commit** the player attaches a note under
  `refs/notes/dashboard-explorations`: `{slug, round, question, answer,
  claims: {name: {kind, passed, metrics}}, at}` (the answer is the
  one declared in the claims file and validated by rules 2 and 5). That is
  the per-round, player-verified answer, and the chain of notes up the
  branch is the evolution of the question.
- On the director's **conclude commit** the player attaches the final
  `{concluded: true, round, answer}` before moving the branch to
  `refs/explored/<slug>`.
- `status` reads the note on the tip of `refs/explored/<slug>` (or, while the
  spike is running, the newest finish note) and shows e.g. `explored: yes
  (round 2)`. The source text stays in `findings/<n>.md` in the tree, so
  nothing is lost if notes are dropped; the note is only the player's
  validated copy, so `status` never needs to parse the free-form findings.

### Not in v1

- A sandbox or any path restriction on the worker.
- `depends_on` a spike, `unblocks`, or any dependent task. A spike never
  satisfies a dependency and never counts toward a milestone.
- Enforced `derived_from`. Follow-up tasks are planned afterwards by the
  director with the operator, as ordinary tasks with ordinary director-written
  checks; the worker may read `refs/explored/<slug>`, as it can read
  retired branches. Copying code from there is allowed and still goes through
  pinned tests and review.
- Player-enforced operator authorization (see Roles), size caps, time
  budgets, running alongside a normal task (see #18).

### Open questions

1. Is "last stdout line `METRICS {json}`" an acceptable claim output
   convention, or should metrics come from a file the claim writes?
2. If findings ever get checked (a later version), the natural step is front
   matter citing claims and quoting recorded metrics, as sketched in the
   discussion; deferred on purpose.

## 16. Detect a live session that has stalled

**Pain.** A dead session is reported but never restarted, and a live one
that stalls without committing isn't detected at all, so hung sessions need
a human. See also #12, which is the same gap seen at launch.

**Suggestion.** Beyond #12's idle signal, a per-project timeout
(`[limits]`, e.g. `stall_minutes`) after which the task is flagged and the
director gets an event. Keep it report-only: no auto-kill, no auto-relaunch,
consistent with the "no auto-relaunch" decision.

## 17. Reduce suite cost on finish claims and merges

**Pain.** The full suite, multi-run checks included, runs on every finish
claim and every merge, so cost grows with the suite.

**Suggestion.** Options: (a) run cheap checks first and stop at the first
failure; (b) reuse the `refs/notes/dashboard` result cache when a tree is
identical to one already checked (e.g. a merge that changes nothing the
check reads); (c) let a check declare the paths it depends on so unrelated
changes skip it. Whatever is skipped must stay derivable from the hub; the
accept gate should still run everything.

## 18. Parallel tasks

**Pain.** Only one task can be posted, in flight, or escalated at once.
That keeps `main` simple but leaves throughput low.

**Suggestion.** Start with the safe cases: spikes (#15), which never touch
`main`, then tasks whose declared paths don't overlap. This is still
undecided in spec §13.

## 19. Catch tests being gamed, not only edited

**Pain.** Pins stop edits to the tests, not code that special-cases them or
a new file that patches around them. Catching that depends entirely on the
director's review.

**Suggestion.** Give review something concrete to look at, without the
player judging truth: e.g. flag a finish whose diff mentions a pinned test's
name, fixture value or path, or that adds files matching test patterns
outside `test_paths`. A flag for the director, not a rejection.

## 20. Say plainly that agents share a Unix user

**Pain.** The design raises the effort needed to cheat and makes it visible,
but it is not a security boundary: a determined agent running as the same
user could write to the hub directly.

**Suggestion.** Document this in the README next to the guarantees. Optionally,
run workers as a separate user, or make the hub's refs writable only by
the player, as a hardening step for anyone who needs it.
