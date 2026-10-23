/-
The architecture's data (architect-v3.md): the types a project's
`architecture/` package declares and the exporter writes, and the Lean
meaning of the IDL's prims. Together they make the manifest: settings,
modules, roots, APIs, fixtures, checks, suite, IDLs and frames.

Vendored by `dashboard arch gen` as `architecture/Dashboard/ArchTypes.lean`;
a copy that differs from the installed dashboard's fails `dashboard arch
check`. Plain Lean with no `syntax`: a file importing it alone (the spec's
model, `tasks/Roadmap.lean`) keeps every word free as a name.
-/

namespace Dashboard

-- ---------------------------------------------------------------------------
-- The manifest
-- ---------------------------------------------------------------------------

inductive Lang
  | rust
  | typescript
  | kotlin
  deriving Repr, DecidableEq, Inhabited

def Lang.toString : Lang → String
  | .rust => "rust"
  | .typescript => "typescript"
  | .kotlin => "kotlin"

/-- `lang = rust` in a `module` block. -/
def rust : Lang := .rust
def typescript : Lang := .typescript
def kotlin : Lang := .kotlin

/-- A module: a set of files, tested in a jail view of only its own files,
its APIs' files and `shared`. Budgets of 0 are off. -/
structure Module where
  name : String
  lang : Lang
  paths : List String := []
  max_ast : Nat := 0
  max_bytes : Nat := 0
  test_cmd : Option String := none
  lint_cmd : Option String := none
  build_cmd : Option String := none
  deriving Repr, DecidableEq, Inhabited

/-- A composition root: calls modules and forwards their values. Budgeted
like a module, story-tested only: no `test_cmd`, no view; `build_cmd`
builds it in a full worktree. -/
structure Root where
  name : String
  lang : Lang
  paths : List String := []
  max_ast : Nat := 0
  max_bytes : Nat := 0
  build_cmd : Option String := none
  deriving Repr, DecidableEq, Inhabited

/-- What may consume an API: a module or a root. -/
inductive Node
  | ofModule (m : Module)
  | ofRoot (r : Root)
  deriving Repr, DecidableEq, Inhabited

instance : Coe Module Node := ⟨.ofModule⟩
instance : Coe Root Node := ⟨.ofRoot⟩

def Node.name : Node → String
  | .ofModule m => m.name
  | .ofRoot r => r.name

/-- One API per connected pair: `consumer` consumes `generator`. Its files
(`paths`) are pinned against workers; `impl` lists the hand-written class
instances, under the generator's paths. -/
structure Api where
  name : String
  generator : Module
  consumer : Node
  paths : List String := []
  «impl» : List String := []
  max_constructors : Nat := 0
  max_types : Nat := 0
  max_elements : Nat := 0
  max_fields : Nat := 0
  max_fns : Nat := 0
  max_inputs : Nat := 0
  max_string_len : Nat := 0
  max_bytes_len : Nat := 0
  max_rules : Nat := 0
  max_tokens : Nat := 0
  deriving Repr, DecidableEq, Inhabited

/-- The name of an API, for an `idl` block to refer to it. -/
def apiName (a : Api) : String := a.name

/-- A test fixture: setup code (a stand-in, scenario ids, helpers) shared by
exactly its `users`. Its files lie under no node; a view holds them when one
of its nodes is a user, and they are pinned against workers. `max_bytes`
counts every file under `paths` (0 is off). With `stand_in`, the users are that
API's generator and consumer, and the fixture may call the API's generated
stand-in. -/
structure Fixture where
  name : String
  users : List Node := []
  paths : List String := []
  max_bytes : Nat := 0
  stand_in : Option Api := none
  deriving Repr, DecidableEq, Inhabited

/-- The manifest's top-level fields. -/
structure Settings where
  max_modules : Nat := 0
  max_roots : Nat := 0
  shared : List String := []
  linked : List String := []
  view_rewritable : List String := []
  source_paths : List String := []
  deriving Repr, DecidableEq, Inhabited

-- ---------------------------------------------------------------------------
-- The suite (spec §6): checks in `Tests.lean` and `Tests/*.lean`
-- ---------------------------------------------------------------------------

/-- A suite check. `covers` names the modules and roots it tests: it runs
in a view of their files and joins the run set of a task on any of them.
`all` (written `covers = all`) runs it in the whole worktree for every
task. `files` are its own test files, beyond `test_paths`. -/
structure Check where
  name : String
  cmd : String
  runs : Nat := 1
  min_pass : Nat := 1
  retry_failed : Nat := 0
  retry_on : List String := []
  covers : List Node := []
  all : Bool := false
  files : List String := []
  deriving Repr, DecidableEq, Inhabited

/-- The `suite where` block of `Tests.lean`. -/
structure Suite where
  test_paths : List String := []
  list_cmd : Option String := none
  deriving Repr, DecidableEq, Inhabited

/-- The run set of a task on `nodes` (spec §6.6): every check covering one
of them, plus every `covers = all` check. The reference definition. -/
def impacted (checks : List Check) (nodes : List Node) : List Check :=
  checks.filter fun c => c.all || c.covers.any fun n => nodes.any (·.name == n.name)

-- ---------------------------------------------------------------------------
-- The IDL data an `idl` block exports
-- ---------------------------------------------------------------------------

/-- A raw representation; `max_len` 0 means none was given. -/
inductive Prim
  | text (max_len : Nat)
  | blob (max_len : Nat)
  | int64
  | float64
  | boolean
  deriving Repr, DecidableEq, Inhabited

/-- An operand of a `where` refinement. -/
inductive Atom
  | v                    -- the value (a string's text, not its proof)
  | field (i : Nat)      -- `v.i`: a component of a product, 1-based
  | len                  -- `v.length`: characters of a string, bytes of a blob
  | num (text : String)  -- a numeric literal, as written
  | str (s : String)
  | bool (b : Bool)
  deriving Repr, DecidableEq, Inhabited

/-- The decidable fragment a `where` refinement is written in (architect-v3.md,
"Proofs supplied by the caller"). The same tree is the reference type's `Prop`
and the generated smart constructor's check. -/
inductive Pred
  | cmp (op : String) (l r : Atom)     -- op: < ≤ = ≠
  | mem (a : Atom) (lits : List Atom)  -- a ∈ [literals]
  | and (p q : Pred)
  | or (p q : Pred)
  | not (p : Pred)
  deriving Repr, DecidableEq, Inhabited

inductive TypeKind
  | prim (p : Prim)
  | refTo (owner ty : String)
  | tuple (ps : List Prim)  -- a product of two to four prims, `i64 × i64`
  deriving Repr, DecidableEq, Inhabited

structure TypeDecl where
  name : String
  kind : TypeKind
  display : Bool := false
  store : Bool := false
  pred : Option Pred := none
  deriving Repr, DecidableEq, Inhabited

/-- How the fact a token proves can change between minting and use. -/
inductive TokenKind
  | permanent              -- never, once true
  | versioned (stale : String)  -- it carries (id, version); every fn taking it returns `stale`
  | linear (spent : String)     -- single use; every fn taking it returns `spent`
  deriving Repr, DecidableEq, Inhabited

/-- Evidence: a T only the API's generator mints. -/
structure Token where
  name : String
  prim : Prim
  kind : TokenKind
  store : Bool := false
  deriving Repr, DecidableEq, Inhabited

structure Ctor where
  name : String
  args : List String := []
  deriving Repr, DecidableEq, Inhabited

structure Variant where
  name : String
  fields : List Prim := []
  deriving Repr, DecidableEq, Inhabited

structure Element where
  name : String
  variants : List Variant := []
  deriving Repr, DecidableEq, Inhabited

inductive Class
  | Foldable
  | Functor
  | Traversable
  | Monoid
  deriving Repr, DecidableEq, Inhabited

def Class.toString : Class → String
  | .Foldable => "Foldable"
  | .Functor => "Functor"
  | .Traversable => "Traversable"
  | .Monoid => "Monoid"

inductive ContainerKind
  | generic (classes : List Class)
  | ofElement (elem owner seq : String)
  deriving Repr, DecidableEq, Inhabited

structure Container where
  name : String
  kind : ContainerKind
  deriving Repr, DecidableEq, Inhabited

structure Instance where
  cls : Class
  owner : String
  seq : String
  deriving Repr, DecidableEq, Inhabited

structure Sum where
  name : String
  ctors : List Ctor := []
  deriving Repr, DecidableEq, Inhabited

structure Fn where
  name : String
  inputs : List String := []
  returns : String
  deriving Repr, DecidableEq, Inhabited

structure Idl where
  apiName : String
  types : List TypeDecl := []
  tokens : List Token := []
  ctors : List Ctor := []
  elements : List Element := []
  containers : List Container := []
  instances : List Instance := []
  sums : List Sum := []
  fns : List Fn := []
  deriving Repr, DecidableEq, Inhabited

-- ---------------------------------------------------------------------------
-- The frame data a `frame` block exports
-- ---------------------------------------------------------------------------

/-- A function of a module's frame: its signature (type names: a prim word,
a type of the frame, or a type of an API the module sees, `api.T` when two
share a name), the callees its body may call (frame fns, or `api.fn` of an
API the module consumes), and `branch`, an input sum whose every
constructor gets its own hole. -/
structure FrameFn where
  name : String
  inputs : List String := []
  returns : String
  callees : List String := []
  branchOn : Option String := none
  deriving Repr, DecidableEq, Inhabited

/-- A frame-local product: readable fields, for values the module computes
inside itself. -/
structure Record where
  name : String
  fields : List String := []
  deriving Repr, DecidableEq, Inhabited

/-- A module's frame (`architecture/Frames/<module>.lean`): the functions
its code is made of, generated as a pinned skeleton whose bodies are holes
the worker fills. `src` is the source directory the skeleton goes under
(`<src>/gen/`, holes in `<src>/holes/`); `bindings` the Rust path the
module's crate mounts its APIs' bindings under (`crate::apis`: then
`crate::apis::core_alpha`). -/
structure Frame where
  moduleName : String
  srcDir : String := ""
  mount : String := "crate"
  fns : List FrameFn := []
  records : List Record := []
  sums : List Sum := []
  deriving Repr, DecidableEq, Inhabited

-- ---------------------------------------------------------------------------
-- The reference semantics the native declarations are built on
-- ---------------------------------------------------------------------------

/-- A string of at most `n` characters: `max_len` as a refinement type. -/
abbrev Str (n : Nat) := { s : String // s.length ≤ n }
abbrev Bytes (n : Nat) := { b : ByteArray // b.size ≤ n }
abbrev I64 := Int
abbrev F64 := Float

/-- The reference container every generic `Seq` is: a list. The class laws
the generated Rust tests check hold here as theorems. -/
abbrev Seq := List

theorem Seq.map_id (xs : Seq α) : xs.map id = xs := List.map_id xs
theorem Seq.map_comp (f : α → β) (g : β → γ) (xs : Seq α) :
    (xs.map f).map g = xs.map (g ∘ f) := List.map_map
theorem Seq.foldr_map (g : α → β) (f : β → γ → γ) (z : γ) (xs : Seq α) :
    (xs.map g).foldr f z = xs.foldr (fun x acc => f (g x) acc) z := by
  induction xs <;> simp [*]
theorem Seq.empty_append (xs : Seq α) : [] ++ xs = xs := List.nil_append xs
theorem Seq.append_empty (xs : Seq α) : xs ++ [] = xs := List.append_nil xs
theorem Seq.append_assoc (xs ys zs : Seq α) : (xs ++ ys) ++ zs = xs ++ (ys ++ zs) :=
  List.append_assoc xs ys zs

end Dashboard
