# Feature requests

Consolidated 2026-10-16 from the future-term and personal-assistant request
files, pruned against `spec.md` and `CLAUDE.md`. Suggestions only: drop or
reshape any that cut against the player's philosophy (single writer, no
trust in narration, status a pure function of the hub).

Already landed and removed from here: task agents committing lock files
(`[player] lock_files`), player-resolved lock-file merge conflicts
(`lock_regen`), per-check timeouts (`[budget] check_timeout`, spec §20 and
CLAUDE.md "Suite time budget"), spikes, stall and idle detection, parallel
tasks, flaky-fix and check retries.

## Bugs and rejections

- **`conclude:` records the worker's answer, not the director's.**
  `spike-grid-render` round 1: the worker's `claims/1.toml` said
  `answer = "webgl"`; the director committed `conclude: dom-rows`; the
  `explored` event and `refs/notes/dashboard-explorations` both say
  `concluded: webgl`. `try_conclude` reads the answer from the note. Ask: take
  it from the `conclude: <answer>` subject, validate it against the round's
  `answers` (plus `inconclusive`) and reject the marker (`ownership`) if it
  isn't one. Keep the worker's pick in the note as `recommended` so `status`
  can show both when they differ.
- **A plan/post/replan based on a slightly stale `main` is rejected as a
  revert (or `history`).** The player merges `main` into task branches, or
  imports a new `main` commit while the director branches from the older tip,
  and the diff against the current tip "removes" the new lines. Hit a post, a
  replan and a `plan` commit (`plan: task/ui-grid-renderer's commits may only
  add tasks/ui-grid-renderer/task.toml`, which wrongly claimed the commit
  touched `docs/`). Options: diff against the merge base (`main...candidate`);
  don't merge into unposted branches; fast-forward-and-replay when the parent
  is an older hub tip of the same branch; and at least name the race ("based
  on <old>, hub/main is <new>") with the exact fetch/reset command, and no
  strike. Task agents already get a rebase request instead of a strike;
  directors don't.
- **A broken contest reply can't be fixed until the worker contests again.**
  `core-resize`: the agent's contest reply c1 had a one-character typo
  (`"\&"` in a Rust string), so the pinned test no longer compiled. The
  follow-up fix was rejected because the task was back `in_revision` and the
  worker can't edit the pinned test; the worker had to contest again, spending
  its last contest (`max_contest_rounds = 2`) on the reply's own mistake. Any
  of: validate a reply (build/run the suite, reject one whose tests don't
  compile or whose task check errors rather than fails); allow one corrective
  agent commit limited to the test files its last reply changed while the
  worker hasn't committed since (re-pin and relay); don't count a contest
  about a file the previous reply changed.
- **False `session_idle` reports.** Seen three times with grok workers: the
  finish was already committed but grok still showed "Responding…"; the
  player's contest reply sat in grok's input queue ("▾ Queued 1"); the
  "finish accepted" message was already in the input box. Ask: before emitting
  `session_idle`, re-fetch the worker clone and don't report if there's an
  unimported commit or the task's state changed since the stop; for grok,
  treat a visible "Responding…" spinner or non-empty "Queued" list as busy.
- **Ownership rejection on `main` should name the commit** (sha, and whether
  the file was added or modified). A stray `git add docs` swept in untracked
  files and needed `git show --stat` per commit to find.

## Operator and director workflow

1. **Wait for the operator before launching.** Posting launches the worker at
   once. For hardware or host milestones (locked phone, dropped adb, GPU
   results the operator must produce) the run fails environmentally and a
   `block:` escalates the task, so the only exit was retire + replan + new
   slug. Ask: `needs_operator = true` in `task.toml`; validated as usual but
   no launch until `dashboard go <slug>`; status shows `posted (waiting for
   operator)`. The operator action is recorded, so no trust issue.
2. **Resumable environmental block.** Escalated tasks are never resumed. A
   worker-marked `block(env):` could park the task as `paused` until
   `dashboard resume <slug>`, capped (one resume per task, or only when the
   operator classifies it) so it can't resume a flailing worker. Also fits a
   disk-quota build failure a worker reported as `unsure`. (Only the
   relaunch-limit escalation is resettable today.)
3. **Exit for a posted task whose session is dead.** `retire` is refused for
   posted tasks, so the only way out was `relaunch` (against real hardware)
   then a hand-made `block:`. Ask for one of: `dashboard abandon <slug>`
   (escalates with a recorded reason), auto-escalate after N minutes dead, or
   `relaunch --no-agent`.
4. **Dry run for director commits.** The rules are learned by rejection (a
   file-carrying `block:` is a finish claim and cost a `suite` strike and a
   hardware run; `retire:` only on some states; `main` only takes
   `roadmap.toml` and `docs_paths`). Ask: `dashboard lint [<ref>]`, a pure read
   saying what the player would make of unimported commits. Could also flag
   phase goals naming retired or unknown slugs.
5. **Show import status.** Imports take 20-90 s; meanwhile `wait` replays old
   events and a not-yet-imported replan leaves the wrong task `ready` and
   postable. Ask: status shows "imported up to <sha>, N commits pending"
   and/or an `imported` event per director commit; mark a `director_rejected`
   event "superseded by <sha>" once a later commit on that ref imported; and
   `wait --since <seq>` to avoid replayed history.
6. **Evidence with a block.** Evidence the worker gathered (e.g. an
   accessibility-tree proof) is invisible to the hub; the workaround is the
   empty `block:` commit body. Ask: a reserved `blocks/<slug>.md` imported on
   a block without counting as a finish, or at least document the convention
   in the worker preamble.
7. **Adopt operator work done outside the loop.** Landing a hand-written
   scraper took a "cherry-pick these commits" task plus the full cycle. Ask:
   `dashboard adopt <slug> --from <ref>` treating a local branch as the finish
   claim, still on a posted task whose check fails first and still gated by
   the suite, so it can't bypass checks.

## Player visibility and robustness

- **Heartbeat during long checks.** Checks now time out, but a suite run
  inside `tick` still stops the heartbeat, so `wait`/`status` say "player
  doesn't seem to be running" past 30 s and every other import, retire and
  event waits. Write the heartbeat from a separate thread, or record "busy:
  <check> on <slug> since <ts>" so `status`/`wait` show "running check X on
  <slug> for 12 min". Show the player's pid and project too (two players ran
  on one machine; only `/proc/<pid>/cwd` told them apart).
