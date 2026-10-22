/-
The dashboard architecture DSL (architect-v3.md): the manifest's modules,
roots, APIs and budgets, and each API's IDL, as Lean declarations. Vendored
into a project's `architecture/` by `dashboard arch gen`; do not edit: a copy
that differs from the installed dashboard's fails `dashboard arch check`.

This file is plain Lean (no `import Lean`), so an API file elaborates fast.
The exporter that turns the declarations into JSON is `Dashboard.Export`.
-/

namespace Dashboard

-- ---------------------------------------------------------------------------
-- The manifest
-- ---------------------------------------------------------------------------

inductive Lang
  | rust
  | typescript
  | kotlin
  deriving Repr, BEq, Inhabited

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
  deriving Repr, Inhabited

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
  deriving Repr, Inhabited

/-- What may consume an API: a module or a root. -/
inductive Node
  | ofModule (m : Module)
  | ofRoot (r : Root)
  deriving Repr, Inhabited

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
  deriving Repr, Inhabited

/-- The name of an API, for an `idl` block to refer to it. -/
def apiName (a : Api) : String := a.name

/-- The manifest's top-level fields. -/
structure Settings where
  max_modules : Nat := 0
  max_roots : Nat := 0
  shared : List String := []
  linked : List String := []
  view_rewritable : List String := []
  source_paths : List String := []
  deriving Repr, Inhabited

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
  deriving Repr, Inhabited

/-- The `suite where` block of `Tests.lean`. -/
structure Suite where
  test_paths : List String := []
  list_cmd : Option String := none
  deriving Repr, Inhabited

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
  deriving Repr, BEq, Inhabited

