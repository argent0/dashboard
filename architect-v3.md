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

## The architecture is written in Lean 4

The manifest and every API's types are Lean declarations in the project's
`architecture/` Lake package, in a small DSL (`architecture/Dashboard.lean`,
vendored by `dashboard arch gen` from the dashboard binary; `lake` must be
installed where the player and `dashboard arch check` run). Lean checks the
design: an API naming an undeclared module, a ref to a type that isn't
there, a misspelled budget field or a type used before its declaration is a
Lean error, so half of the old manifest validator is type checking. The
dashboard exports the declarations to JSON (`Dashboard/Export.lean`, run by
`lake env lean` on a generated runner) and keeps everything that acts on real
code: opacity, exhaustive matching, lints, path existence, ownership, code
budgets, law tests. No Mathlib, no Lean at runtime, no proofs about generated
code. The reference semantics live in `Dashboard.lean`: `Str n` is
`{ s : String // s.length ≤ n }`, a generic container is `Seq := List`, and
the Functor and Monoid laws are theorems over it.

`architecture/Architecture.lean` (director-owned; a task agent may edit
`Apis/*.lean`, never this):

```lean
import Dashboard
open Dashboard

architecture where
  max_modules = 27
  shared = ["Cargo.toml", "Cargo.lock"]
  linked = ["node_modules", "target"]
  view_rewritable = ["Cargo.lock"]
  source_paths = ["engine", "web/src"]

module database where
  lang = rust
  paths = ["engine/crates/database"]
  max_bytes = 260000
  max_ast = 90000
  test_cmd = "cargo test --offline -p database"
  build_cmd = "cargo build --offline -p database"

root «engine-bridge» where
  lang = rust
  paths = ["engine/bridge"]
  max_bytes = 20000
  build_cmd = "cargo build --offline -p engine-bridge"

api «nutrition-db» : database → nutrition where   -- generator → consumer
  paths = ["apis/nutrition-db"]                    -- bindings go to apis/nutrition-db/gen/
  max_constructors = 6
  max_fns = 3
  max_string_len = 256
```

`architecture/Apis/nutrition-db.lean` (one per API, named after it):

```lean
import Architecture
import Apis.«foldable-database»       -- every API it refs
open Dashboard

idl «nutrition-db» where
  type Name : string 64
  type Id : i64 display store
  type Parent : ref «other-api».Event
  element Consumption
    | Consumption : i64 → f64 → string 16
  container Consumptions := «foldable-database».Seq Consumption
  sum Written
    | Written : Id
    | Rejected : Reason
  fn insertProduct : Name → Kind → Grams → Written
  fn log : Consumptions → Written
  constructor Tick                     -- the API's own default sum, if it has one
```

Names with a `-` are written `«like-this»`. The older `architecture.toml` +
`*.api.kdl` form still loads; `dashboard arch migrate` converts it.

## Contracts

A (generator) produces values; B (consumer) calls A. A module can be both: it is a generator in each API it provides and a consumer in each API it uses.

- **Generator (A):** every output is a valid `ModuleAPI` value.
- **Consumer (B):**
  - Uses and matches every `C` of the API exhaustively. **No wildcards.** Adding a constructor must break the consumer.
  - Handles every `T` of the API.
  - Control flow depends only on `C`. The `T`s are payload: stored, forwarded or displayed, never branched on.
  - So the constructor budget bounds a consumer's decision space.

Because `T` is opaque (no derive, no public inner field, no accessors), a consumer cannot branch on a `T`. The rule is enforced by the API type's surface, not left to convention.

### No tunnels

```kdl
api "reports-api"
type "Payload" repr="string" display=#true     // "JSON report"
constructor "Report" "Payload"
```

Rejected: `Payload` is a tunnel. Any number of reports or methods pass
through `Report` as JSON text without spending a constructor, a `T` slot, an
element or a field, so the budgets measure nothing. So:

