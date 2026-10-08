# Feature request: let task agents commit lock-file changes (future-term)

Written 2026-10-08 from the first posts of the future-term roadmap (Rust
workspace, five parallel tasks, task agents on claude, workers on grok).
The player source was read for this (`Player::agent_paths_ok`,
`checks.rs` impurity detection).

## The problem

A task agent can't post a test that needs a new crate. Two rules each
reject one of the two ways it can try:

1. **Without the lock entry, the check is impure.** The agent adds a
   test-only dependency to `tests/Cargo.toml` (under `test_paths`, so it is
   the agent's file). Running the check (`cargo test --offline …`) resolves
   the new crate from the local cache and rewrites `Cargo.lock`. The worktree
   is left dirty (`M Cargo.lock`), so the post is rejected: `post: check
   "html-sanitizer" is impure: it left files in its worktree`.
2. **With the lock entry, the commit breaks ownership.** The agent commits the
   regenerated `Cargo.lock` so the check leaves the worktree clean. But
   `agent_paths_ok` only allows `tests.toml`, `tests.d/`, `test_paths` and
   `tasks/<slug>/`, so the post is rejected: `ownership: a task agent writes
   tests, the prompt and reviews, never "Cargo.lock": the code is the
   worker's`.

So any test that needs a crate not already in `Cargo.lock` is impossible to
post. Both rejections are strikes. Hit twice so far:

- `core-pty` (wanted `libc` for one `ioctl`): `post` 1/2 and `ownership` 1/2.
  The agent gave up on the crate and declared the ioctl by hand with Linux's
  constants. That works, but is a workaround forced by the tool.
- `html-sanitizer` (wanted `scraper` to parse the sanitizer's output node by
  node against a 136-payload XSS corpus): `post` 1/2 and `ownership` 1/2. It
  is now one strike from escalating on each, and parked waiting for this
  request. Its fallback is a hand-written HTML tokenizer inside the test, a
  weaker test.

The impure lock file is not a real impurity. It's a file the build tool
writes as a pure function of the manifests and the registry cache, and the
same post commits the change. The same shape applies to other ecosystems
(`package-lock.json`, `deno.lock`, `uv.lock`, `go.sum`).

## Why no current feature covers it

- **Spikes (§14):** a spike worker may touch any path, `Cargo.lock`
  included, but a spike never merges and nothing may `depends_on` it, so
  a dependency added there never reaches `main` or the task's tests.
- **flaky-test-fix `fix_paths` (§15):** unpins plain files for the *worker*
  of a flake fix. It isn't for task agents and only applies to that task
  kind.
- **Adding `Cargo.lock` to `test_paths`:** would let the agent commit it, but
  it would also pin it against the worker, who then can't add the product's
  own crates (`ammonia` for `html-sanitizer`): a `pins` strike for every
  task that adds a real dependency.
- **`docs_paths` for the director:** the director could land the dependency on
  `main` before the task posts. But that means the director edits
  `tests/Cargo.toml` (test code, which the skill says the director never
  writes), it needs a second commit and a suite run per dependency, and the
  director would have to know the test's needs before the agent wrote it.
- **`[sandbox] writable`:** about where a check may write outside the
  worktree. The lock file is inside the worktree, and the dirty-tree check is
  separate.

## Proposal

A director-owned list of lock files in `roadmap.toml`:

```toml
[player]
lock_files = ["Cargo.lock"]   # default []: today's behavior
```

- `agent_paths_ok` also allows any path in `lock_files` in a task agent's
  post, send-back or contest-reply commit.
- **No new trust:** the existing impurity check already proves the
  committed lock is the one the build produces. If the agent commits a
  hand-edited or stale lock, the check rewrites it, the worktree is dirty,
  and the post is rejected as impure, as today. Nothing extra to verify.
- Workers keep writing lock files as now (they aren't pinned). A lock file
  listed in `lock_files` must not also sit under `test_paths`; `init` /
  roadmap validation can refuse that.
- Since `lock_files` lives in `roadmap.toml` on `main`, agents can't widen it.
  Read it at the tip, like `test_paths`.

Tests (dashboard's own suite, `tests/task_agent.rs`): a post adding a
test-only dependency plus its lock entry is accepted when `lock_files`
names it; the same commit is `ownership` without the setting; a post
committing a lock that the check rewrites is still `impure`; a worker
commit touching the lock file is unaffected.

## Related, lower priority

**Lock-file merge conflicts.** Parallel tasks that each add crates all
append to `Cargo.lock`, and the accept-time merge conflicts on it. The
director resolves it by hand, usually by regenerating the file. An optional
`[player] lock_regen = { "Cargo.lock" = "git checkout --ours Cargo.lock && cargo metadata --offline >/dev/null" }`
(adds the missing entries without upgrading anything) could let the player resolve conflicts *only* in lock files by running the
command after the merge and then letting the suite judge the result. Every
other conflicted path still goes to the director.

## Bug: `conclude:` records the worker's answer, not the director's

`spike-grid-render` round 1: the worker's `claims/1.toml` said
`answer = "webgl"`. The operator chose differently, and the director
committed `conclude: dom-rows`. The `explored` event and the
`refs/notes/dashboard-explorations` note both say
`concluded: webgl` (`"answer":"webgl","concluded":true`). Spec §14 says the
conclude commit gets `{concluded: true, round, answer}`, and the point of
concluding is that the operator and director pick the answer. The worker's
pick is only a recommendation.

Ask: take the answer from the `conclude: <answer>` subject. Validate it
against the round's `answers` (plus `inconclusive`) and reject the marker
(`ownership`) if it isn't one. Keep the worker's pick in the note as
`recommended` so `status` can show both when they differ.

## Rejection: a plan based on a slightly stale `main` looks like a revert

The director branched `task/ui-grid-renderer` from `hub/main` (f8d8dac)
while the player was still importing the director's next `VISION.md` commit
on `main` (40d6e1a). The plan was rejected: `plan: task/ui-grid-renderer's
commits may only add tasks/ui-grid-renderer/task.toml`, because the
diff is taken against the current `main` tip, so the branch "removes" the
new VISION.md lines. Same family as personal-assistant request #4.
Diffing against the merge base (`git diff main...candidate`) would accept
it; the rejection text should at least say "based on <old>, main is <new>".
