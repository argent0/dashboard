# Open issues

Work that follows fixture blocks and frames (`spec.md` §6.2, `architect-v3.md` "Fixtures",
"Frames"). Each issue states what is, what is missing, and a proposed route.

## Jargon

- **Fixture**: test setup (a stand-in, scenario ids, helpers) shared by the users a
  `fixture` block names.
- **Frame**: a module's functions declared in `architecture/Frames/<module>.lean`.
- **Hole**: a function body the worker writes in `<src>/holes/<fn>.rs`.
- **Abort-stub**: a generated hole body that calls `std::process::abort()`, standing in
  for a hole outside a check's reach.
- **Behaviour duplicate**: two tests asserting the same rule on the same scenario.
- **Layered overlap**: one scenario tested at two layers (consumer stand-in, hub storage,
  static data), each making its own claim; worth keeping.

## Frames

### 1. Pass reuse per hole

A check's pass is reused by its view tree, and a frame module's view holds every hole
file. Proposed: a check declares `exercises = [alpha.decide]`; the player builds its view
with the holes it reaches (through the frame's `calls`) and an abort-stub for each other
hole, keyed by the skeleton, the bindings and the reached hole files. A test calling
outside `exercises` then aborts, the way a test reaching outside `covers` finds nothing.
The pilot proved the stub views in one crate; abort beats `unreachable!()`, which
`catch_unwind` or `#[should_panic]` can swallow.

### 2. Two APIs' types sharing a name

A frame refuses two types named alike (`core-alpha.Score`, `alpha-ui.Score`). Proposed:
import them with aliases in the prelude (`CoreAlphaScore`) and name them so in holes.

### 3. Code outside the holes

A framed module's crate root and its other files stay ordinary worker code, so a worker
can implement a provided API in `lib.rs` and skip the frame. Proposed: a framed module
holds only `frame/`, `holes/`, its crate root and its manifest, and the crate root's shape
(`pub mod frame;` plus binding mounts) is checked.

### 4. Mint lint in hole tests

The mint lint reads a hole file whole, `#[cfg(test)]` items included, so a hole test
building a consumed token for a stand-in answer is flagged. Proposed: skip `#[cfg(test)]`
items there, or generate a test-only mint helper in the stand-in module.

### 5. TypeScript and Kotlin frames

Frames are Rust only. TypeScript: the context as an interface, the generated caller passing
a fresh object holding only the declared methods (structural typing would accept the whole
frame), an exhaustive `switch` with a `never` check, and a lint on imports and globals
(`Deno`, `fetch`, `globalThis`). Kotlin: interfaces, sealed classes with an exhaustive
`when`, and the same import lint (`internal` is module-wide).

### 6. Async and generic frame fns

The context is `&mut dyn <Fn>Ctx`, so frame fns stay synchronous and monomorphic, and
callee results are owned. Async would need boxed futures; a borrowed return would pin the
context across calls.

## Fixtures

### 7. Pins on an unloadable export

`records::pinned_files` reads the suite and the architecture without an exporter and
pins only `tasks/` and `architecture/` when either fails to load. A `main` commit adding a
fixture over an in-flight task's check `files` makes that branch unloadable. Proposed:
when `main` adds a fixture, check the open branches' checks against it, and make a load
failure in `pinned_files` an error.

### 8. Flaky-fix baseline

Test files absent at the merge base with `main` count as the worker's, including any the
post itself added (a repro harness). Proposed: take the baseline from the commit the pins
were first taken at.

### 9. Reserved words

The DSL's words (`fixture`, `frame`, `src`, `bindings`, `calls`, `branch`, `record`, beside
`module`, `api`, `check`, …) are tokens in every Lean file of the package; a hand-written
name among them needs `«…»`. Proposed: say so in the Lean error `arch check` relays, or
pick rarer words.

## Test hygiene (from the fixture audit)

The running example: a hub module `core` and consumers `alpha`, `beta`, `gamma`, each
tested against a stand-in of its API. An audit of such a project found about a quarter of
all test lines to be copied fixture, and the copies drift: one module tested against two
worlds.

### 10. Duplicate-setup lint

Add to `arch::lint` (findings in `src/lint.rs`), run by `dashboard arch check`:

- Duplicated blocks across test files (`loaded.test_files` plus every check's `files`): hash
  windows of N consecutive normalized lines (trimmed; skip blank, brace-only and `use`
  lines); report each window seen in two files, merged into ranges, with `file:line` on
  both sides. Fixture files are the shared home, so a block inside a fixture and a copy
  outside it reports the copy.
- `#![allow(dead_code)]` at the top of a Rust test file: every file holding dead copies in
  the audit had it.
- Threshold in the `suite where` block: `dup_lines : Nat := 0` (0 = off), in
  `src/lean/Dashboard.lean` `Suite`, `Export.lean` and `config.rs` with `#[serde(default)]`;
  documented in spec §6.2 and `architect-v3.md`. Unit tests beside `test_reach`'s.

### 11. Cleanup plan kind

`dashboard draft dedup [<slug>] [--phase <id>]` in `src/draft.rs`, built like
`speedup`/`flaky`: read issue 10's findings on `main`, plan a hygiene task whose post moves
the copies into a fixture, with a brief "helpers first, keep assertions as strong". Add a
`worker.dedup` prompt like `worker.test-speedup`, spec §15.2, and a skill line under
"Special task kinds".

### 12. Applying it to a project

Director work, through the loop, once issues 10–11 are installed:

1. On `main`: a `fixture` block per shared setup, `dup_lines` set (start near 30).
2. Hygiene tasks, one per module or small group, each moving a stand-in into its fixture
   and picking one behaviour wherever copies drifted (an operator call on each drift),
   largest copies first. Typical shapes: two task files in one consumer sharing a
   near-identical stand-in; one task file a superset of an older one; the hub's test files
   each building the same game and adapters (per-file seeds that differ on purpose stay);
   sibling UI modules holding byte-identical helpers.
3. Behaviour duplicates go through `dashboard trim` at each later accept. Typical shapes: a
   worker check whose file copies the task check's and asserts a subset; one rule asserted
   once per API surface over one shared function; one long scenario played in two story
   checks; a test asserting what a refined IDL type already guarantees; near-identical tests
   differing only in inputs (table candidates). Layered overlaps stay.

## Decisions to confirm with the operator

- Post scope: a task agent writes a fixture when any of its users is in the task's
  `modules`, and a branch changing a fixture runs every user's checks.
- A post, send-back or contest reply changing a fixture keeps its users' checks passing,
  judged on the fixture change alone.
- When two test files merge, each original check stays its own check over the merged file;
  the director folds them with trim.