- A string is a name, an id or a message, never a document. Every string and
  bytes type and element field carries a bound (`string 64`, `bytes 4000000`);
  the Lean IDL requires one. The bound is one refinement among others
  ("Proofs supplied by the caller"), still required. Per API, `max_string_len` (default 256) caps
  every string bound and `max_bytes_len` (default off) every bytes bound:
  a bigger one is a budget raise, so a tunnel has to be asked for out loud.
  `dashboard arch check` prints the largest bound against the cap.
- Every distinct result shape is a constructor; every method is a `fn`.

### Proofs supplied by the caller

A callee never branches on an input `T`, yet the engine must refuse bad
writes. Who decides: the caller supplies the proof, in the type, and the
callee trusts it. In Lean the signature carries the obligation
(`log : (a : Amount) → (p : ProductRef) → Written` with `Amount := { g :
Float // 0 < g ∧ g ≤ 20000 }` and `ProductRef := { id : Int // ProductExists
id }`); the proof is erased and the data kept, which is how it compiles: a
refinement becomes a generated smart constructor run by whoever holds the raw
value, evidence becomes a token only the owning module can mint. **A refusal
is a type, a warning is a report**: refusals live in refinements, evidence and
the owner's constraints and never reach a callee's control flow; warnings are
folds over elements returned as data for the user to judge. A fact is only as
stable as what it proves, so:

| Fact | Changes? | Mechanism | Who decides at use |
|---|---|---|---|
| Value in range, format, span order | never | refinement: `where`, generated smart constructor | nobody, the type |
| Exists, append-only | never after true | `token … permanent` | nobody |
| Exists but revocable, time window | yes | `token … versioned stale=Ctor`: the owner re-checks atomically | the store's constraint |
| Balance covers amount | yes, by others | a hold: `reserve` mints the token as it decrements, `linear` | the store, at reservation |
| Unique name | yes, by others | the store's constraint, `Rejected Duplicate` | the store's constraint |
| Permission | rarely | capability token | nobody |
| Not already applied (retry) | yes, by self | `token … linear spent=Ctor`, consumed in the same transaction | the store, same transaction |
| Odd relative to history | n/a | a report from a fold over elements | the user |

The three compiled forms:

- **Refinement** (`type Grams : f64 where 0 < v ∧ v ≤ 20000`, `type Span :
  i64 × i64 where v.1 < v.2`, `type Unit : string 8 where v ∈ ["g", "ml"]`):
  a decidable fragment (comparisons of `v`, `v.1`…`v.4`, `v.length` with
  literals or each other, `∧`, `∨`, `¬`, `∈ [literals]`; anything else is an
  elaboration error) that is both the reference type's `Prop` and the
  generated check: Rust `Grams::new(f64) -> Result<Grams, Violation>`, TS and
  Kotlin constructors throw a `Violation { type_name, rule }`, the wire
  decoder runs the same check. The string bound is one refinement among
  others, still required and capped by `max_string_len`. A product prim
  (two to four scalars) wraps a tuple (Rust, TS; Kotlin takes the components
  as constructor parameters). `max_rules` counts the comparisons and
  memberships across an API's `where` clauses.