/-- An operand of a `where` refinement. -/
inductive Atom
  | v                    -- the value (a string's text, not its proof)
  | field (i : Nat)      -- `v.i`: a component of a product, 1-based
  | len                  -- `v.length`: characters of a string, bytes of a blob
  | num (text : String)  -- a numeric literal, as written
  | str (s : String)
  | bool (b : Bool)
  deriving Repr, Inhabited

/-- The decidable fragment a `where` refinement is written in (architect-v3.md,
"Proofs supplied by the caller"). The same tree is the reference type's `Prop`
and the generated smart constructor's check. -/
inductive Pred
  | cmp (op : String) (l r : Atom)     -- op: < ≤ = ≠
  | mem (a : Atom) (lits : List Atom)  -- a ∈ [literals]
  | and (p q : Pred)
  | or (p q : Pred)
  | not (p : Pred)
  deriving Repr, Inhabited

inductive TypeKind
  | prim (p : Prim)
  | refTo (owner ty : String)
  | tuple (ps : List Prim)  -- a product of two to four prims, `i64 × i64`
  deriving Repr, Inhabited

structure TypeDecl where
  name : String
  kind : TypeKind
  display : Bool := false
  store : Bool := false
  pred : Option Pred := none
  deriving Repr, Inhabited

/-- How the fact a token proves can change between minting and use. -/
inductive TokenKind
  | permanent              -- never, once true
  | versioned (stale : String)  -- it carries (id, version); every fn taking it returns `stale`
  | linear (spent : String)     -- single use; every fn taking it returns `spent`
  deriving Repr, Inhabited

/-- Evidence: a T only the API's generator mints. -/
structure Token where
  name : String
  prim : Prim
  kind : TokenKind
  store : Bool := false
  deriving Repr, Inhabited

structure Ctor where
  name : String
  args : List String := []
  deriving Repr, Inhabited

structure Variant where
  name : String
  fields : List Prim := []
  deriving Repr, Inhabited

structure Element where
  name : String
  variants : List Variant := []
  deriving Repr, Inhabited

inductive Class
  | Foldable
  | Functor
  | Traversable
  | Monoid
  deriving Repr, BEq, Inhabited

def Class.toString : Class → String
  | .Foldable => "Foldable"
  | .Functor => "Functor"
  | .Traversable => "Traversable"
  | .Monoid => "Monoid"

inductive ContainerKind
  | generic (classes : List Class)
  | ofElement (elem owner seq : String)
  deriving Repr, Inhabited

structure Container where
  name : String
  kind : ContainerKind
  deriving Repr, Inhabited

structure Instance where
  cls : Class
  owner : String
  seq : String
  deriving Repr, Inhabited

structure Sum where
  name : String
  ctors : List Ctor := []
  deriving Repr, Inhabited

structure Fn where
  name : String
  inputs : List String := []
  returns : String
  deriving Repr, Inhabited

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
  deriving Repr, Inhabited

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

-- ---------------------------------------------------------------------------
-- The manifest DSL
-- ---------------------------------------------------------------------------

syntax (name := archSettings) "architecture" " where" manyIndent(group(ident " = " term)) : command
syntax (name := archModule) "module " ident " where" manyIndent(group(ident " = " term)) : command
syntax (name := archRoot) "root " ident " where" manyIndent(group(ident " = " term)) : command
syntax (name := archApi) "api " ident " : " ident " → " ident " where" manyIndent(group(ident " = " term)) : command

/-- The plain text of a declaration name: `«engine-api»` is `engine-api`. -/
def identText (n : Lean.Syntax) : String :=
  n.getId.toString (escape := false)

macro_rules
  | `(architecture where $[$k:ident = $v:term]*) => do
    let id := Lean.mkIdent `settings
    `(def $id : Dashboard.Settings := { $[$k:ident := $v:term],* })

macro_rules
  | `(module $n:ident where $[$k:ident = $v:term]*) => do
    let s := Lean.Syntax.mkStrLit (identText n)
    `(def $n : Dashboard.Module := { name := $s, $[$k:ident := $v:term],* })

macro_rules
  | `(root $n:ident where $[$k:ident = $v:term]*) => do
    let s := Lean.Syntax.mkStrLit (identText n)
    `(def $n : Dashboard.Root := { name := $s, $[$k:ident := $v:term],* })

macro_rules
  | `(api $n:ident : $g:ident → $c:ident where $[$k:ident = $v:term]*) => do
    let s := Lean.Syntax.mkStrLit (identText n)
    `(def $n : Dashboard.Api := { name := $s, generator := $g, consumer := $c, $[$k:ident := $v:term],* })

syntax (name := archCheck) "check " ident " where" manyIndent(group(ident " = " term)) : command
syntax (name := archSuite) "suite" " where" manyIndent(group(ident " = " term)) : command

/-- `covers = all` sets `all`; `covers = [a, b]` coerces each element to a
`Node`, so modules and roots mix. A check named like a module is a
redeclaration. -/
macro_rules
  | `(check $n:ident where $[$k:ident = $v:term]*) => do
    let s := Lean.Syntax.mkStrLit (identText n)
    let mut ks : Array Lean.Ident := #[]
    let mut vs : Array (Lean.TSyntax `term) := #[]
    for key in k, val in v do
      if key.getId.eraseMacroScopes != `covers then
        ks := ks.push key
        vs := vs.push val
      else if val.raw.isIdent && val.raw.getId.eraseMacroScopes == `all then
        ks := ks.push (Lean.mkIdent `all)
        vs := vs.push (← `(true))
      else
        match val with
        | `([$xs,*]) =>
          let elems ← xs.getElems.mapM fun x => `(($x : Dashboard.Node))
          ks := ks.push key
          vs := vs.push (← `([$elems,*]))
        | _ => Lean.Macro.throwErrorAt val "covers is `all` or a list of modules and roots: covers = [a, b]"
    `(def $n : Dashboard.Check := { name := $s, $[$ks:ident := $vs],* })

macro_rules
  | `(suite where $[$k:ident = $v:term]*) => do
    let id := Lean.mkIdent `suite
    `(def $id : Dashboard.Suite := { $[$k:ident := $v:term],* })

-- ---------------------------------------------------------------------------
-- The IDL DSL
-- ---------------------------------------------------------------------------

declare_syntax_cat idlPrim
declare_syntax_cat idlItem

syntax "string" (num)? : idlPrim
syntax "bytes" (num)? : idlPrim
syntax "i64" : idlPrim
syntax "f64" : idlPrim
syntax "bool" : idlPrim

/-
Proofs supplied by the caller (architect-v3.md): a callee never branches on
an input `T`, so the caller's proof travels in the type.

  type Grams : f64 where 0 < v ∧ v ≤ 20000      -- a refinement: `new` checks it
  type Day   : string 10 where v.length = 10    -- the bound stays; `where` adds to it
  type Span  : i64 × i64 where v.1 < v.2        -- a product of two to four prims
  type Unit  : string 8 where v ∈ ["g", "ml"]
  token ProductRef   : i64 permanent            -- evidence: only the generator mints it
  token EventTypeRef : i64 versioned stale=Retired   -- carries (id, version); fns taking it return `Retired`
  token Funds        : i64 linear spent=AlreadyApplied  -- single use; fns taking it return `AlreadyApplied`

The predicate fragment is comparisons (`<`, `≤`, `=`, `≠`) of `v`, `v.N`,
`v.length` and literals, `∧`, `∨`, `¬` and `v ∈ [literals]`; anything else is
an elaboration error. A named `sum` may also be a `fn` input: a closed
vocabulary the callee matches.
-/
declare_syntax_cat idlAtom
declare_syntax_cat idlPred
declare_syntax_cat idlTokenKind

syntax ident : idlAtom                     -- v, v.length, true, false
syntax ident noWs "." noWs num : idlAtom   -- v.1
syntax num : idlAtom
syntax "-" noWs num : idlAtom
syntax scientific : idlAtom
syntax "-" noWs scientific : idlAtom
syntax str : idlAtom

syntax:50 idlAtom " < " idlAtom : idlPred
syntax:50 idlAtom " ≤ " idlAtom : idlPred
syntax:50 idlAtom " = " idlAtom : idlPred
syntax:50 idlAtom " ≠ " idlAtom : idlPred
syntax:50 idlAtom " ∈ " "[" sepBy1(idlAtom, ", ") "]" : idlPred
syntax:35 idlPred:36 " ∧ " idlPred:35 : idlPred
syntax:30 idlPred:31 " ∨ " idlPred:30 : idlPred
syntax:max "¬" idlPred:40 : idlPred
syntax:max "(" idlPred ")" : idlPred

syntax "permanent" : idlTokenKind
syntax "versioned " "stale" "=" ident : idlTokenKind
syntax "linear " "spent" "=" ident : idlTokenKind

syntax "type " ident " : " sepBy1(idlPrim, " × ") ("display")? ("store")? (" where " idlPred)? : idlItem
syntax "token " ident " : " idlPrim idlTokenKind ("store")? : idlItem
syntax "type " ident " : " "ref " ident : idlItem
syntax "element " ident many("|" ident (" : " sepBy1(idlPrim, " → "))?) : idlItem
syntax "container " ident " : " sepBy1(ident, ", ") : idlItem
syntax "container " ident " := " ident ident : idlItem
syntax "instance " ident " for " ident : idlItem
syntax "sum " ident many("|" ident (" : " sepBy1(ident, " → "))?) : idlItem
syntax "fn " ident " : " sepBy1(ident, " → ") : idlItem
syntax "constructor " ident (" : " sepBy1(ident, " → "))? : idlItem

syntax (name := archIdl) "idl " ident " where" manyIndent(idlItem) : command

/-- `(api, Type)` of a two-component name like `«foldable-x».Seq`. -/
def splitRef (n : Lean.Syntax) : Lean.MacroM (String × String) :=
  match n.getId with
  | .str (.str .anonymous a) t => pure (a, t)
  | _ => Lean.Macro.throwErrorAt n "expected api.Name"

def primTerm : Lean.TSyntax `idlPrim → Lean.MacroM (Lean.TSyntax `term × Lean.TSyntax `term)
  | `(idlPrim| string $[$n:num]?) => do
    let n := n.getD (Lean.Syntax.mkNumLit "0")
    pure (← `(Dashboard.Prim.text $n), ← `(Dashboard.Str $n))
  | `(idlPrim| bytes $[$n:num]?) => do
    let n := n.getD (Lean.Syntax.mkNumLit "0")
    pure (← `(Dashboard.Prim.blob $n), ← `(Dashboard.Bytes $n))
  | `(idlPrim| i64) => do pure (← `(Dashboard.Prim.int64), ← `(Dashboard.I64))
  | `(idlPrim| f64) => do pure (← `(Dashboard.Prim.float64), ← `(Dashboard.F64))
  | `(idlPrim| bool) => do pure (← `(Dashboard.Prim.boolean), ← `(Bool))
  | _ => Lean.Macro.throwUnsupported

/-- Whether a prim is text (a string or bytes): its reference value is the
`.val` of a bounded subtype, and `.length` reads it. -/
def primIsText : Lean.TSyntax `idlPrim → Bool
  | `(idlPrim| string $[$_:num]?) => true
  | `(idlPrim| bytes $[$_:num]?) => true
  | _ => false

/-- The source text of a scientific literal: `(m, true, e)` is `m / 10^e`. -/
def sciText : Nat × Bool × Nat → String
  | (m, false, e) => toString m ++ String.ofList (List.replicate e '0')
  | (m, true, e) =>
    let digits := (toString m).toList
    let digits := if digits.length ≤ e then List.replicate (e + 1 - digits.length) '0' ++ digits else digits
    let cut := digits.length - e
    String.ofList (digits.take cut) ++ "." ++ String.ofList (digits.drop cut)

/-- `(exported atom, reference term)` of one atom of a `where` predicate over
a value bound as `v` of a type with components `texts` (one for a scalar;
`true` where a component is a string or bytes). -/
def atomTerms (texts : List Bool) : Lean.TSyntax `idlAtom → Lean.MacroM (Lean.TSyntax `term × Lean.TSyntax `term)
  | `(idlAtom| $n:ident) => do
    match n.getId with
    | `v =>
      if texts.length > 1 then Lean.Macro.throwErrorAt n "a product is compared by component: v.1, v.2, …"
      let t ← if texts.getD 0 false then `(v.val) else `(v)
      pure (← `(Dashboard.Atom.v), t)
    | `v.length =>
      if texts.length == 1 && texts.getD 0 false then pure (← `(Dashboard.Atom.len), ← `(v.val.length))
      else Lean.Macro.throwErrorAt n "`.length` is for a string or bytes type"
    | `true => pure (← `(Dashboard.Atom.bool true), ← `(true))
    | `false => pure (← `(Dashboard.Atom.bool false), ← `(false))
    | _ => Lean.Macro.throwErrorAt n "expected v, v.N, v.length or a literal (comparisons, ∧, ∨, ¬, ∈ [literals])"
  | `(idlAtom| $n:ident.$i:num) => do
    if n.getId != `v then Lean.Macro.throwErrorAt n "expected v.N"
    let k := i.getNat
    let arity := texts.length
    if arity < 2 then Lean.Macro.throwErrorAt n "v.N is for a product type (`i64 × i64`)"
    if k == 0 || k > arity then Lean.Macro.throwErrorAt i s!"v.{k}: the product has {arity} components"
    -- `A × B × C` is `A × (B × C)`: component k is k-1 `.2`s, then `.1` unless last.
    let mut t ← `(v)
    for _ in [:k - 1] do
      t ← `(($t).2)
    if k < arity then
      t ← `(($t).1)
    if texts.getD (k - 1) false then
      t ← `(($t).val)
    pure (← `(Dashboard.Atom.field $i), t)
  | `(idlAtom| $n:num) => do
    pure (← `(Dashboard.Atom.num $(Lean.Syntax.mkStrLit (toString n.getNat))), ← `($n))
  | `(idlAtom| -$n:num) => do
    pure (← `(Dashboard.Atom.num $(Lean.Syntax.mkStrLit ("-" ++ toString n.getNat))), ← `(-$n))
  | `(idlAtom| $n:scientific) => do
    pure (← `(Dashboard.Atom.num $(Lean.Syntax.mkStrLit (sciText n.getScientific))), ← `($n))
  | `(idlAtom| -$n:scientific) => do
    pure (← `(Dashboard.Atom.num $(Lean.Syntax.mkStrLit ("-" ++ sciText n.getScientific))), ← `(-$n))
  | `(idlAtom| $s:str) => do pure (← `(Dashboard.Atom.str $s), ← `($s))
  | _ => Lean.Macro.throwUnsupported

/-- `(exported predicate, reference `Prop`)` of a `where` predicate. -/
partial def predTerms (texts : List Bool) : Lean.TSyntax `idlPred → Lean.MacroM (Lean.TSyntax `term × Lean.TSyntax `term)
  | `(idlPred| $a:idlAtom < $b:idlAtom) => cmp "<" a b
  | `(idlPred| $a:idlAtom ≤ $b:idlAtom) => cmp "≤" a b
  | `(idlPred| $a:idlAtom = $b:idlAtom) => cmp "=" a b
  | `(idlPred| $a:idlAtom ≠ $b:idlAtom) => cmp "≠" a b
  | `(idlPred| $a:idlAtom ∈ [$ls:idlAtom,*]) => do
    let (ea, ta) ← atomTerms texts a
    let mut es := #[]
    let mut ts := #[]
    for l in ls.getElems do
      let (e, t) ← atomTerms texts l
      es := es.push e
      ts := ts.push t
    pure (← `(Dashboard.Pred.mem $ea [$es,*]), ← `($ta ∈ [$ts,*]))
  | `(idlPred| $p:idlPred ∧ $q:idlPred) => bin (fun a b => `(Dashboard.Pred.and $a $b)) (fun a b => `($a ∧ $b)) p q
  | `(idlPred| $p:idlPred ∨ $q:idlPred) => bin (fun a b => `(Dashboard.Pred.or $a $b)) (fun a b => `($a ∨ $b)) p q
  | `(idlPred| ¬ $p:idlPred) => do
    let (e, t) ← predTerms texts p
    pure (← `(Dashboard.Pred.not $e), ← `(¬ $t))
  | `(idlPred| ($p:idlPred)) => predTerms texts p
  | _ => Lean.Macro.throwUnsupported
where
  cmp (op : String) (a b : Lean.TSyntax `idlAtom) : Lean.MacroM (Lean.TSyntax `term × Lean.TSyntax `term) := do
    let (ea, ta) ← atomTerms texts a
    let (eb, tb) ← atomTerms texts b
    let t ← match op with
      | "<" => `($ta < $tb)
      | "≤" => `($ta ≤ $tb)
      | "=" => `($ta = $tb)
      | _ => `($ta ≠ $tb)
    pure (← `(Dashboard.Pred.cmp $(Lean.Syntax.mkStrLit op) $ea $eb), t)
  bin (fe ft : Lean.TSyntax `term → Lean.TSyntax `term → Lean.MacroM (Lean.TSyntax `term))
      (p q : Lean.TSyntax `idlPred) : Lean.MacroM (Lean.TSyntax `term × Lean.TSyntax `term) := do
    let (ep, tp) ← predTerms texts p
    let (eq, tq) ← predTerms texts q
    pure (← fe ep eq, ← ft tp tq)

/-- Everything one `idl` block declares, gathered item by item. -/
structure IdlAcc where
  cmds : Array (Lean.TSyntax `command) := #[]
  types : Array (Lean.TSyntax `term) := #[]
  tokens : Array (Lean.TSyntax `term) := #[]
  ctors : Array (Lean.TSyntax `term) := #[]
  elements : Array (Lean.TSyntax `term) := #[]
  containers : Array (Lean.TSyntax `term) := #[]
  instances : Array (Lean.TSyntax `term) := #[]
  sums : Array (Lean.TSyntax `term) := #[]
  fns : Array (Lean.TSyntax `term) := #[]
  /-- The `fn` names and signatures: the fields of the generated `Fns` structure. -/
  fnNames : Array Lean.Ident := #[]
  fnTypes : Array (Lean.TSyntax `term) := #[]
  /-- The default sum's constructors: names and native types. -/
  ctorNames : Array Lean.Ident := #[]
  ctorTypes : Array (Lean.TSyntax `term) := #[]

def strLit (n : Lean.Syntax) : Lean.TSyntax `term :=
  Lean.Syntax.mkStrLit (identText n)

def listTerm (xs : Array (Lean.TSyntax `term)) : Lean.MacroM (Lean.TSyntax `term) :=
  `([$xs,*])

def asTerm (i : Lean.Ident) : Lean.TSyntax `term := ⟨i.raw⟩

/-- A constructor's arrow type ending in `ret`. -/
def arrowType (args : Array (Lean.TSyntax `term)) (ret : Lean.TSyntax `term) : Lean.MacroM (Lean.TSyntax `term) :=
  args.foldrM (fun a acc => `($a → $acc)) ret

def ctorTerm (c : Lean.Ident) (args : Array Lean.Ident) : Lean.MacroM (Lean.TSyntax `term) := do
  let names ← listTerm (args.map fun a => strLit a)
  `(Dashboard.Ctor.mk $(strLit c) $names)

def classTerm (cls : Lean.Ident) : Lean.TSyntax `term :=
  asTerm (Lean.mkIdent (`Dashboard.Class ++ cls.getId))

def addItem (acc : IdlAcc) : Lean.TSyntax `idlItem → Lean.MacroM IdlAcc
  | `(idlItem| type $n:ident : $[$ps:idlPrim]×* $[display%$d]? $[store%$s]? $[where $w:idlPred]?) => do
    let mut pts := #[]
    let mut tys := #[]
    for p in ps do
      let (pt, ty) ← primTerm p
      pts := pts.push pt
      tys := tys.push ty
    if pts.size > 4 then Lean.Macro.throwErrorAt n "a product has two to four components"
    let kind ← if pts.size == 1 then `(Dashboard.TypeKind.prim $(pts[0]!)) else `(Dashboard.TypeKind.tuple $(← listTerm pts))
    let ty ← tys.pop.foldrM (fun a acc => `($a × $acc)) tys.back!
    let d ← if d.isSome then `(true) else `(false)
    let s ← if s.isSome then `(true) else `(false)
    let (pred, ty) ← match w with
      | some w =>
        let (e, prop) ← predTerms (ps.map primIsText).toList w
        pure (← `(some $e), ← `({ v : $ty // $prop }))
      | none => pure (← `(none), ty)
    let decl ← `(Dashboard.TypeDecl.mk $(strLit n) $kind $d $s $pred)
    let cmd ← `(abbrev $n : Type := $ty)
    pure { acc with cmds := acc.cmds.push cmd, types := acc.types.push decl }
  | `(idlItem| token $n:ident : $p:idlPrim $k:idlTokenKind $[store%$s]?) => do
    let (pt, ty) ← primTerm p
    let kind ← match k with
      | `(idlTokenKind| permanent) => `(Dashboard.TokenKind.permanent)
      | `(idlTokenKind| versioned stale = $c:ident) => `(Dashboard.TokenKind.versioned $(strLit c))
      | `(idlTokenKind| linear spent = $c:ident) => `(Dashboard.TokenKind.linear $(strLit c))
      | _ => Lean.Macro.throwUnsupported
    let s ← if s.isSome then `(true) else `(false)
    let decl ← `(Dashboard.Token.mk $(strLit n) $pt $kind $s)
    -- The fact is opaque: in Lean too, the only way to a value is a `fn` of
    -- this API that returns one.
    let fact := Lean.mkIdent (n.getId ++ `Fact)
    let c1 ← `(opaque $fact : $ty → Prop)
    let c2 ← `(abbrev $n : Type := { id : $ty // $fact id })
    pure { acc with cmds := acc.cmds.push c1 |>.push c2, tokens := acc.tokens.push decl }
  | `(idlItem| type $n:ident : ref $r:ident) => do
    let (a, t) ← splitRef r
    let decl ← `(Dashboard.TypeDecl.mk $(strLit n) (Dashboard.TypeKind.refTo $(Lean.Syntax.mkStrLit a) $(Lean.Syntax.mkStrLit t)) false false none)
    let cmd ← `(abbrev $n : Type := $r)
    pure { acc with cmds := acc.cmds.push cmd, types := acc.types.push decl }
  | `(idlItem| element $n:ident $[| $vs:ident $[: $[$fs:idlPrim]→*]?]*) => do
    let mut variants := #[]
    let mut tys := #[]
    for v in vs, f in fs do
      let mut prims := #[]
      let mut fieldTys := #[]
      for p in f.getD #[] do
        let (pt, ty) ← primTerm p
        prims := prims.push pt
        fieldTys := fieldTys.push ty
      variants := variants.push (← `(Dashboard.Variant.mk $(strLit v) $(← listTerm prims)))
      tys := tys.push (← arrowType fieldTys (asTerm n))
    let decl ← `(Dashboard.Element.mk $(strLit n) $(← listTerm variants))
    let cmd ← `(inductive $n : Type where $[| $vs:ident : $tys]*)
    pure { acc with cmds := acc.cmds.push cmd, elements := acc.elements.push decl }
  | `(idlItem| container $n:ident : $cs:ident,*) => do
    let classes := cs.getElems.map classTerm
    let decl ← `(Dashboard.Container.mk $(strLit n) (Dashboard.ContainerKind.generic $(← listTerm classes)))
    let cmd ← `(abbrev $n : Type → Type := Dashboard.Seq)
    pure { acc with cmds := acc.cmds.push cmd, containers := acc.containers.push decl }
  | `(idlItem| container $n:ident := $r:ident $e:ident) => do
    let (a, c) ← splitRef r
    let decl ← `(Dashboard.Container.mk $(strLit n)
      (Dashboard.ContainerKind.ofElement $(strLit e) $(Lean.Syntax.mkStrLit a) $(Lean.Syntax.mkStrLit c)))
    let cmd ← `(abbrev $n : Type := $r $e)
    pure { acc with cmds := acc.cmds.push cmd, containers := acc.containers.push decl }
  | `(idlItem| instance $cls:ident for $r:ident) => do
    let (a, c) ← splitRef r
    let decl ← `(Dashboard.Instance.mk $(classTerm cls) $(Lean.Syntax.mkStrLit a) $(Lean.Syntax.mkStrLit c))
    pure { acc with instances := acc.instances.push decl }
  | `(idlItem| sum $n:ident $[| $cs:ident $[: $[$as:ident]→*]?]*) => do
    let mut ctors := #[]
    let mut tys := #[]
    for c in cs, a in as do
      let args := a.getD #[]
      ctors := ctors.push (← ctorTerm c args)
      tys := tys.push (← arrowType (args.map asTerm) (asTerm n))
    let decl ← `(Dashboard.Sum.mk $(strLit n) $(← listTerm ctors))
    let cmd ← `(inductive $n : Type where $[| $cs:ident : $tys]*)
    pure { acc with cmds := acc.cmds.push cmd, sums := acc.sums.push decl }
  | `(idlItem| fn $n:ident : $[$ts:ident]→*) => do
    let ret := ts.back!
    let ins := ts.pop
    let decl ← `(Dashboard.Fn.mk $(strLit n) $(← listTerm (ins.map fun a => strLit a)) $(strLit ret))
    let ty ← arrowType (ins.map asTerm) (asTerm ret)
    pure { acc with fns := acc.fns.push decl, fnNames := acc.fnNames.push n, fnTypes := acc.fnTypes.push ty }
  | `(idlItem| constructor $c:ident $[: $[$as:ident]→*]?) => do
    let args := as.getD #[]
    let decl ← ctorTerm c args
    let ty ← arrowType (args.map asTerm) (asTerm (Lean.mkIdent `Api))
    pure { acc with ctors := acc.ctors.push decl, ctorNames := acc.ctorNames.push c, ctorTypes := acc.ctorTypes.push ty }
  | _ => Lean.Macro.throwUnsupported

macro_rules
  | `(idl $n:ident where $items:idlItem*) => do
    let mut acc : IdlAcc := {}
    for it in items do
      acc ← addItem acc it
    let mut cmds := acc.cmds
    if !acc.ctorNames.isEmpty then
      let names := acc.ctorNames
      let tys := acc.ctorTypes
      let id := Lean.mkIdent `Api
      cmds := cmds.push (← `(inductive $id : Type where $[| $names:ident : $tys]*))
    if !acc.fnNames.isEmpty then
      let names := acc.fnNames
      let tys := acc.fnTypes
      let id := Lean.mkIdent `Fns
      cmds := cmds.push (← `(structure $id where $[$names:ident : $tys]*))
    let id := Lean.mkIdent `spec
    let spec ← `(def $id : Dashboard.Idl := {
        apiName := Dashboard.apiName $n,
        types := $(← listTerm acc.types),
        tokens := $(← listTerm acc.tokens),
        ctors := $(← listTerm acc.ctors),
        elements := $(← listTerm acc.elements),
        containers := $(← listTerm acc.containers),
        instances := $(← listTerm acc.instances),
        sums := $(← listTerm acc.sums),
        fns := $(← listTerm acc.fns) })
    cmds := cmds.push spec
    let all := #[← `(namespace $n)] ++ cmds ++ #[← `(end $n)]
    return Lean.mkNullNode (all.map (·.raw))

end Dashboard
