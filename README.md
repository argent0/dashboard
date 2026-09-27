# dashboard

`dashboard` is the enforcement point for a subagent-per-task roadmap loop.
It keeps a bare git hub as the single source of truth and runs a *player*
process that imports and validates every commit a director or subagent
produces, runs every check itself, launches subagents in `tmux`, counts
strikes, merges accepted work, and retires dead ends. The status it renders
is a pure function of the hub: nobody writes a status by hand. See
[`spec.md`](spec.md) for the full design.

## Appraisal of the process

### What it gets right

- **Claims must be checkable.** A task can't be posted without a failing
  check, finished without a passing suite, or sent back without a new
  failing test. Neither "done!" from a subagent nor a vague objection from
  the director counts; only a result the player ran itself does.
- **Pins stop test tampering.** The test tree and task files are hashed at
  launch, so a subagent can't quietly edit a test or lower a threshold to
  get a pass.
- **Status can't drift.** It is derived from refs, notes, and session
  liveness, so there is no hand-edited file to fall out of date.
- **Failure is budgeted.** Strike limits and review rounds stop agents
  from looping forever; an escalated task is retired rather than forced
  through, and its branch and post-mortem stay in the hub as history.
- **Flaky checks get a deterministic verdict.** `runs`/`min_pass` lets
  LLM-backed or otherwise noisy tests take part in the same pass/fail rules.

### Where it falls short

- **It is only as good as the tests.** Pins stop edits to the tests, not
  code that special-cases them or a new file that patches around them.
  Catching that still depends on the director's review.
- **Not a security boundary.** Every agent runs as the same Unix user; the
  design raises the effort needed to cheat and makes it visible, but a
  determined agent could still write to the hub directly.
- **One task at a time.** Only one task can be posted, in flight, or
  escalated at once, which keeps `main` simple but leaves throughput low.
- **Cost grows with the suite.** The full suite, including multi-run
  checks, runs on every finish claim and merge.
- **Hung sessions need a human.** A dead session is reported but never
  restarted; a live one that stalls without committing isn't detected.
- **Rigid for exploratory work.** Everything has to be expressed as a
  failing check up front, which suits well-scoped tasks better than design
  spikes, where escalate-and-retire is the only escape hatch.
