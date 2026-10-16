# dashboard

The enforcement point for a worker-per-task roadmap loop (see `spec.md`,
the design source of truth). `dashboard` is no longer a stateless renderer
of a hand-edited `tasks.yaml`: it's the **player** — a bare git hub plus a
process that imports and validates every commit a director, task agent or
worker clone produces, runs every check itself, launches task agents and
workers in `tmux`,
counts strikes, merges accepted work, retires dead ends, and renders a
status that's a pure function of the hub. Nobody writes a status; git (plus
whether a worker's session is alive) is the only source of truth. This is
greenfield: there is no compatibility with the old `tasks.yaml` format, and
none is planned.

Installed as a `dashboard` binary on `PATH` (`cargo install --path .`) and
invoked by other repos' directors and by `~/.claude/skills/dashboard` (the
director playbook for this CLI). See "Installing" below.

## Repo layout

```
src/main.rs     CLI (clap): init / status (default) / watch / player / relaunch / ask / wait
src/lib.rs      the library crate every module below hangs off of
src/config.rs   roadmap.toml / tests.toml / task.toml schemas (serde) + defaults
src/git.rs      thin sync wrapper over the `git` CLI (rev-parse, diff, notes, merge-tree, …)
src/paths.rs    XDG home layout and `--home` / `dashboard.home` resolution
src/init.rs     `dashboard init`: creates the hub and wires up the director <-> hub links
src/queue.rs    queued runs: `dashboard test` snapshot + request files, results
src/idl.rs      the API IDL (`<name>.api.kdl`, KDL): parse/validate, constructor and T-slot counts, Rust binding generator
src/lint.rs     consumer-contract lints (tree-sitter): wildcard arms over an API's sum type, wire-helper leaks
src/arch.rs     `dashboard arch check` (architect-v3.md): modules budget, per-module bytes/AST (tree-sitter),
                file ownership, tests in each module's jail view
src/checks.rs   check runner (a scratch worktree + `sh -c`) and the notes-based result cache
src/records.rs  provenance/enforcement records: import notes, rejections, pins, retirements, accepted-merge lookup,
                spike base commits and exploration notes
src/flaky.rs    flaky-test-fix (spec §15): inventory comparison, evidence verdict text
src/speedup.rs  test-speedup (spec §15.1): scope check, the ratchet, removal text (pure)
src/fsm.rs      the status gate as a table: who (director/task agent/worker) may do what at which
                status -> allow / reject(reason) / ignore / hold; tests walk every cell. player.rs
                asks it for every status/actor gate (markers, posts, send-backs, replies,
                resolves, worker imports, agent commits); content checks stay in player.rs
src/prompts.rs  prompts.kdl (spec §19): template validation, the max-bytes cap, rendering
src/prompts.kdl the starter prompts.kdl `dashboard init` commits (embedded with include_str!)
src/sandbox.rs  the check jail (spec §20): bubblewrap command lines, policy fingerprint, preflight
src/spike.rs    spikes (spec §14): reading a round, running claims against the base-with-overlay, evaluating a finish
src/state.rs    derives every task's status (spec §12) purely from the hub + session liveness
src/player.rs   the player: fetch, validate, import, launch/relay, merge, task agents — spec §5/§7/§9/§11/§17
src/events.rs   the director-relevant event log, cursor, and heartbeat behind `dashboard wait`
src/render.rs   renders `state::HubState` + recent events to the status text
src/ui.rs       styling/width helpers (color codes, bars, truncation, terminal width)
```

`Cargo.toml`: `serde`/`serde_json`/`toml` for parsing, `kdl` for
`prompts.kdl`, `clap` for the CLI;
`tempfile` as a dev-dependency for the integration tests in `tests/`, which
build real throwaway git repos. Ordinary Rust that can keep growing more
crates and surface — there's no zero-deps constraint.

## On-disk layout (a project's dashboard home)

Resolved via `--home`, else `git config dashboard.home` read from the repo
in the current directory (set by `dashboard init` in the director clone).
`$XDG_DATA_HOME/dashboard/<project>/` (fallback `~/.local/share/dashboard`)
unless `--home`/`--name` override it:

```
hub.git             bare repo — the only source of truth (spec §4)
work/               the player's own scratch worktree for running checks
events.jsonl        append-only, JSON per line — the director-event log
director.cursor     the director's last-read event seq (dashboard wait)
player-state.json   last-seen status per slug, and per session the launch
                     id whose death was reported, the stop reported
                     idle (`idle_told`) and its last screen hash, since
                     when and whether told (`panes`), so a restarted player
                     doesn't re-emit old events (an older bare
                     {slug: [status, alive]} map is migrated on load)
launches/<key>      the latest launch id of a session (<slug>, or
                     <slug>_agent), written by whoever launched it
player.heartbeat     unix time of the last tick, so `wait` can tell the
                     player looks dead (missing or >30s old)
```

Worker clones live *outside* this home, nested inside the **director's own
working tree** instead (`paths::workers_root`/`worker_clone`):

