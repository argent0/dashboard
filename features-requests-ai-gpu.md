# Feature requests from the `ai-gpu` project

Written 2026-10-02 after ~36 tasks through the loop (Rust service, `cargo test`
as the single `unit` check). Everything here is a **suggestion**: if it cuts
against the player's philosophy (single writer, never trusts narration, status
is a pure function of the hub), drop it or reshape it. The pain is real; the
proposed fix is only one way to address it.

Evidence comes from observed behaviour and from `spec.md` §6.1 and §10. The
player source was not read.

---

## 0. A `flaky-test-fix` task kind (the real gap; recommended first)

**Pain.** Fixing a bug *in a test* cannot be expressed today. The test tree is
pinned, so the subagent may not edit it. A post must carry a check that fails
now, but a flaky test is green most of the time, so there is no honest failing
check. And the director cannot do the fix inside the post commit: the check
would already pass and the post is rejected (§6.3). Every workaround is a hack
(a source-scan lint test, a higher `suite` strike limit, a hand edit outside the
loop). Section 1 below is the instance that exposed this.

**Idea.** A `flaky-test-fix` task *replaces* a flaky test with a stable
one. The director proves the old test is flaky and pins what "stable" means.
The subagent writes the replacement. The player runs all of it.

**Scope: flaky tests only.** A test that does not test the right thing (a
wrong or missing assertion) is not this path: the fix is a *new* test, and
that is motivated by a code change, so it belongs in an ordinary task whose
post carries the failing test. `flaky-test-fix` exists for the one case that
has no honest failing check: a test that sometimes passes and sometimes fails.

```toml
kind = "flaky-test-fix"
fix_paths = ["tests/face_ffmpeg_fix.rs", "tests/common/stub.rs"]

[[replace]]
old = "ffmpeg_ops_run_the_built_args"
new = "ffmpeg_ops_run_the_built_args_stable"

[[evidence]]               # the old test is flaky: run on main
name = "etxtbsy-flaky"
cmd = "scripts/stress.sh ffmpeg_ops_run_the_built_args"
runs = 100
min_pass = 1               # it works sometimes (not just broken)...
max_pass = 99              # ...and fails sometimes

[check]                    # the replacement is not flaky (§6.3 task check)
name = "stable"
cmd = "scripts/stress.sh ffmpeg_ops_run_the_built_args_stable"
runs = 100
min_pass = 100
```

- **Pins.** Only `fix_paths` are unpinned, for this task only. These are
  files: the player can't tell which test in a file a diff touched. The
  director lists the helper files here too, never a wildcard. Everything else
  stays enforced (`pins` strike as usual). `tasks/<slug>/`, so the evidence
  and the acceptance check, stays pinned.
