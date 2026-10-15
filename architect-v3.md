# Architect: modules, APIs and total consumers

Third version. `architect.md` is the original; `architect-refined.md` is the second draft with review comments. This one folds in the answers to those comments.

## Modules

- The director defines the modules. A module is a set of files.
- Modules talk to each other only through APIs.
- **Budgets:** total AST size and total byte size per module. Budgets are set per project. Units are deferred until there is an implementation.

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
- Opaque `T`s are checked by the API check: each `T` is a wrapped newtype with no derive, no public inner field and no accessors, or another API's type.

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