```
<director-clone>/.dashboard/workspace/          the one clone of the hub they're all worktrees of
<director-clone>/.dashboard/workers/<slug>/   a worker's worktree (branch task/<slug>)
<director-clone>/.dashboard/agents/<slug>/      a task agent's worktree (branch agent/<slug>, spec §17)
```

The player writes each agent's prompt to `dashboard-prompt.md` in that
worktree's git directory; a task agent leaves its report there as
`dashboard-report.md`, which the player reads and removes (leaving an
empty `dashboard-reported` behind). Every launch also merges Claude Code
hooks into the worktree's `.claude/settings.local.json` (hidden from git:
`info/exclude`, or `--skip-worktree` if the project tracks it): `Stop` /
`StopFailure` write `stop|error <unix> <pid>` to `dashboard-stopped` in
the git dir, `UserPromptSubmit` removes it. See "Idle sessions" below.

A worker clone is a fresh directory the player creates with nobody present
to click through an agent CLI's first-run workspace-trust dialog. Nesting it
inside the director's project directory means it inherits whatever trust the
operator already granted that directory through their own ordinary use,
instead of the player needing to (and being unable to safely) grant that
trust itself. `dashboard init` adds `/.dashboard/` to the director repo's
`.gitignore` (committing the change on `main` if needed) so this never shows
up as untracked noise in the operator's own `git status`.

## Hub refs and notes

```
refs/heads/main            director-owned; player-only writes except
                            roadmap.toml / prompts.kdl / docs_paths commits (spec §5)
refs/heads/task/<slug>     one per task, never deleted even once accepted
                            (spec §8.1) — moved, not deleted, on retire
refs/retired/<slug>        a retired task's branch, moved here by the player
refs/explored/<slug>       a concluded spike's branch, moved here on `conclude:` (spec §14)
refs/incoming/director/*   the director clone's main/task/* fetched in,
                            pre-validation (src/player.rs's `fetch`)
refs/incoming/worker/<slug>  a worker clone's task/<slug>, pre-validation
refs/incoming/agent/<slug>     a task agent clone's agent/<slug>, pre-validation (spec §17)
refs/dashboard/agent/<slug>    the tip a task's task agent was launched at; never moved
refs/dashboard/agent-asked/<slug>  the tested tip its task agent was last asked to review
refs/dashboard/pins/<slug> the commit pins were computed from (spec §8.3)
refs/dashboard/base/<slug> a spike's base commit (the tip at launch); never moved
refs/dashboard/escalated/<slug>  set when a send-back would exceed
                            max_review_rounds, a contest max_contest_rounds,
                            or a relaunch max_relaunches (the other escalation causes —
                            a strike limit, a block: marker — are read
                            straight off strikes/notes, no separate ref)
refs/dashboard/escalation-cleared/<slug>/<n>  a relaunch-limit escalation the
                            operator cleared (`relaunch --reset`), moved here
refs/dashboard/held/<slug> a worker finish held, not judged: it fails only on
                            checks already impure on main (spec §20)
refs/dashboard/released/<slug>  a held finish the player merged main under on
                            release; ignored (not "diverged") until rebased
refs/rejected/<scope>/<n>  every rejected commit, kept as evidence
                            (scope is a task slug, or `_main`)
```

Notes (JSON payloads, `git notes --ref <ref>`):

```
refs/notes/dashboard             check-result cache: {"checks": {name: CheckResult}}
                                 (a result carries `millis`, `timeout`, `timed_out`)
refs/notes/dashboard-imports     provenance per imported commit (actor, transition, slug, at)
refs/notes/dashboard-rejections  reason/actor/message/at per refs/rejected/* commit
refs/notes/dashboard-escalations cause/at per refs/dashboard/escalated/<slug>
                                 (+ relaunch_limit: "worker"|"agent" for a relaunch-limit one)
refs/notes/dashboard-escalations-cleared  per escalation-cleared commit:
                                 [{slug, n, cause, session, escalated_at, at}]
refs/notes/dashboard-flakes       the flake ledger: checks that passed only on a retry (refs/flakes/<scope>/<n>)
refs/notes/dashboard-impurities   the impurity ledger: checks found impure (refs/impure/<scope>/<n>, spec §20)
refs/notes/dashboard-explorations a spike round's verified answer + claim results, on each validated
                                 finish commit; {concluded: true, ...} on the `conclude:` commit
refs/notes/dashboard-speedups    on a test-speedup finish: the removed tests and rewired
                                 `cmd`s text (spec §15.1)
refs/notes/dashboard-conflicts   on a conflicted task's tip: the exact main commit an accept couldn't
                                 merge in, and the conflicted paths ({main, paths, at})
refs/notes/dashboard-agent-reports  task agent reports read at a task tip: [{verdict, text, at}]
refs/notes/dashboard-relaunches  `dashboard relaunch`es at a task tip: [{agent, reports, at}]
                                 (`reports`: the agent reports noted there at the time;
                                 `reset: true`: a `--reset`, the session's count restarts after it)
```