- **Post.** The evidence must show flakiness on `main`: at least `min_pass`
  and at most `max_pass` of `runs` pass, so a test that always fails, or
  never fails, is rejected. (`max_pass` is new; `min_pass` is §6.1's.) A
  deterministic repro, such as a lint that flags the race, may be added as
  extra evidence but doesn't replace this. The task check must fail too,
  which holds because the replacement doesn't exist yet. The stress `[check]`
  is **not** added to `tests.toml`, an exception to §6.3: it is a one-off
  proof for this task, and a 100-run check would otherwise slow every future
  suite. The replacement test itself joins the suite as usual. A post whose
  evidence shows no flake is not a `post` strike: see *No repro* below.
- **Finish.** The suite must pass, with these test-tree rules replacing the
  plain pin on the replaced tests:
  - every `old` test is gone or still present, but no other test is removed
    or renamed (inventory, e.g. `cargo test -- --list`, is a superset of
    `main`'s minus the `old` names);
  - every `new` test exists;
  - the director's task check passes on the branch, so the player, not the
    subagent, decides it is stable.
- **No weakening.** The player can't see deleted assertions, loosened
  thresholds, or a helper that quietly neuters other tests. The task agent's
  review reads the diff for that, helper files in particular, as it checks for
  gaming today.
- **Rest of the lifecycle unchanged:** `max_review_rounds` applies, `accept:`
  merges into `main`, and nothing may depend on it unless the director wants
  an ordering.

**No repro.** The evidence is the director's claim that the test is flaky,
and a statistical claim can miss by chance. When the player runs the evidence
on `main` (at post, and again at launch) and it does not reproduce:

- the post is rejected with a new reason `evidence`, not `post`: the director
  made no structural mistake, the flake just did not show up;
- the strike limit for `evidence` defaults to 1, so the task is escalated on
  the first miss and **the player retires it itself**: it moves the branch to
  `refs/retired/<slug>` and records the post-mortem `evidence did not
  reproduce (k/N)`. No `retire:` commit is needed, because there is no
  subagent session and no work to review.
- the check result is cached by commit, so reposting the same commit would
  yield the same verdict. To try again the director plans a new slug with a
  larger `runs`. Slugs are never reused, so the dead end stays in the history.

This is a small spec change (§9 and §10): today only the director retires,
and only through a `retire:` marker. Auto-retire stays within the player's
role because it already moves branches on retire, and it limits how deep an
unlikely flake can drag the loop.

---

## 1. Don't charge a `suite` strike for an environmental flake in a pinned test

(Complementary to section 0: this reduces how often a flake costs a strike, while
section 0 gives the way to fix the test once it is known.)

**Pain.** Task `face-frame-count-fix` was implemented correctly, and its own
check passed. The player's `unit` check (`cargo test`) then failed on the first
finish claim, in a *different, pinned* test (`tests/face_runner.rs`,
`ffmpeg_ops_run_the_built_args`):

    Spawn { message: "Text file busy (os error 26)" }

That is Linux `ETXTBSY`: the test writes a shell stub, `chmod +x`, and execs it
immediately, which races with the still-open write fd. It is a property of the
test and the machine under parallel load, not of the subagent's diff.

Consequences:

- The failure was recorded as a `suite` strike against the subagent (limit 2),
  so one unlucky run burned half the budget of an escalation.
- The subagent, reading "Text file busy", reasonably assumed production code
  was guilty and changed production (added a spawn retry). It happened to be a
  valid hardening, but nothing about the task required it: scope creep caused
  by a misleading verdict.
- A second pinned test (`tests/face_ffmpeg_fix.rs:76`) has the same race on its
  own bare `Command` call, which no production change can fix and the subagent
  may not edit (pinned). Observed 1/30 under 16-thread stress, 0/5 in normal
  runs. If it fires, a correct task takes a strike it cannot do anything about.

§6.1 already says a threshold check "can still fail by chance on a correct
implementation... absorbed by a higher suite limit". That only works by
loosening the strike limit for the whole project, which also weakens the signal
for real failures.

## Suggestion

Keep the player the sole judge of what failed, but let it re-confirm a failure
before blaming the subagent. Any of these, in increasing ambition:

### 1a. `retry_failed` on a check (smallest, recommended)

Optional per-check field in `tests.toml`:

```toml
[[check]]
name = "unit"
cmd = "cargo test"
retry_failed = 1     # on failure, re-run the check up to N more times
```

Semantics: on a failing run the player re-runs the check; the check passes if
any attempt passes (the existing `runs`/`min_pass` rules apply per attempt, or
simply "N extra attempts after a failed verdict"). Every attempt is recorded in
the `refs/notes/dashboard` cache, and `status` shows `passed on retry 2/2` so a
flaky pass is visible rather than silent. Director-owned and pinned like the
rest of the check definition, so the subagent cannot raise it.

Cost: a genuinely failing suite takes `N + 1` times as long to be rejected.
Worth it for a 1-minute `cargo test`; optional per check for slow ones.

### 1b. `flaky_patterns`: retry only when the output looks environmental

```toml
[[check]]
name = "unit"
cmd = "cargo test"
retry_failed = 2
retry_on = ["Text file busy", "os error 26", "Address already in use"]
```

Retry only if the failing output matches one of these regexes; any other
failure is a strike immediately. Narrows the blind spot of 1a: a real
regression can never be masked by a retry unless it prints one of the director's
listed signatures.

### 1c. Flake ledger in status (visibility, works with either of the above)

When a check needed a retry, record `(task, check, attempt outputs)` and show a
`flaky: N` marker on the task and a short list in `status`. The director can
then plan a hygiene task (here: wrap the stub-exec in the test helpers) with
evidence instead of anecdotes. Without this, 1a/1b would hide a real problem
that is only being papered over.

## Non-goals / things I would not do

- Don't auto-forgive a strike after the fact based on narration from the
  subagent ("this is a flake"). The player should only trust a re-run it did
  itself.
- Don't let a retry apply to the task's *own* new check on the post step: a
  post whose check fails must keep meaning "fails for the reason the task
  exists" (§6.3), and retrying there would only waste time.
- Don't make the subagent aware of retries in its preamble; the pass/fail
  verdict it is told about should stay a plain verdict.

## Acceptance sketch

- A check with `retry_failed = 1` whose first run fails and second passes
  yields `tested`, no `suite` strike, and a visible `flaky` note.
- The same check failing twice yields one `suite` strike (not two).
- With `retry_on` set, a failure whose output matches nothing in the list is
  not retried.
- Removing the field restores today's behaviour exactly.
