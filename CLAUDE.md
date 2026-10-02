# dashboard

The enforcement point for a subagent-per-task roadmap loop (see `spec.md`,
the design source of truth). `dashboard` is no longer a stateless renderer
of a hand-edited `tasks.yaml`: it's the **player** — a bare git hub plus a
process that imports and validates every commit a director or subagent
clone produces, runs every check itself, launches subagents in `tmux`,
counts strikes, merges accepted work, retires dead ends, and renders a
status that's a pure function of the hub. Nobody writes a status; git (plus
whether a subagent's session is alive) is the only source of truth. This is
greenfield: there is no compatibility with the old `tasks.yaml` format, and
none is planned.

Installed as a `dashboard` binary on `PATH` (`cargo install --path .`) and
invoked by other repos' directors and by `~/.claude/skills/dashboard` (the
director playbook for this CLI). See "Installing" below.

## Repo layout

```
src/main.rs     CLI (clap): init / status (default) / watch / player / relaunch / wait
src/lib.rs      the library crate every module below hangs off of
src/config.rs   roadmap.toml / tests.toml / task.toml schemas (serde) + defaults
src/git.rs      thin sync wrapper over the `git` CLI (rev-parse, diff, notes, merge-tree, …)
src/paths.rs    XDG home layout and `--home` / `dashboard.home` resolution
src/init.rs     `dashboard init`: creates the hub and wires up the director <-> hub links
src/checks.rs   check runner (a scratch worktree + `sh -c`) and the notes-based result cache
src/records.rs  provenance/enforcement records: import notes, rejections, pins, retirements, accepted-merge lookup,
                spike base commits and exploration notes
src/flaky.rs    flaky-test-fix (spec §15): inventory comparison, evidence verdict text, subagent prompt
src/spike.rs    spikes (spec §14): reading a round, running claims against the base-with-overlay, evaluating a finish
src/state.rs    derives every task's status (spec §12) purely from the hub + session liveness
src/player.rs   the player: fetch, validate, import, launch/relay, merge — spec §5/§7/§9/§11
src/events.rs   the director-relevant event log, cursor, and heartbeat behind `dashboard wait`
src/render.rs   renders `state::HubState` + recent events to the status text
src/ui.rs       styling/width helpers (color codes, bars, truncation, terminal width)
```

`Cargo.toml`: `serde`/`serde_json`/`toml` for parsing, `clap` for the CLI;
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
player-state.json   last-seen (status, session-alive) per slug, so a
                     restarted player doesn't re-emit old events
player.heartbeat     unix time of the last tick, so `wait` can tell the
                     player looks dead (missing or >30s old)
```

Subagent clones live *outside* this home, nested inside the **director's own
working tree** instead (`paths::subagents_root`/`subagent_clone`):

```
<director-clone>/.dashboard/subagents/<slug>/   one clone per launched task
```

A subagent clone is a fresh directory the player creates with nobody present
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
                            roadmap.toml / docs_paths commits (spec §5)
refs/heads/task/<slug>     one per task, never deleted even once accepted
                            (spec §8.1) — moved, not deleted, on retire
refs/retired/<slug>        a retired task's branch, moved here by the player
refs/explored/<slug>       a concluded spike's branch, moved here on `conclude:` (spec §14)
refs/incoming/director/*   the director clone's main/task/* fetched in,
                            pre-validation (src/player.rs's `fetch`)
refs/incoming/subagent/<slug>  a subagent clone's task/<slug>, pre-validation
refs/dashboard/pins/<slug> the commit pins were computed from (spec §8.3)
refs/dashboard/base/<slug> a spike's base commit (the tip at launch); never moved
refs/dashboard/escalated/<slug>  set when a send-back would exceed
                            max_review_rounds (the other escalation causes —
                            a strike limit, a block: marker — are read
                            straight off strikes/notes, no separate ref)
refs/rejected/<scope>/<n>  every rejected commit, kept as evidence
                            (scope is a task slug, or `_main`)
```