Every note write (`Git::notes_modify`) goes through a private
`refs/notes/scratch/<pid>-<n>` copy and a compare-and-swap of the notes ref,
retried if it moved: `git notes` itself has no old-value check, and
`dashboard relaunch` writes beside the player.

```
```

Git config: the director clone carries `dashboard.home`; the hub carries
`dashboard.director` (the director clone's path) and `dashboard.project`
(used to name tmux sessions and as the header's fallback title).

## File schemas

`roadmap.toml` (on `main`):
```toml
title = "…"
docs_paths = ["docs/adr"]        # paths the director may touch on main besides roadmap.toml itself

[[phases]]
id = "p1"
title = "…"
goal = "…"
hidden = false                   # true: left out of `status` (phase list + milestones),
                                 # still counted in Overall; shown as "(N hidden phases, M tasks)"

[limits]
max_review_rounds = 2            # default 2
max_contest_rounds = 2           # default 2: worker contests per task (spec §18)
max_relaunches = 2               # default 2: `dashboard relaunch`es of a task's worker, and
                                 # apart of its task agent, since that session's last progress

[limits.strikes]                 # one limit per rejection reason (spec §10), default 2:
                                 # history ownership pins suite plan post review accept
                                 # retire claims; evidence = 1 (a flaky-test-fix post whose
                                 # flake doesn't reproduce); budget = 2 (see [budget])

[player]
runs = "sequential"              # default. "sequential": one task in flight at a time (others
                                 # stay `pending` instead of `ready`); "parallel": any number
                                 # (spec §4); "queued": parallel, but test runs go through a
                                 # queue the player serves (see `dashboard test`)
agent_cmd = "claude --dangerously-skip-permissions"        # the worker; default
task_agent_cmd = "claude --dangerously-skip-permissions"   # the task agent (spec §17); default.
                                                           # "" turns task agents off
idle_after = 900                 # default: a session owing work whose tmux pane hasn't
                                 # changed for this many seconds is reported idle; 0: off
lock_files = ["Cargo.lock"]      # default none: lock files a task agent may commit
                                 # (a test-only dependency's check rewrites them);
                                 # read at main's tip, so an agent can't widen it
                                 # (refused under test_paths: the worker would be pinned out)
[player.lock_regen]              # default none: a merge of main into a task at accept
"Cargo.lock" = "cargo metadata --offline"  # that conflicts only in these paths is resolved by
                                 # the player: branch's version, then this command in the jail

[budget]                         # the suite time budget; seconds, 0 (default) = off
check_timeout = 600              # kill any one run of a suite check past this: it fails
                                 # (a `budget` strike, never retried)
suite_warn = 300                 # the checks' total wall time past this: a `suite_slow` event
suite_fail = 900                 # ...past this: a worker finish is rejected (`budget`), even
                                 # with every test passing. suite_warn must be < suite_fail

[sandbox]                        # the check jail (spec §20)
enabled = true                   # default; false runs checks as plain `sh -c`
network = false                  # default: loopback only
writable = ["~/.cargo"]          # extra writable host paths (absolute or ~/);
                                 # default none: only the worktree and a private /tmp
```

`prompts.kdl` (on `main`, required; spec §19): every text the player
writes into a prompt file or pastes into a session, as named templates.
`src/prompts.kdl` is the starter and documents each template's
placeholders.
```kdl
max-bytes 6144                   # cap on each delivered prompt's static text
                                 # (placeholders and what fills them not counted)
worker max-bytes=512 {         # optional: cap on each template in the section
    launch max-bytes=1536 #"""   # optional: this template's cap, instead of the section's
        You are the worker for task `{slug}` in project `{project}`. …
        """#
    finish-accepted "finish accepted — …"
    …
}
spike { … }
task-agent { … }
```
Validation (`prompts::Prompts::parse`, at `init` and on every `main`
import, `ownership` otherwise): every template present, none unknown, only
its own `{placeholders}` (other braces are literal), every composed
prompt within `max-bytes`, and every template within its own or its
section's `max-bytes=`. The player reads it from main's tip each time.

`tests.toml` (project suite, on `main` and every task branch):
```toml
test_paths = ["tests/"]          # the pinned test tree (spec §6.2, §8.3)
list_cmd = "cargo test -- --list" # test inventory, one per line; flaky-test-fix only

[[check]]
name = "unit"
cmd = "cargo test"
runs = 1                         # default; >1 for a threshold check
min_pass = 1                     # default; must be 1..=runs
retry_failed = 1                 # default 0; re-run a failing check up to N more times
retry_on = ["Text file busy"]    # optional regexes: retry only if a failing run's output matches
```

`tests.d/<slug>.toml` (a task's suite fragment, added by its post; spec §6.2):
only `[[check]]` entries, same shape. The suite is `tests.toml`'s checks plus
every `tests.d/*.toml`'s in path order (`config::suite_at`); check names must
be unique across all of them. Per-task files mean parallel posts never
conflict on the suite. A post's task check must be in its own fragment, not
`tests.toml`; fragments are pinned like `tests.toml`, and a worker adding
one is a `pins` strike.

`tasks/<slug>/task.toml`:
```toml
title = "…"
kind = "task"                    # or "spike" (spec §14): no depends_on/[check]/milestone,
                                 # or "flaky-test-fix" (spec §15): see below,
                                 # or "test-speedup" (spec §15.1): [[slow]] check = "<name>"
                                 # per slow suite check, no [check]
phase = "p1"
milestone = false
depends_on = ["other-slug"]
brief = "…"                      # the director's context for the task agent, pasted
                                 # into its prompt (spec §17)

[check]                          # the task's own failing-at-post check; added
name = "…"                       # to tests.d/<slug>.toml in the same post commit
cmd = "…"
```

A flaky-test-fix's extra `task.toml` fields (`fix_paths`, `[[replace]]`,
`[[evidence]]`; `tests.toml` needs `list_cmd`) are described in spec §15, a
spike's per-round files (`questions/`, `baseline/`, `claims/`, `findings/`) in
spec §14. A post whose flaky evidence doesn't reproduce is an `evidence`
rejection (limit 1) and the player retires the task itself.

Every struct uses `#[serde(default)]` field-by-field, and unknown fields are
ignored — no `deny_unknown_fields` anywhere.

## CLI

- `dashboard init [--name N]` — run inside the director clone (must have a
  local `main` with `roadmap.toml`/`tests.toml`). Commits the starter
  `prompts.kdl` on `main` if there's none (validates it otherwise), creates
  the home, `git clone --bare`s it into `hub.git`, and wires up the
  two-way config links.
- `dashboard` / `dashboard status [--no-color] [--width N]` — the default:
  renders the roadmap derived from the hub.
- `dashboard watch [-n SECS]` — `status` re-rendered in place every N
  seconds (default 2) until `q`/Esc/Ctrl-C. Flicker-free: alternate screen, and each
  frame overwrites the last in one write (cursor-home + per-line clear-to-EOL)
  rather than clearing first; re-renders immediately on terminal resize.
  A frame taller than the terminal scrolls (`ui::Viewport`, crossterm raw
  mode): ↑↓/j/k, PgUp/PgDn/space/b/Ctrl-U/Ctrl-D, g/G/Home/End, with a
  footer showing the visible range. The offset survives refreshes, and a
  view scrolled to the end stays pinned there. The mouse is never captured
  (text selection stays native); the wheel scrolls wherever the terminal
  turns it into arrow keys on the alternate screen (e.g. Alacritty, and
  tmux with `mouse off`), but not inside tmux with `mouse on`.
- `dashboard player [--once] [--interval SECS]` — the player loop (default
  interval 2s). `--once` runs a single tick, prints its report, and exits;
  this is what drives `scripts/e2e.sh` and what a supervisor (systemd timer,
  cron, a loop) should call if you don't want a long-running process.
- `dashboard relaunch <slug> [--agent] [--reset]` — manually restarts a dead worker
  session with the same clone and prompt file; `--agent` restarts the task
  agent instead, with a fresh prompt for the task's current step. There is
  no automatic relaunch (see Decisions below); a dead session just shows up
  in `status` and as a `session_died` / `agent_died` event. Capped at
  `[limits] max_relaunches` per session since its last progress: one more
  is refused (exit 1) and escalates the task instead. `status` shows
  `relaunched n/max` beside a session's liveness. `--reset` clears that
  escalation once the operator has fixed its cause, starts the session's
  count over and relaunches it; refused for any other escalation (see
  "Relaunch cap" below).
- `dashboard ask <slug> <question…>` — pastes the director's question into
  the task agent's session; the answer comes back as an `agent_report`
  event.
- `dashboard test [--no-wait] [--timeout SECS]` — `runs = "queued"` only, run
  from a worker's or task agent's worktree (branch `task/<slug>` or
  `agent/<slug>`): snapshots the working tree (a deterministic commit in a
  private index, ref `refs/dashboard/test/<id>` in the workspace), files
  `queue/<id>.req` in the home and waits for `queue/<id>.result` (`PASS`/`FAIL`
  line, then every check's output), which it prints; exit 1 on a failing
  suite, 2 on timeout. The player serves one request per tick
  (`Player::serve_queue`: fetch the ref into the hub, `run_suite`), so runs
  never overlap; it is no strike, event or hub record. The agent reads the
  output itself; nothing is pasted into its session. The player adds
  `worker.queued` / `task-agent.queued` (prompts.kdl) to worker and task-agent
  prompts when queued, and sets
  `dashboard.home` in the worker workspace so `test` finds the home.
- `dashboard arch check [--root DIR]` — reads `architecture.toml` (modules, APIs, budgets; `config::ArchConfig`)
  and fails on any budget overrun, a file in two modules, or a module whose `test_cmd` fails with only its own
  files, its APIs' files and `shared` present. Prints headroom. Run it as an ordinary suite check; the player is
  not involved in the check itself. The player does: `architecture.toml` is a path the director may commit on
  `main` (validated on import: `ownership` if it doesn't parse), and the manifest plus every API path in it is
  pinned against workers (`records::pinned_files`, a `pins` strike; not for spikes). A task agent may write API
  paths (read at the tip) but never the manifest (`agent_paths_ok`). Raise validation is a later step.
- `dashboard arch gen [--root DIR] [--check]` — regenerates each API's bindings from its one `*.api.kdl`
  (written to `gen/<api>.rs` beside it, so under the API's pinned paths). `--check` writes nothing and exits 1 on a
  stale binding; `arch check` runs the same drift check, plus the IDL's constructor count (`max_constructors`) and
  T slots, i.e. argument slots across constructors (`max_types`), and ref validity (known, linked, acyclic API;
  declared type). IDL: `type "T" repr="string|bytes|i64|f64|bool" [display=#true]` or `ref="api.Type"`, and
  `constructor "C" "T"...`. Every generated T is opaque (private field, no trait impls, no accessors; `display`
  opts into Display). The JSON wire code (`to_json`/`from_json`, needs `serde_json`; bytes as hex) is emitted only
  for a pair whose two modules differ in `lang`, and such an API may ref only other cross-language APIs. Rust
  (`gen/<api>.rs`) and TypeScript (`gen/<api>.ts`: ES2022 `#private` class fields, a `kind`-tagged union, i64 as
  `bigint`, refs as extensionless `./<api>` imports) generators exist; any other language is reported as "no <lang>
  generator yet". Kotlin (`gen/<Api>.kt`): package `api.<name>`, a `sealed interface`, plain classes
  (not `data`/`value`, so no value equality) with a private `v`, and a file-private JSON codec (no dependency).
  Each generator's output was compiled and round-tripped against the others' JSON by hand (cargo, deno, kotlinc).
  The wire code's `to_value`/`toValue` read a `T`'s inner value, so it exists only on cross-language APIs; the
  adapters (step 4) should ban calling it outside the generated file.
- `dashboard arch check` also lints each module's own (non-API) files (`src/lint.rs`): in a module that consumes an
  API, a match over its sum type with a wildcard or catch-all binding arm (Rust `_`/lone binding, TypeScript
  `default`, Kotlin `else`) fails, and so does any `.to_value()` / `.toValue()` call (the wire helper that reads a
  T). A match "is over an API" when an arm/case/branch names one of its constructors (`Ab::X`, `case "X"`,
  `Ab.X`), so a glob-imported `use Ab::*` escapes the Rust check: a heuristic, not a type check. A module's
  optional `lint_cmd` adds the project's own adapter (e.g. clippy): it runs in the checkout root with
  `DASHBOARD_MODULE` set and prints a JSON array of `{file, line, message}` findings; any finding, or a non-JSON
  failure, fails the check.
- `dashboard wait [--timeout SECS] [--all]` — the director's blocking
  listener (see Decisions). Prints every event past the director's cursor,
  advances it, and exits; `--timeout` exits 2 after N idle seconds; `--all`
  dumps the whole history without touching the cursor.

`--home`, `--no-color`, and `--width` are global flags valid on every
subcommand.

## Build, test, install

- `cargo build` / `cargo build --release`
- `cargo test` — unit tests inline in each module plus real-git integration
  tests in `tests/*.rs` (throwaway repos in temp dirs via `tests/common/mod.rs`)
- `scripts/e2e.sh [path-to-binary]` — an end-to-end scenario against the
  real binary and real `tmux`: a throwaway director repo + hub, a small
  fake-agent shell script standing in for a real Claude Code worker, and
  the full plan/post/launch/finish/send-back/accept/block/escalate/retire/
  reject loop, asserted via `dashboard status --no-color` and raw hub ref
  checks. Run it after any change that touches `src/player.rs`,
  `src/state.rs`, or the on-disk/ref layout — it's the only test that
  exercises real `tmux` sessions and a real worker-style fetch/merge/
  commit cycle.
- **Installing**: `cargo install --path .` replaces `~/.cargo/bin/dashboard`,
  the binary every director and `~/.claude/skills/dashboard` invoke. When a
  change alters the CLI surface (subcommands, flags, output a director
  parses), update that skill to match in the same pass.

## Decisions on the spec's parking lot (§13)

Resolved by the implementation plan (`spec.md` §13 has one-line pointers
back here):

- **CLI shape**: `init` / `status` (default) / `player [--once]` /
  `relaunch <slug>` / `wait` — see CLI above.
- **Hub/clone locations**: the hub home (hub.git, work/, events.jsonl, …) is
  XDG (`paths::Home`); worker clones are *not* — they nest inside the
  director's own working tree (`paths::workers_root`) so they inherit the
  operator's own workspace trust instead of needing the player to grant it
  to a directory it creates unattended. See On-disk layout above.
  `dashboard init` is how the hub gets created (`src/init.rs`).
- **`dashboard wait`**: the director's listener, so the director's own
  session never polls. It blocks on `events.jsonl` past `director.cursor`
  and returns human-readable lines plus a per-kind hint (e.g. "fetch hub
  and review it"). `dashboard player` still polls its clones internally —
  it's a process, so that's fine; only the director-facing side had to stop.
- **No auto-relaunch**: a dead worker session is reported (`status`, a
  `session_died` event) but never restarted automatically. `dashboard
  relaunch <slug>` is the manual trigger, reusing the existing clone and
  pins. Death is reported once per launch, not on an observed alive→dead
  edge: a launched session (a worker with pins while in flight, a task
  agent until its task is finished) that's dead under a launch id not yet
  told is reported, so one that crashed before any tick saw it alive still
  is. Every launch (`Player::launch_session`, including `relaunch` in its
  own process) writes a fresh id to `launches/<key>`, re-arming it.
  Sessions are launched with `remain-on-exit on`, so a crashed agent's
  pane lingers, dead: "alive" means the pane's command is running
  (`#{pane_dead}` is 0, `state::tmux_alive`, also what `status` asks),
  not that the session exists. Each tick, a dead session that lingers is
  reaped (`Sessions::reap`: its history, `#{pane_dead_status}` /
  `#{pane_dead_signal}`, then `kill-session`), watched or not, and the
  death event carries the exit status and its screen tail. A launch kills
  a dead session still holding its name first, so `relaunch` works
  before the player reaps it. Not a tee of the agent's output to a file:
  piping a TUI's output breaks it.
- **Idle sessions**: a live Claude Code session that ends its turn
  without the commit or report it owes (or on an API error / usage
  limit) is otherwise invisible. The launch-installed hooks (see On-disk
  layout) leave `dashboard-stopped` while the agent waits for input; the
  player removes it (and `dashboard-reported`) before every launch and
  every relay (`Player::relay`, which every paste goes through, `ask`
  included), so a stop is always newer than the last thing sent. Each
  tick, a live session with a stop is reported once per stop
  (`session_idle` / `agent_idle`, `Player::report_idle`, `idle_told`) if
  it `owes_work`: a worker while pinned and `posted`/`in_progress`/
  `in_revision` with no held finish (a finish, block: or contest: moves
  it past these); a task agent while its task is `staged` (the post),
  `tested` (a review) or `contested` (a reply) and no report was collected
  since its last launch/relay. Anything else is waiting on someone else.
  `owes_work` is the reusable "supposed to be working" rule. Another
  `agent_cmd` just ignores the settings file.
- **Screen tails**: every idle and death event ends with the session's
  last screen (`player::screen_tail`: the last 25 non-blank lines,
  trimmed and indented under "last screen:", cut from the front to 2 KiB),
  or "no screen captured", so a director that can't `tmux attach` sees
  why it stopped. Idle: `Sessions::capture`; death: `Sessions::reap`.
- **Diagnose before relaunching**: the `wait` hints for these four
  events (`Event::hint`) send the director to the screen tail (and exit
  status) first, never to `tmux attach` (an LLM director can't; the
  operator can). By cause: a usage/rate limit or API error waits for the
  reset; a task agent's question gets `dashboard ask`; a permission/trust
  dialog, auth failure, bad `agent_cmd` or crash at startup goes to the
  operator (a relaunch would die the same way); only an unexplained hang
  (kill the session first) or a one-off mid-work crash is relaunched.
  Retire is a judgement for a task that keeps failing, not the default.
  A worker's question has no director route mid-work: a send-back needs
  `tested`, a contest reply `contested`, and every relay to a worker is
  triggered by its own commit. So it goes to its task agent (`dashboard
  ask`) when it shows the brief or tests are wrong, and to the operator,
  who can answer in the session (ideally: turn it into a `contest:` or
  `block:`, which the hub can then answer).
- **Relaunch cap**: nothing else stops a director relaunching a failing
  agent forever, so `[limits] max_relaunches` (default 2) caps it, with
  the worker and the task agent counted apart (different sessions,
  different failure modes; the agent's relaunches say nothing about the
  worker's). Each `relaunch` notes `{agent, reports, at}` on the task tip
  (`refs/notes/dashboard-relaunches`, `records::record_relaunch`, after
  the launch succeeds), so the count is hub state, survives a player
  restart and is derived (`TaskState::relaunches`, `AgentState::
  relaunches`). Only relaunches since the session's last progress count:
  any commit on task/<slug> moves the tip, so a note on an older tip no
  longer counts (for the worker, its finish, block or contest; also a
  send-back or sync, which hand it a new round); a task agent's note counts
  only while the reports on the tip are as many as when it was taken, so a
  collected report resets it too. Past the cap, `check_relaunch_limit`
  refuses and records an ordinary escalation (`refs/dashboard/escalated/
  <slug>` at the tip, cause `relaunch limit (N) reached: the <worker|task
  agent> ...`): `state` derives `escalated`, `status` shows it and the
  player emits `escalated` on its next tick. An escalated task's task
  agent past its cap is just refused. The session-health hints mention
  the cap.
- **Resetting a relaunch-limit escalation**: the cap mostly catches the
  environment (a bad `agent_cmd`, an expired login, a trust dialog, a
  usage limit), which the operator can fix, so this one escalation, unlike
  review rounds, strikes and `block:`, isn't retire-only. The record says
  whose cap it was (`EscalationRecord::relaunch_limit`; read off the cause
  text for an older record). `dashboard relaunch <slug> [--agent] --reset`
  (`Player::reset_relaunch`) refuses while that session is alive, unless
  the task is escalated, and unless its *current* cause
  (`state::escalation_cause`, where strikes and `block:` outrank a record)
  is that record and for that session. Then `records::clear_escalation`
  moves `refs/dashboard/escalated/<slug>` to `refs/dashboard/
  escalation-cleared/<slug>/<n>` in one `update-ref --stdin` transaction
  with old values (`Git::move_ref_cas`: an escalation the player records
  meanwhile is never lost) and notes a `ClearedEscalation` on its commit
  (its own list note, since the same tip may escalate again and overwrite
  its escalations note); a `{reset: true}` relaunch record restarts that
  session's count (`relaunch_count` counts only after a session's last
  reset); then the ordinary relaunch runs, so the count reads 1 and the cap
  applies again. On its next tick the player sees the task leave
  `escalated` (its `last` status) for anything but a terminal status and
  emits an informational `resumed` (the cleared cause, the new status),
  instead of the new status's own event; persisted like every status, so a
  player restarted in between still tells it once. The `escalated` hint
  for a relaunch-limit cause names the exact `--reset` command.
- **Stale screens** (any agent): each tick, a live session that
  `owes_work` has its pane captured (`Sessions::capture`, `tmux
  capture-pane -p -t =<name>:`) and hashed (`pane_hash`, trailing
  whitespace ignored). Unchanged for `[player] idle_after` seconds
  (default 900, 0 off), it's the same `session_idle` / `agent_idle`
  ("no new output for N min": hung, or on a dialog), once per unchanged
  screen (`Player::watch_pane`, `panes` in player-state.json, keyed by
  launch id). A pane change, a relay or a launch re-arms it, as does the
  session no longer owing work. A stop marker covers its whole wait (no
  stale report while it's there), and a stop on a screen already
  reported stale isn't reported again. A capture failure is no data
  (death is `report_death`'s). Limits: anything that moves counts as
  output, so a working Claude Code (spinner, timer) is never stale, a
  turn hung with the spinner still going isn't caught, and a TUI that
  redraws a clock while idle is never stale either. Tests pin the clock
  with `Player::set_clock`.
- **`agent_cmd`**: `[player] agent_cmd` in `roadmap.toml`, default `claude
  --dangerously-skip-permissions`. Launched as `tmux new-session -d -s
  dash-<project>-<slug> -c <clone> sh -c '<agent_cmd> "$(cat
  <prompt-file>)"' \; set-option -w remain-on-exit on`; relays go
  through `tmux load-buffer`/`paste-buffer`/`send-keys` into that same
  session.

- **flaky-test-fix** (spec §15): `src/flaky.rs` plus `flaky_scope_violations`
  / `validate_flaky_finish` in `player.rs`. Evidence runs in full, never
  early-stopping, so its cached result and k/N are exact.
- **Suite time budget**: `[budget]` in `roadmap.toml` (not `tests.toml`: task
  agents edit the suite), read from main's tip each tick in `check_sandbox`
  and applied by `Runner::set_budget`. `run_suite` runs each check under
  `check_timeout` (not a task's own `[check]`, evidence or spike claims,
  which keep theirs); the kill is `run_command`'s existing deadline, and
  `CheckResult::timed_out` stops further runs and retries. Every result
  records its wall time (`millis`, all runs and retries) and the timeout it
  ran under (`timeout`, part of the cache key), so a suite's time
  (`SuiteResult::millis`) is a pure function of the hub's notes: a cache
  hit reports the original timing, not zero. A failing suite with a timed-out
  check is a `budget` strike instead of `suite`; a *passing* one over
  `suite_fail` is a `budget` rejection at a worker finish only (`over_budget`).
  The director's main import and accepts don't enforce `suite_fail` (main's
  own policy lives there: rejecting would lock out raising it), and a
  timeout still fails them as before. `suite_warn` puts a `suite_slow` event
  (informational, no hint) in the report: per worker finish, and once per
  main tip from `verify_main`. Tests: `tests/budget.rs`.
- **An over-budget `main`** (spec §20) and **test-speedup** (spec §15.1):
  `verify_main` keeps `main_over` (`checks::Overrun`: killed checks, whether
  the total is over, every check's `millis`) beside `main_impure`, reports it
  once per tip (`main_over_budget` event, `state::HubState::main_over_budget`
  for `status`). `bad_on_main` widens `held_by_main` to checks killed on both;
  `held_over_total` holds a passing finish over `suite_fail` only because main
  is (per-check, `checks::within_tolerance`); `held_reason` is both, used by
  `release_held` and the finish. `test-speedup` (`TaskKind::TestSpeedup`,
  `[[slow]]`): `slow_miss` is its `evidence_miss` (an `evidence` rejection
  and auto-retire, also at launch), `records::pinned_files` unpins the test
  side, `test_speedup_scope` + `speedup::scope_problems` fence the worker
  (`pins`), and `speedup::ratchet_problems` judges a finish still over budget.
  `test_speedup_notes` lists removed tests (`flaky::removed`) and changed
  `cmd`s: the `speedup_finish` event, `records::record_speedup`, and the
  review prompt's `{notes}`. fsm.rs needed no change: it is kind-agnostic.
  Tests: `tests/speedup.rs`.
- **Check retries** (spec §16): `Runner::execute` loops attempts; never
  applied to `run_task_check` (`Check::without_retries`) or evidence. Every
  retry is in the cached result; a pass after one goes in the flake ledger
  (`records::record_flake`, per task or `_main`). The worker is never told.
- **Spikes** (spec §14): `src/spike.rs`. Baseline claims run against the
  base commit with `tasks/<slug>/` overlaid (`spike::overlay_commit`, a
  deterministic commit so the check cache hits); claim runs go through
  `Runner::run_claim` (timeout). A spike's answer is read from the
  `dashboard-explorations` note, never from the free-form findings. No CLI
  change: a spike is just different director commit shapes. The "operator
  only" rule isn't checked by the player.
- **Staged tasks** (spec §17): `Transition::Stage`, `Player::try_stage`;
  `Status::Staged` counts as pending in Overall. `state::staged_at` walks
  first-parent history from the tip past `merge_unblock`/`replan` commits to
  the last marker, because the player's merge of `main` becomes the tip and
  would shadow the note there; a replan keeps a task staged, and
  `unblock_merges` keeps feeding a staged branch `main`. A managed `ready`
  task emits a `ready` event.
- **Parallel tasks** (spec §4, §11): `Player::land` (for an `accept:`)
  merges a moved `main` into the branch (a player `sync` commit), then the
  branch into `main` once the suite passes on the result. A suite failing
  because of that merge makes the tip a `sync_revision` (`merge_failed`
  event). A textual conflict imports the marker with a `dashboard-conflicts`
  note; conflict events come from the tick report, not a status change, so a
  repeat conflict is still reported. The director's only merge is
  `Player::try_resolve`: it must merge exactly the recorded `main` commit,
  and may differ from `git::merge_tree_conflicts`'s result only on the
  recorded paths (`ownership` otherwise); any other director merge is
  `history`. The walk over a task branch's new commits skips anything
  already on `main`. `[player] lock_regen` lets the player resolve a conflict
  confined to those paths (`Runner::regen_locks`: merge in the scratch
  worktree, `checkout --ours`, the command in the jail, stage); the suite
  judges the result. `lock_files` is the task agent's write allowance
  (`agent_paths_ok`); `config::check_lock_files` refuses one under
  `test_paths`, at `init` and on every `main` import.
- **Task agents** (spec §17): `Sessions` key `state::agent_key(slug)`
  (`<slug>_agent`; `_` never occurs in a slug); commits are `Actor::TaskAgent`
  and go through the director's post/send-back validation plus the path
  allowance (`agent_paths_ok`). A hub tip that moved under an unimported
  agent commit gets a rebase request, not a strike. Tests turn task agents
  off in their fixture roadmap (`tests/common`'s `NO_TASK_AGENTS`) unless
  they're about them (`tests/task_agent.rs`).
- **Contests** (spec §18): `Transition::Contest` / `ContestReply`;
  `TaskState::contest_round` counts the `contests/c*.md` at the tip; a
  contest past `max_contest_rounds` escalates without being imported.
- **Check isolation** (spec §20): `Runner::set_sandbox` jails every
  `run_command` under `bwrap` (`Sandbox::command`); the policy comes from
  main's `roadmap.toml` each tick (`Player::check_sandbox`), never from the
  suite. `Sandbox::preflight` runs in `init` and in the player at start and
  on every policy change; a failure stops the tick. `Runner::attempt` runs
  `git status` after each run: leftovers are `CheckResult::leaked`, never
  retried, and `impurity()` keeps it from counting as a required failure.
  `CheckResult::isolation` (the policy fingerprint) is part of the cache
  key. Tests need a working `bwrap`; `tests/sandbox.rs` covers the jail, and
  `tests/flaky.rs`'s fixture adds a `writable` dir for its counter files.
- **An impure `main`** (spec §20): `Player::verify_main` runs main's suite
  every tick (before and after `import_main`) and at `dashboard player`
  startup, which prints the result. `held_by_main` decides a failure is
  main's; a worker finish is then held (`refs/dashboard/held`,
  `worker.held`, a `held` event), an accept in memory (`held_accepts`, keyed
  by main tip). `release_held` merges main into a held branch once clean (a
  `sync_revision`, `worker.released`); a conflict there just un-holds it.
  Tests: `tests/main_impure.rs`.
- **Impurity beyond leaks** (spec §20): `Sandbox::walls_hit` sets
  `CheckResult::blocked`, impure like `leaked`; the fingerprint's `v2` makes
  results from the leak-only detector re-run. Every impure result goes in
  the impurity ledger (`records::record_impurity`, via `note_ledgers` and at
  post/launch). `hold_launch` reports a held launch once (`launch_told`).