- **Concurrent checks.** Longer term: run checks for different tasks
  concurrently so one slow task doesn't stall imports for everyone.
- **Disk.** A worker clone's `target/` hit 4.5 GB and the temp quota stranded
  a worker. Ask: `[player] env = { CARGO_TARGET_DIR = ... }` for launched
  sessions, build output deleted on accept and retire (verify retire), and a
  disk note in the worker preamble.
- **`agent_cmd` must start unattended.** Idle detection reports a stall on
  Claude Code's trust dialog, but `init` could still warn when `agent_cmd`
  isn't set explicitly, and docs should say it can't prompt and give working
  examples (e.g. `grok --model <m> --reasoning-effort medium
  --always-approve`).
- **Show `agent_cmd` safety posture** (e.g. `--always-approve`) in `status`;
  it decides whether a mistaken launch can touch real hardware.
- **`satisfied` retire class.** A task whose behaviour already works can never
  have a failing check, and the player rightly refuses the post. Retiring
  works but status renders it like a failure; a `satisfied` reason would
  render as honest.
- **Event ids.** `ready` is reused for "dependencies accepted" and re-emitted
  after replans; an id would separate a fresh `ready` from a repeat.
- **`status --json`** for scripting.

## Test quality and docs

- **Structured verification docs.** A file-only proof (`grep -q CONFIRMED`)
  passes by writing the word. Pattern that worked: the check derives the
  verdict (GO/NO-GO) from a raw-measurements JSON and verifies every number in
  the doc appears verbatim. Document it as the milestone shape, with the rule
  to produce the raw file *before* posting. Also note in the README that
  "accepted" means the suite is green, and only real-device milestones caught
  real defects (input stored as output; a 7977 MiB lease vs a 10639 MiB peak).
- **Cheaper suite on finish and merge** (spec §13, undecided). Results are
  already cached by tree in `refs/notes/dashboard`. Still open: run cheap
  checks first and stop at the first failure; let a check declare the paths it
  depends on so unrelated changes skip it. Whatever is skipped must stay
  derivable from the hub, and the accept gate should still run everything.
- **Catch gamed tests, not only edited ones.** Pins don't stop code that
  special-cases a test or a new file that patches around it. Flag (don't
  reject) a finish whose diff mentions a pinned test's name, fixture value or
  path, or adds test-pattern files outside `test_paths`, for the director.
- **README note on the Unix-user boundary.** `spec.md` §3.3 says it; the
  README guarantees section should too. Optional hardening: workers as a
  separate user, or hub refs writable only by the player.