Notes (JSON payloads, `git notes --ref <ref>`):

```
refs/notes/dashboard             check-result cache: {"checks": {name: CheckResult}}
refs/notes/dashboard-imports     provenance per imported commit (actor, transition, slug, at)
refs/notes/dashboard-rejections  reason/actor/message/at per refs/rejected/* commit
refs/notes/dashboard-escalations cause/at per refs/dashboard/escalated/<slug>
refs/notes/dashboard-flakes       the flake ledger: checks that passed only on a retry (refs/flakes/<scope>/<n>)
refs/notes/dashboard-explorations a spike round's verified answer + claim results, on each validated
                                 finish commit; {concluded: true, ...} on the `conclude:` commit
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

[limits]
max_review_rounds = 2            # default 2

[limits.strikes]                 # each defaults to 2 (spec §10)
history = 2
ownership = 2
pins = 2
suite = 2
plan = 2
post = 2
review = 2
accept = 2
retire = 2
claims = 2                       # a spike's finish whose claims/findings don't validate
evidence = 1                     # a flaky-test-fix post whose flake doesn't reproduce

[player]
agent_cmd = "claude --dangerously-skip-permissions"   # default
```

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

`tasks/<slug>/task.toml`:
```toml
title = "…"
kind = "task"                    # or "spike" (spec §14): no depends_on/[check]/milestone,
                                 # or "flaky-test-fix" (spec §15): see below
phase = "p1"
milestone = false
depends_on = ["other-slug"]

[check]                          # the task's own failing-at-post check; added
name = "…"                       # to tests.toml in the same post commit
cmd = "…"
```

A flaky-test-fix's extra `task.toml` fields (spec §15; `[check]` is the
stability proof and is *not* added to `tests.toml`, which needs `list_cmd`):
```toml
fix_paths = ["tests/a.rs"]       # plain files unpinned for this task
[[replace]]
old = "flaky"                    # test names (last `::` segment is enough)
new = "flaky_stable"
[[evidence]]                     # run on main at post and launch; must flake
name = "…"
cmd = "…"
runs = 100
min_pass = 1                     # 1 <= min_pass <= max_pass < runs
max_pass = 99
```
A post whose evidence doesn't reproduce is an `evidence` rejection (limit 1),
and the player retires the task itself: a marker commit with the post-mortem
`evidence did not reproduce (k/N)` goes under `refs/retired/<slug>`.

A spike's per-round files (spec §14), all under `tasks/<slug>/`:
```toml
# questions/<n>.toml (director, pinned)   baseline/<n>.toml (director, pinned)
question = "…"                            [[claim]]
why = "…"                                 name = "…"
answers = ["yes", "no"]                   cmd = "…"   # + runs/min_pass/timeout
refines = 1                               # on current code; must pass on main

# claims/<n>.toml (subagent)              findings/<n>.md (subagent, free-form,
answer = "yes"                            #   must exist and be non-empty)
[[claim]]
name = "…"
kind = "spike"                   # or "baseline"
supports = "yes"                 # spike claims: the answer this backs
cmd = "…"                        # last stdout line may be `METRICS {json}`
```

Every struct uses `#[serde(default)]` field-by-field, and unknown fields are
ignored — no `deny_unknown_fields` anywhere.

## CLI

- `dashboard init [--name N]` — run inside the director clone (must have a
  local `main` with `roadmap.toml`/`tests.toml`). Creates the home, `git
  clone --bare`s it into `hub.git`, and wires up the two-way config links.
- `dashboard` / `dashboard status [--no-color] [--width N]` — the default:
  renders the roadmap derived from the hub.
- `dashboard watch [-n SECS]` — `status` re-rendered in place every N
  seconds (default 2) until Ctrl-C. Flicker-free: alternate screen, and each
  frame overwrites the last in one write (cursor-home + per-line clear-to-EOL)
  rather than clearing first; redraws immediately on terminal resize.
