# dashboard

The player of a worker-per-task roadmap loop: a bare git hub plus a process that validates every
commit from the director, task agents and workers, runs the checks, launches sessions in `tmux`,
merges or retires work, and renders a status derived purely from the hub. Greenfield: no compat.

- Design source of truth: `spec.md` (loop, hub refs, notes, limits) and `architect-v3.md`
  (architecture, IDL, Lean DSL, proofs). One module per concern under `src/` (see `src/lib.rs`).
- Build: `cargo build`. Test: `cargo test -j 1`, one target at a time
  (`cargo test -j 1 --test <t>`); integration tests in `tests/` build real throwaway git repos
  and need `bwrap`, `tmux`, `lake` (~/.elan), `deno`.
- After touching `src/player.rs`, `src/state.rs` or the ref/on-disk layout, run `scripts/e2e.sh`.
- Install: `cargo install --path .` replaces the `dashboard` that directors and
  `~/.claude/skills/dashboard` call; update that skill whenever the CLI surface changes.
- Config structs use `#[serde(default)]` per field; never `deny_unknown_fields`.
- Commits: the `commit` skill (2 lines, 60 chars each).
