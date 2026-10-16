# Architect: modules, APIs and total consumers

Third version. `architect.md` is the original; `architect-refined.md` is the second draft with review comments. This one folds in the answers to those comments.

## Modules

- The director defines the modules. A module is a set of files.
- Modules talk to each other only through APIs.
- **Budgets:** total AST size and total byte size per module, and a **total modules budget**: the maximum number of modules in the project. Budgets are set per project. Units are deferred until there is an implementation.

## APIs

- An API is a set of files, distinct from its module, so a change of API is an explicit change.
- **One API per connected pair.** For each pair of modules (A, B) where B consumes A there is one API file. A module consumed by N other modules has N APIs.
- An API is a **flat sum type of products** (FSTOP) (Haskell is used only as notation):

```haskell
data ModuleAPI =
    C_1 T_1_1 T_1_2 ...
  | C_2 T_2_1 T_2_2 ...
```

- **Budgets:** the number of constructors `C`, and the total number of argument types `T`. Set per project; units deferred.
- **`T` is opaque.** Each `T` is a wrapped newtype, or another API's type (matched through that API). Concretely, a `T`:
  - derives nothing (no `PartialEq`, `Ord`, `Hash`, ...),
  - has no public inner field,
  - has no accessors.
- Anything a consumer must decide on goes in a constructor, not in a `T`.

## Contracts

A (generator) produces values; B (consumer) calls A. A module can be both: it is a generator in each API it provides and a consumer in each API it uses.

- **Generator (A):** every output is a valid `ModuleAPI` value.
- **Consumer (B):**
  - Uses and matches every `C` of the API exhaustively. **No wildcards.** Adding a constructor must break the consumer.
  - Handles every `T` of the API.
  - Control flow depends only on `C`. The `T`s are payload: stored, forwarded or displayed, never branched on.
  - So the constructor budget bounds a consumer's decision space.

Because `T` is opaque (no derive, no public inner field, no accessors), a consumer cannot branch on a `T`. The rule is enforced by the API type's surface, not left to convention.

## Changes and testing

### Intra-module change

- Run only the module's own tests.
- Generator side: outputs still type-check as `ModuleAPI`.
- Consumer side: the matches are still total.
- Consumer tests use synthetic `ModuleAPI` values: cases deemed test-worthy, not A's real outputs.

### Inter-module change

A change that touches more than one module is split into:

1. **API change.** Edit the API files. Consumers that no longer use every `C` and handle every `T` fail, so tests break (this is the failing-at-post check). Any API change that breaks nothing is not a change of the contract.
2. **Generator changes.** Tested in the generator module alone.
3. **Consumer changes.** Tested in the consumer module alone, against synthetic `ModuleAPI` values.

Steps 2 and 3 depend only on the API from step 1, not on each other, so they can run in parallel.

**State of `main`.** `main` never goes red. The director sequences the tasks of steps 1 to 3 until the tests are green, and only then is a commit made to `main`.

## Enforcement (as pins + suite checks, no new player machinery)

- API dirs are pinned like `test_paths`. Only the director or task agent changes them; a worker that does gets a `pins` strike.
- Suite checks per module: the constructor / `T` budget, the AST and byte budgets, and the no-wildcard rule.
  - The no-wildcard rule needs a per-language check (compiler or linter); which one is a project choice, not part of this design.
  - Rust: `clippy::wildcard_enum_match_arm` and `clippy::match_wildcard_for_single_variants` at deny.
- Module tests run in a jail view: a fresh directory holding only the module's files, its APIs' files and `shared`, plus a symlink back into the checkout for each existing `linked` path (build caches like `node_modules` and `target`, never copied, never in a budget). `linked` may not overlap any module or API path. A missing cache is skipped, so the suite check's `cmd` builds the caches before `dashboard arch check`. `test_cmd` gets `DASHBOARD_MODULE` and `DASHBOARD_ROOT`.
- Opaque `T`s are checked by the API check: each `T` is a wrapped newtype with no derive, no public inner field and no accessors, or another API's type.

## Budget increase path

Budgets exist to force a split, so a raise is the last resort and always an explicit, audited act.

### Where budgets live
In the pinned `architecture.toml` (its own file). A worker or task agent that edits it gets a `pins` strike. Only a director commit on `main` changes it; the player's ownership gate allows that one path, like `docs_paths`.

### When work hits a budget
The suite check fails (a worker finish is rejected with an `arch` budget reason). Neither the worker nor the task agent may touch the budget. In order, the options are:

1. Refactor within the module.
2. Split the module (the director adds a module to the manifest). This consumes the total modules budget.
3. Insert an intermediary module, if the pressure is on an API. An API is fixed per (generator, consumer) pair, so it cannot be sharded; reduce it by changing the topology.
4. Ask for a raise: a `block:` that escalates, with the measured overage in the reason.

### The raise
- **Every budget raise is discussed with the operator** before the director commits it. This covers module AST and byte budgets, API constructor and `T` budgets, and the total modules budget. The player does not check this (best effort): it is a rule of the director playbook, and the audit note below makes violations visible.
- API budgets (constructors, `T`s) are the most sensitive: a constructor bounds the decision space of every consumer, so one raise grows the match arms of N modules. The raise discussion lists the affected consumers (the manifest knows them). The director never raises these on its own judgment.
- The total modules budget is the project's complexity ceiling. Raise it only after trying to merge or remove modules.
- The commit changes only budget fields in `architecture.toml`; mixing in any other edit is an `ownership` rejection.
- It carries a required `raise:` reason, recorded by the player in `refs/notes/dashboard-budget-raises` (budget, old, new, reason, triggering task, at). `status` shows how many times each budget was raised.
- A single raise is bounded by a step cap (at most +25% of the old value; for small counts, +1). A larger jump takes several visible raises.
- `main` must stay green: a raise never lands together with work that depends on it going red.

### Lowering and headroom
Lowering a budget is always allowed. If it puts `main` over, the over-budget-`main` handling applies: work is held, not blamed, until a task fixes it. `dashboard arch` prints each budget's headroom, and flags budgets with long-unused headroom as candidates to tighten, so budgets do not drift upward.

### Units
Units stay deferred. The raise machinery only compares a number against its previous value, so it works with whatever units are chosen.

## Tests

### Inter-module tests

* Tests that the `generator` module can generate the FSTOP with the right `T`. That is, that the types and values are
  correct.

* Tests that the `consumer` module can consume the `FSTOP`. That is, there is
  no value of the `T`s that would make the consumer crash or produce "wrong"
  output.

### User story tests

- Created from an operator-provided list.
- The modules a story impacts are detected with `atime` or `strace`-like tools.
- Run at `player` start and on merges to `main`.
- On merges to `main`, only the stories that exercise the changed modules run.
