# dashboard

A standalone console dashboard for a project's `docs/roadmap/tasks.yaml`
roadmap file — a progress bar, per-phase breakdown, milestones, an
"actionable now" list computed from the dependency graph, in-revision and
blocked sections. This repo isn't scoped to one consumer: it lives outside
any single project and is installed as a `dashboard` binary on `PATH`
(`cargo install --path .`), then invoked by other repos and by the
`~/.claude/skills/dashboard` skill, which drives an entire subagent-loop
workflow through this exact CLI (`dashboard --no-color`, `--file`, default
resolution of `./docs/roadmap/tasks.yaml` then `./tasks.yaml`).

## Current state

Expansion phase. The project started as a byte-for-byte-duplicated Python
script (`dashboard.py`, pulled out of `api-proxy` and `personal-assistant`),
was migrated to a single hand-rolled-parser Rust file, and has since picked
up real dependencies (`serde`/`serde_yaml` for parsing, `clap` for the CLI)
in place of that hand-rolled code. Treat it as ordinary Rust that can grow
more crates and more surface as needed — do not reintroduce a zero-deps
constraint. There's no roadmap of its own (no `docs/roadmap/tasks.yaml`
here) and no version/compat story with the repos that consume it: this repo
is the source of truth for the rendering format, and a change here is
expected to just propagate to every consumer via `cargo install --path .`,
not to be gated on their needs.

## Repo layout

```
src/main.rs   everything: tasks.yaml schema (serde), rendering, CLI (clap)
Cargo.toml    serde + serde_yaml (parsing), clap (CLI)
```

## tasks.yaml schema

`Roadmap { title, phases: [Phase], tasks: [Task] }`
- `Phase { id, title?, goal? }`
- `Task { id, title, status = "pending", phase, depends_on: [id], milestone: bool, reviews: [String], blocked_reason? }`

`status` is one of `pending`, `in_progress`, `in_revision`, `built`,
`tested`, `blocked`. "Actionable now" = `status == pending` and every
`depends_on` id resolves to a task with `status == tested`. A `reviews`
entry is free-form text matched against `^r(\d+)\s+(sent-back|accepted|escalated)`
(see `review_verdict`) — `REVIEW_CAP` sent-back rounds on a task is flagged
as the final round before a human should get involved (`cap_hit`).

Unknown fields in `tasks.yaml` are ignored by serde (no `deny_unknown_fields`)
so consumer repos can carry extra fields without breaking this tool.

## Build, install, run

- Build: `cargo build --release`
- Install/refresh the PATH binary every consumer repo relies on:
  `cargo install --path .`
- Run against the current directory's roadmap: `dashboard` (or
  `dashboard --no-color --file <path>` — see `dashboard --help`)
- After changing the schema or output format, rebuild, reinstall, and
  smoke-test against a real `tasks.yaml` from a consumer repo (e.g.
  `api-proxy/docs/roadmap/tasks.yaml` or `personal-assistant/docs/roadmap/tasks.yaml`)
  before calling it done — there's no test suite yet.