- `dashboard player [--once] [--interval SECS]` — the player loop (default
  interval 2s). `--once` runs a single tick, prints its report, and exits;
  this is what drives `scripts/e2e.sh` and what a supervisor (systemd timer,
  cron, a loop) should call if you don't want a long-running process.
- `dashboard relaunch <slug>` — manually restarts a dead subagent session
  with the same clone and prompt file. There is no automatic relaunch (see
  Decisions below); a dead session just shows up in `status` and as a
  `session_died` event.
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
  fake-agent shell script standing in for a real Claude Code subagent, and
  the full plan/post/launch/finish/send-back/accept/block/escalate/retire/
  reject loop, asserted via `dashboard status --no-color` and raw hub ref
  checks. Run it after any change that touches `src/player.rs`,
  `src/state.rs`, or the on-disk/ref layout — it's the only test that
  exercises real `tmux` sessions and a real subagent-style fetch/merge/
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
  XDG (`paths::Home`); subagent clones are *not* — they nest inside the
  director's own working tree (`paths::subagents_root`) so they inherit the
  operator's own workspace trust instead of needing the player to grant it
  to a directory it creates unattended. See On-disk layout above.
  `dashboard init` is how the hub gets created (`src/init.rs`).
- **`dashboard wait`**: the director's listener, so the director's own
  session never polls. It blocks on `events.jsonl` past `director.cursor`
  and returns human-readable lines plus a per-kind hint (e.g. "fetch hub
  and review it"). `dashboard player` still polls its clones internally —
  it's a process, so that's fine; only the director-facing side had to stop.
- **No auto-relaunch**: a dead subagent session is reported (`status`, a
  `session_died` event) but never restarted automatically. `dashboard
  relaunch <slug>` is the manual trigger, reusing the existing clone and
  pins.
- **`agent_cmd`**: `[player] agent_cmd` in `roadmap.toml`, default `claude
  --dangerously-skip-permissions`. Launched as `tmux new-session -d -s
  dash-<project>-<slug> -c <clone> sh -c '<agent_cmd> "$(cat
  <prompt-file>)"'`; relays go through `tmux load-buffer`/`paste-buffer`/
  `send-keys` into that same session.

- **flaky-test-fix** (spec §15): pins exclude `fix_paths`; at finish, on top
  of the suite, the task check must pass and the `list_cmd` inventory at the
  pins commit minus the `old` names must still be in the branch's, with every
  `new` present (a `suite` strike otherwise). Evidence runs in full, never
  early-stopping, so its cached result and k/N are exact.
- **Check retries**: `retry_failed = N` re-runs a failing check (all its
  `runs`) up to N more times; any passing attempt passes it, a check failing
  every attempt is still one `suite` strike. `retry_on` limits retries to
  failures whose output matches a regex. Never applied to a task's own
  `[check]` (`Runner::run_task_check`) or evidence. Every retry is in the
  cached result; a pass after a retry goes in the flake ledger
  (`records::record_flake`, per task or `_main`), shown in `status` as
  `flaky: N`, a "Flaky checks" list and `passed on retry k/N`. The subagent
  is never told about retries.
- **Spikes** (spec §14): `kind = "spike"`, operator-initiated by process
  rule (the player doesn't check it), nothing may depend on one, never
  merges. The player runs the claims itself: baseline claims against the
  base commit with `tasks/<slug>/` overlaid (`spike::overlay_commit`, a
  deterministic commit so the check cache hits), spike claims on the branch
  and again on the overlay where they must fail. Claim runs have a timeout
  (`Runner::run_claim`). A concluded spike's branch moves to
  `refs/explored/<slug>`; its answer is read from the
  `dashboard-explorations` note, never from the free-form findings. No
  CLI change: a spike is just different director commit shapes
  (`kind = "spike"`, `questions/`, `conclude:`).

Still undecided (spec §13): timeout/crash handling beyond manual relaunch,
scope enforcement beyond pins, suite-runtime cost at scale, and parallel
tasks.
