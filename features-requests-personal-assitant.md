# Feature requests (personal-assistant, ai-gpu, appraisal)

Written 2026-10-01 from ~20 tasks on an Android phone (grok worker), a
14-task Rust roadmap, and a process appraisal. Suggestions only: drop or
reshape any that cut against the player's philosophy (single writer, no
trust in narration, status a pure function of the hub). The player source
was not read when these were written. Pruned 2026-10-08: spikes, stall and
idle detection, parallel tasks, flaky-fix and check retries have landed
(spec §14-§20, `CLAUDE.md`) and are gone from here.

## Priority

1. **Wait for the operator before launching.** Posting launches the worker at
   once. For hardware or host milestones (locked phone, dropped adb, GPU
   results the operator must produce) the run fails environmentally, and a
   `block:` escalates the task, so the only exit was retire + replan + new
   slug. Ask: `needs_operator = true` in `task.toml`; the post is validated as
   usual but no launch until `dashboard go <slug>`; status shows `posted
   (waiting for operator)`. Operator action is recorded, so no trust issue.
2. **Resumable environmental block.** Escalated tasks are never resumed. A
   worker-marked `block(env):` could park the task as `paused` until
   `dashboard resume <slug>`. Needs a cap (one resume per task, or only when
   the operator classifies it) so it can't resume a flailing worker. Also
   fits a disk-quota build failure that a worker reported as `unsure`.
3. **Exit for a posted task whose session is dead.** `retire` is refused for
   posted tasks, so the only way out was `relaunch` (against real hardware)
   then a hand-made `block:` in the worker clone. Ask for one of:
   `dashboard abandon <slug>` (escalates with a recorded reason),
   auto-escalate after N minutes dead, or `relaunch --no-agent`.
4. **No strike when the player's own merge caused the divergence.** The
   player merges `main` into task branches, so the director's (or task
   agent's) next commit is rejected as `history`. Hit a post, a replan and a
   `plan` commit. The `plan` rejection even claimed the commit touched
   `docs/`; the real cause was a stale base after `main` moved. Options: don't
   merge into unposted branches; fast-forward-and-replay when the parent is
   an older hub tip of the same branch; at least name the race ("based on
   <old>, hub/main is <new>") and give the exact fetch/reset command. (Task
   agents already get a rebase request instead of a strike; directors don't.)
5. **Dry run for director commits.** The rules are learned by rejection (a
   file-carrying `block:` is a finish claim and cost a `suite` strike and a
   hardware run; `retire:` only on some states; `main` only takes
   `roadmap.toml` and `docs_paths`). Ask: `dashboard lint [<ref>]`, a pure
   read saying what the player would make of unimported commits. Could also
   flag phase goals naming retired or unknown slugs.
6. **Show import status.** Imports take 20-90 s; meanwhile `wait` replays old
   events and a not-yet-imported replan leaves the wrong task `ready` and
   postable. Ask: status shows "imported up to <sha>, N commits pending"
   and/or an `imported` event per director commit. Also mark a
   `director_rejected` event "superseded by <sha>" once a later commit on
   that ref imported, and let `wait --since <seq>` avoid replayed history.
7. **Evidence with a block.** Evidence the worker gathered (e.g. an
   accessibility-tree proof) is invisible to the hub; the workaround is a
   convention of putting it in the empty `block:` commit body. Ask: a
   reserved `blocks/<slug>.md` imported on a block without counting as a
   finish, or at least document the convention in the worker preamble.
8. **Adopt operator work done outside the loop.** Landing a hand-written
   scraper took a task whose content was "cherry-pick these commits" plus the
   full cycle. Ask: `dashboard adopt <slug> --from <ref>` treating a local
   branch as the finish claim, still on a posted task whose check fails first
   and still gated by the suite, so it can't bypass checks.

## Smaller

- **Heartbeat during long checks.** `wait` said the player looked dead while
  it ran a >30 s suite. Write the heartbeat from a separate thread, or record
  "busy: <check> since <ts>". Show the player's pid and project in
  `status`/`wait` (two players ran on one machine; only `/proc/<pid>/cwd`
  told them apart).
- **Disk.** A worker clone's `target/` hit 4.5 GB and the temp quota stranded
  a worker. Ask: `[player] env = { CARGO_TARGET_DIR = ... }` for launched
  sessions, build output deleted on accept and retire (verify retire), and a
  disk note in the worker preamble.
- **`agent_cmd` must start unattended.** Idle detection now reports a stall
  on Claude Code's trust dialog, but `init` could still warn when `agent_cmd`
  isn't set explicitly, and docs should say it can't prompt and give working
  examples (e.g. `grok --model <m> --reasoning-effort medium
  --always-approve`).
- **Ownership rejection on `main` should name the commit** (sha, and whether
  the file was added or modified). A stray `git add docs` swept in untracked
  files and needed `git show --stat` per commit to find.
- **`satisfied` retire class.** A task whose behaviour already works can
  never have a failing check, and the player rightly refuses the post.
  Retiring works but status renders it like a failure; a `satisfied` reason
  would render as honest.
- **Event ids.** `ready` is reused for "dependencies accepted" and re-emitted
  after replans; an id would separate a fresh `ready` from a repeat.
- **Show `agent_cmd` safety posture** (e.g. `--always-approve`) in `status`;
  it decides whether a mistaken launch can touch real hardware.
- **Structured verification docs.** A file-only proof (`grep -q CONFIRMED`)
  passes by writing the word. Pattern that worked: the check derives the
  verdict (GO/NO-GO) from a raw-measurements JSON and verifies every number
  in the doc appears verbatim. Document it as the milestone shape, with the
  rule to produce the raw file *before* posting. Also note in the README that
  "accepted" means the suite is green, and only real-device milestones caught
  real defects (input stored as output; a 7977 MiB lease vs a 10639 MiB peak).
- **`status --json`** for scripting, if absent.

## From the appraisal

- **Cheaper suite on finish and merge** (spec §13, undecided). Results are
  already cached by tree in `refs/notes/dashboard`. Still open: run cheap
  checks first and stop at the first failure; let a check declare the paths
  it depends on so unrelated changes skip it. Whatever is skipped must stay
  derivable from the hub, and the accept gate should still run everything.
- **Catch gamed tests, not only edited ones.** Pins don't stop code that
  special-cases a test or a new file that patches around it. Flag (don't
  reject) a finish whose diff mentions a pinned test's name, fixture value or
  path, or adds test-pattern files outside `test_paths`, for the director.
- **README note on the Unix-user boundary.** `spec.md` §3.3 says it; the
  README guarantees section should too. Optional hardening: workers as a
  separate user, or hub refs writable only by the player.