- **Evidence** (`token ProductRef : i64 permanent`): a `T` only the API's
  generator mints (`ProductRef::mint`; a consumer file naming `mint`, called
  or bare, is a lint finding). A test file (the suite's `test_paths`, the
  `files` of a director's or task agent's check) stands in for the
  generator and mints freely; production code reaching one (a Rust `mod`
  outside `#[cfg(test)]`, `include!`, a TypeScript import, a Kotlin import
  of its declarations) is a lint finding, so minted evidence stays in tests. A permanent token clones. `versioned stale=Retired` carries
  `(id, version)` (`store` adds `version()` for the owner's `where version =
  ?`) and every `fn` taking it must return a sum with `Retired`; `linear
  spent=Ctor` is taken by value in Rust (no `Clone`: single use by the
  compiler) and its `fn`'s sum must contain `Ctor`. Tokens cross modules by
  `ref`, keeping their kind (the rule on the fn's sum follows them) and never
  gaining `mint`. `max_tokens` counts them. In Lean a token is a subtype over
  an opaque `Fact`, so a value exists only as a `fn`'s result there too.
  A TypeScript binding imports each ref'd API from its gen file, by the
  path relative to its own (`../../test-report/gen/test_report`,
  extensionless). A Rust binding names it `super::<api_snake>`: the crate
  mounts every API its bindings ref, transitively, as sibling `#[path]` mods.
- **The owner's constraints** for state-dependent facts: uniqueness, time
  windows, holds and one-shot use are decided by the store (`UNIQUE`, foreign
  keys, triggers, `where version = ?`), answered as constructors (`Rejected
  Duplicate`, `Frozen`, `Stale`, `AlreadyApplied`, `Insufficient`). Nothing
  is generated; the tool only checks the sums above.

Across the wire, refinements run in the client before sending and in the
decoder after; a token travels as its id (and version) and is re-created by
the decoder. Forgery at the JSON edge is possible by construction and
accepted: the sandbox keeps libraries away from the wire, and the owner's
constraints are the last line, answering with a constructor.

### APIs are signatures

An API reads like Haskell signatures: `insertProduct :: Name -> Kind ->
Grams -> Written`. Inputs are `T`s the callee can only store or forward,
containers of elements it folds, or a named sum (a closed vocabulary such as
`Kind | Food | Supplement | Recipe`: matching its constructors is the one
decision a callee may make). Outputs are named sums the caller matches
exhaustively. In the IDL: `sum Written | Written : Id | Rejected : Reason`
and `fn insertProduct : Name → Kind → Grams → Written`; `returns` must name a
`sum` of the API, every sum must be returned or taken by some fn and every
type, token and container used by some fn or sum, and `constructor` lines keep declaring the API's unnamed default sum for
fn-less APIs. `max_fns` and `max_inputs` (input slots across all fns) are
API budgets beside `max_constructors`/`max_types`, which now count across all
sums. The generated bindings: a trait / interface per API (`insert_product(&mut
self, name: Name, …) -> Written`), implemented by the generator and handed to
the consumer by a composition root (`&mut dyn NutritionDb`; a consumer's tests
use a fake with synthetic sums); for a cross-language pair, a Rust `dispatch`
serving `{"fn": …, "args": […]}` and a TypeScript `<Api>Client` over a
`send` transport, so the root is wiring. Adding a `fn` breaks every generator,
adding a constructor every consumer: both fail at post. `type Id : i64 store`
generates the forwarding adapter (`rusqlite::ToSql`) so a callee can store a T
without reading it. A ref takes the same flags (`type Kind : ref «a».Kind
display store`), forwarding the wrapped type's, which must declare them. A `fn` taking a container (directly or inside an input sum) means the
generator folds it, so the generator module must consume the class API for
that `Seq`; the caller builds it with `new` through the `ref`. A container
only returned asks nothing of the generator. A TypeScript module gets a
container from a Rust-owned generic through its own class API
(`foldable → sandbox`): that API's binding is TypeScript only, the instance
and its laws, with no Rust side and no `impl` check (the impl files are
checked through the Rust consumers' class APIs). A named sum may be a
constructor argument, declared before the sum that takes it.

### The DAG

The module graph (an edge generator → consumer per API, roots included as
consumers) must be acyclic: a cycle is a manifest validation error, hence an
`ownership` rejection on `main`, naming the cycle. `dashboard arch check`
runs module tests in topological order, generators before consumers, and
prints the order.

### Composition roots

A root (`root «engine-bridge» where …`) calls modules and forwards their
values (typically generated dispatch and client stubs). It owns files like a
module and is budgeted like one (`max_bytes`, `max_ast`, and `max_roots` in
total), but has no `test_cmd` and no view: story tests only. It may have a `build_cmd`:
`dashboard build` runs it in a full worktree of the worker's snapshot, since
a root links the modules it calls (spec §21). It appears as an
API's consumer like a module, so "one API per pair" and the DAG stay honest,
and the lints apply to it. With `source_paths` set, every file under them
must belong to a module, an API, a root or `shared`: "`<path>`: owned by
nobody" otherwise.

## Elements and containers

An opaque `T` lets a module compute only over data it owns. For data a consumer must compute over (rows of another domain), an API declares two more kinds of thing:

- **Element**: readable, a flat sum of products over raw reprs (`string`, `bytes`, `i64`, `f64`, `bool`), never `T`s. A record is the one-variant case. An element is reachable only through a container, never as a constructor argument.
- **Container**: a parametric `f a` with declared classes. The consumer gets exactly the classes' operations: no length, no index, no iteration it drives itself. The container's storage is private.

```haskell
query :: Foldable f => Method -> Arg -> f Row             -- generator
total = foldr (\row acc -> acc + kcal row) 0 rows         -- consumer: an algebra TOTAL in Row
```

Constructors still bound control flow outside algebras. Inside an algebra the consumer decides on raw fields, which is the point; the algebra must match every variant of the element (no wildcard, the same rule as for constructors).

**One module per class.** The v1 classes are `Foldable` (`foldr`), `Functor` (`fmap`), `Traversable` (`traverse`, over `Result`) and `Monoid` (`empty`, `append`). Each class module's API *is* its class: the tool generates the class trait and the class's **law tests**; nothing of the API is hand-written. The instance is hand-written and generic (`Seq a`, like `[a]`), in the API's `impl` files. `foldable` owns the type (`foldr` and `new` are the primitives); `functor`, `traversable` and `monoid` add their instances on top of `foldr` and `new`.

```kdl
api "foldable-nutrition"
container "Seq" classes="Foldable"                 // owner: generic Seq a

api "functor-nutrition"
instance "Functor" for="foldable-nutrition.Seq"    // a class for a container another API owns

api "db-nutrition"
element "Row" {
    variant "Consumption" "f64" "string"
    variant "Product" "i64" "string"
}
container "Rows" of="Row" ref="foldable-nutrition.Seq"   // Seq Row
constructor "Success" "Rows"
```

A consumer gets `fmap` on `Rows` only if it also consumes a `functor-*` API with an instance for that `Seq`. `Monoid` serves both the container (`append` is concatenation) and any accumulator a consumer folds into: the generated `fold_map` maps each element into a monoid the consumer implements.

Laws (property tests over `i64` and `String` elements with generated functions; they run wherever the binding is compiled, so in the class module's own `test_cmd`):

- Foldable: `new(xs)` holds `xs` in order; `foldr` is the right fold.
- Functor: `fmap id == id`; `fmap (g . f) == fmap g . fmap f`; `foldr f z (fmap g xs) == foldr (f . g) z xs`.
- Traversable: `traverse Ok == Ok`; naturality (mapping the error commutes); the first failure in element order wins; `fmap` recovered from `traverse` maps in order.
- Monoid: left and right identity; associativity; `append` is concatenation; `fold_map` of singletons is the identity.

**Budgets:** `max_elements` counts variants across an API's elements and `max_fields` their fields, since readable data widens the decision space like a constructor. A container argument is one `T` slot.

**Files:** `impl` (per `[[api]]`) lists the hand-written instance files. They must lie under the generator module's paths: owned, budgeted and the worker's to edit (no `pins` strike), unlike the API's own paths. The generated Rust binding mounts each with `#[path]` beneath itself, so the instance sees the class trait as `super::Foldable`. A module's jail view also holds every API its APIs ref, transitively, with their `impl` files, so a consumer's tests use the real instance. `dashboard arch check` fails a declared class with no `impl Class for Container` in the API's `impl` files. In TypeScript there is no class module: the consumer's binding holds a generated `Seq<A>` class with exactly the classes it may use (the owner's, plus instances of APIs the same module consumes), its laws in `gen/<api>.laws.test.ts`, and elements as discriminated unions of readable records. On the wire a container is a JSON array of `{"v": variant, "f": [raw fields]}`. Kotlin has no element/container generator yet.

## Frames

A module's code may be declared further, down to its functions, in
`architecture/Frames/<module>.lean` (Rust modules so far):

```lean
frame alpha where
  src "crates/alpha/src"
  bindings "crate::apis"        -- where the crate mounts its APIs' bindings
  fn decide : Id → Plan → Decision calls rate, «core-alpha».load branch Plan
  fn rate : Score → Tally
  record Tally : i64 → bool     -- frame-local, readable types
  sum Outcome | Won : Tally | Lost
```

- **Signatures** name prims, the frame's `record`s and `sum`s, and the
  types of every API the module sees (`api.T` where two share a name).
- **`calls`** lists a fn's callees: frame fns, or `api.fn` of an API the
  module consumes. **`branch S`** makes the body a `match` on its `S`
  input with one hole per constructor.
- **Generated** by `dashboard arch gen`, pinned like a binding, under
  `<src>/frame/` (`mod.rs`, `skeleton.rs`, `holes.rs`): per fn a context
  trait `<Fn>Ctx` holding exactly its callees, the `Frame` struct holding
  the consumed APIs (`Frame::new(&mut core_alpha, …)`) and routing each
  context call, the entry fns doing the declared `match`, the impl of each
  API the module generates when its fns are frame fns, and a
  `#[cfg(test)]` stand-in per context (`DecideCtxStandIn`: queued answers
  per callee, a call log, unused answers fail at drop). The crate root
  writes `pub mod frame;`.
- **Holes** are the worker's: `<src>/holes/<fn>.rs`, one `pub fn` per hole
  (`<fn>`, or `on_<ctor>` per arm), each taking `ctx: &mut dyn <Fn>Ctx`
  first. `arch gen` writes a `todo!()` starter for a missing file and
  leaves an existing one alone. Private helpers and types may live in a
  hole file.
- **Enforced** by the build and by `dashboard arch check`. The build
  narrows each hole to `&mut dyn <Fn>Ctx`, so a hole asking for more fails
  to compile, and a new constructor breaks the generated `match`. The hole
  lint closes what the compiler leaves open: a hole names its parameters,
  its `ctx`, the frame's prelude, std minus I/O and ambient state
  (`fs`, `net`, `io`, `process`, `env`, `thread`, `time`, …) and its own
  items; no `crate::`/`super::`/`::` path, `static`, `unsafe`,
  `extern`, `macro_rules!`, `mod x;`, linking or `cfg_attr` attribute,
  `impl` for a type it does not define, macro outside a small allowed set,
  or `pub` helper. `#[cfg(test)]` items are its tests.
- **Tracking.** `dashboard arch calls [<module>]` prints the call graph,
  each API fn with its callers, and the consumed API fns no frame fn calls;
  `arch check` reports per frame its fns, holes, API calls, the APIs it
  provides and the fns those APIs never reach.
- **Ownership.** The director writes a frame on `main`, or a task agent
  for one of its task's modules: the Lean file, the regenerated skeleton
  and the starters of new holes (each exactly as `arch gen` writes it), in
  one commit; deleting the hole of a dropped fn goes with it. `src` lies
  inside the module (a load error otherwise). `arch check` reports a file
  under `<src>/frame/` the frame does not generate and a hole file of no
  declared fn. The skeleton stays outside the module's budgets.
- **Names.** The DSL's words are reserved in every Lean file of the
  package (`frame`, `fixture`, `src`, `bindings`, `calls`, `branch`,
  `record`, beside `module`, `api`, `check`, …): quote such a name as
  `«record»`. In Rust, a frame's types stay clear of `Frame`, `Q`, the
  context traits, the stand-ins and the API traits; a callee named
  `default`, `drop` or `clone` collides with the stand-in; a parameter
  named after a Rust keyword gets a `_` suffix (`match_: Match`).
- **Limits so far.** Two APIs' types sharing a name cannot both enter one
  frame. The frame governs the holes; the module's other files (the crate
  root) stay ordinary worker code. A hole's own tests are linted like the
  hole by the mint lint (a test minting a consumed token is flagged).

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
- Module tests run in a jail view: a fresh directory holding only the module's files, its APIs' files and `shared`, plus a symlink back into the checkout for each existing `linked` path (build caches like `node_modules` and `target`, never copied, never in a budget). `linked` may not overlap any module or API path. `view_rewritable` lists `shared` files a tool rewrites in a view (cargo trims a workspace lock to the members present): a check's view purity check (spec §20) skips them, and the worktree's check covers them in full. A missing cache is skipped, so the suite check's `cmd` builds the caches before `dashboard arch check`. `test_cmd` and `build_cmd` get `DASHBOARD_MODULE` and `DASHBOARD_ROOT`; `dashboard build` compiles a module in the same view (spec §21).
- Opaque `T`s are checked by the API check: each `T` is a wrapped newtype with no derive, no public inner field and no accessors, or another API's type.

## Budget increase path

Budgets exist to force a split, so a raise is the last resort and always an explicit, audited act.

### Where budgets live
In the pinned manifest (`architecture/Architecture.lean`): the module, root,
API and fixture (`max_bytes`) budgets. A worker or task agent that edits it gets a `pins` strike. Only a director commit on `main` changes it; the player's ownership gate allows the architecture's paths, like `docs_paths`.

### When work hits a budget
The suite check fails (a worker finish is rejected with an `arch` budget reason). Neither the worker nor the task agent may touch the budget. In order, the options are:

1. Refactor within the module.
2. Split the module (the director adds a module to the manifest: "Adding a module" below). This consumes the total modules budget.
3. Insert an intermediary module, if the pressure is on an API. An API is fixed per (generator, consumer) pair, so it cannot be sharded; reduce it by changing the topology.
4. Ask for a raise: a `block:` that escalates, with the measured overage in the reason.

### Adding a module
A task's `modules` must already be in the manifest, so a module enters before any task names it, as one director `main` commit holding its skeleton: the `module` and `api` entries in `Architecture.lean`, an IDL per API, the APIs' generated bindings (under each API's `gen/`), the module's empty files rendered from the project's template, and the `shared` files its build touches (a lockfile). The player admits exactly that on `main` (spec §5): bindings under any `gen/`, and a new module's paths and `shared` files only while the module's files equal the scaffold's rendering. `dashboard arch add-module <name> --api <generator>:<consumer>…` writes it into the director clone's working tree (main at the hub's tip) and runs the module's `build_cmd` so lockfiles follow; the director fills the stub IDLs, reruns `dashboard arch gen`, checks with `dashboard arch check`, and commits. At the `max_modules` cap the `+1` lands first as its own raise. `dashboard arch add-api` adds APIs between existing nodes the same way.

The template is the project's: `architecture/scaffold/module.lean` (the manifest entry), optional `api.lean` (an API's entry; `{api}`, `{generator}`, `{consumer}`) and `files/` (the module's files, mirrored from the project root). `{name}` and `{crate}` (`-` as `_`) are substituted in `module.lean`, in `files/` paths and in their contents. An API's first path without an extension names its directory, so `arch gen` writes the binding to `<path>/gen/` even before that directory exists.

### The raise
- **Every budget raise is discussed with the operator** before the director commits it. This covers module AST and byte budgets, API constructor and `T` budgets, and the total modules budget. The player does not check this (best effort): it is a rule of the director playbook, and the audit note below makes violations visible.
- API budgets (constructors, `T`s) are the most sensitive: a constructor bounds the decision space of every consumer, so one raise grows the match arms of N modules. The raise discussion lists the affected consumers (the manifest knows them). The director never raises these on its own judgment.
- The total modules budget is the project's complexity ceiling. Raise it only after trying to merge or remove modules.
- The commit changes only budget fields in the manifest (for the Lean package: only files under `architecture/`, and the export minus budgets unchanged); mixing in any other edit is an `ownership` rejection.
- It carries a required `raise:` reason, recorded by the player in `refs/notes/dashboard-budget-raises` (budget, old, new, reason, triggering task, at). `status` shows how many times each budget was raised.
- A single raise is bounded by a step cap (at most +25% of the old value; for small counts, +1). A larger jump takes several visible raises.
- `dashboard raise [<slug>] --reason …`, run in the director clone with `main` checked out at the hub's tip, commits the budget edits of its working tree as `raise(<slug>): <reason>` on top of the hub's `main`, after judging them as the player will (only budget fields, within the step cap); edits that raise nothing are refused as an ordinary `main` commit's work.
- `main` must stay green: a raise never lands together with work that depends on it going red.

### Lowering and headroom
Lowering a budget is always allowed. If it puts `main` over, the over-budget-`main` handling applies: work is held, not blamed, until a task fixes it. `dashboard arch` prints each budget's headroom, and flags budgets with long-unused headroom as candidates to tighten, so budgets do not drift upward.

### Units
Units stay deferred. The raise machinery only compares a number against its previous value, so it works with whatever units are chosen.

## Tests

Every check is declared in Lean with the modules it `covers` and its own
`files` (`architecture/Tests.lean`, `architecture/Tests/<slug>.lean`,
`architecture/Tests/<slug>.worker.lean`; spec §6.2):

```lean
import Dashboard
import Architecture
open Dashboard

check «search-story» where
  cmd = "deno test web/stories/search.test.ts"
  covers = [database, nutrition, «engine-bridge»]
  files = ["web/stories/search.test.ts"]
```

- A module's `test_cmd` is an implicit check covering that module.
- A check runs in the view of its `covers` (the module view, widened to
  every covered module, plus `test_paths` and its `files`), so its
  `covers` is proven on every run: a test reaching outside it fails.
- `Dashboard.impacted : List Node → List Check` gives a task's run set from
  its `modules` (spec §6.6); `covers = all` checks are always in it.
- The player runs the whole suite once at start, then run sets only.
- A worker's working tree holds its task's files only (spec §21);
  `dashboard build` and `dashboard test` assemble the views.

### Fixtures

Setup that modules share is declared beside them in
`architecture/Architecture.lean` (spec §6.2): `fixture <name> where` with
`users` (modules and roots), `paths` (under no node) and `max_bytes`. A
view holds a fixture when one of its nodes is a user, so a fixture shared
by `core` and `alpha` is absent from `beta`'s checks, and the test that
reaches for it there fails like any reach outside `covers`. Module-only,
API-shared and arbitrary groups are one form: `users = [alpha]`,
`users = [core, alpha]`, `users = [beta, gamma]`. The manifest's blocks
(`architecture`, `module`, `root`, `api`, `fixture`) come from
`Architecture.lean` only; a task agent's `Apis/` or `Tests/` file declaring
one fails the export.

**Generated stand-in.** Every API with `fn`s gets, in its Rust binding, a
macro `<api>_stand_in!(Name, path::to::binding)` (`-` as `_` in the name):
a `#[derive(Default)]` struct with one queue of scripted closures per fn
(`VecDeque<Box<dyn FnMut(args) -> Ret>>`), answering calls in order; an
empty queue panics naming the fn, and `calls` logs the fn names. A fixture
with `stand_in = <api>` calls it crate-neutrally, so the generator's and the
consumer's test crates share one file:

```rust
// fixtures/core-alpha/mod.rs
#![allow(unused)]
super::stand_in!(Stand, super::api);

// alpha/tests/decide.rs (core's tests: `use crate::core_alpha as api;` …)
use alpha::core_alpha as api;
use alpha::core_alpha_stand_in as stand_in;
#[path = "../../fixtures/core-alpha/mod.rs"] mod fixture;
```

The per-API name keeps two APIs' macros apart (one `stand_in!` name would
collide), and the path argument keeps the fixture free of either crate's
name. TypeScript and Kotlin interfaces take an inline object, so they get
no macro. A fixture compiles whole in each test crate that mounts it:
`#![allow(unused)]` keeps `-D warnings` builds quiet.

### Inter-module tests

* Tests that the `generator` module can generate the FSTOP with the right `T`. That is, that the types and values are
  correct.

* Tests that the `consumer` module can consume the `FSTOP`. That is, there is
  no value of the `T`s that would make the consumer crash or produce "wrong"
  output.

### User story tests

- Created from an operator-provided list.
- A story is a check whose `covers` lists the modules and roots it
  exercises; its view proves the list.
- Runs at `player` start with the whole suite, then in the run set of every
  task whose `modules` meet its `covers`.
