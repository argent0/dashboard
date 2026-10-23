/-
The architecture's syntax (architect-v3.md): the commands a director writes
in `architecture/` (`architecture`, `module`, `root`, `api`, `fixture`,
`check`, `suite`, `idl`, `frame` … `where`), each elaborating to the data of
`Architect.Types`. Every word atom of a `syntax` line here is a token in
every file that imports it, so a name spelled like one needs `«…»`.

Vendored as `architecture/Architect/Syntax.lean`. Plain Lean (no
`import Lean`), so an API file elaborates fast.
-/
import Architect.Types

namespace Dashboard

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

syntax (name := archFixture) "fixture " ident " where" manyIndent(group(ident " = " term)) : command

/-- One field of a `fixture` block: `users = [a, b]` coerces each element
to a `Node`; `stand_in = x` is `some x`. -/
def fixtureField (key : Lean.Ident) (val : Lean.TSyntax `term) : Lean.MacroM (Lean.TSyntax `term) := do
  let field := key.getId.eraseMacroScopes
  if field == `users then
    match val with
    | `([$xs,*]) =>
      let elems ← xs.getElems.mapM fun x => `(($x : Dashboard.Node))
      `([$elems,*])
    | _ => Lean.Macro.throwErrorAt val "users is a list of modules and roots: users = [a, b]"
  else if field == `stand_in then
    `(some $val)
  else
    pure val

macro_rules
  | `(fixture $n:ident where $[$k:ident = $v:term]*) => do
    let s := Lean.Syntax.mkStrLit (identText n)
    let vs ← (k.zip v).mapM fun (key, val) => fixtureField key val
    `(def $n : Dashboard.Fixture := { name := $s, $[$k:ident := $vs],* })

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
syntax "type " ident " : " "ref " ident ("display")? ("store")? : idlItem
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
  | `(idlItem| type $n:ident : ref $r:ident $[display%$d]? $[store%$s]?) => do
    let (a, t) ← splitRef r
    -- `display` and `store` forward the wrapped type's.
    let d ← if d.isSome then `(true) else `(false)
    let s ← if s.isSome then `(true) else `(false)
    let decl ← `(Dashboard.TypeDecl.mk $(strLit n) (Dashboard.TypeKind.refTo $(Lean.Syntax.mkStrLit a) $(Lean.Syntax.mkStrLit t)) $d $s none)
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

-- ---------------------------------------------------------------------------
-- The frame DSL
-- ---------------------------------------------------------------------------

/-
A module's frame, in `architecture/Frames/<module>.lean`:

  frame alpha where
    src "crates/alpha/src"
    bindings "crate::apis"
    fn decide : HeroId → Plan → Decision calls score, «core-alpha».load branch Plan
    fn score : Hero → Score calls «dice-alpha».roll
    record Tally : i64 → bool
    sum Outcome | Win : Tally | Lose

`dashboard arch gen` generates the skeleton (pinned) and a starter for each
missing hole; `dashboard arch check` keeps every hole inside its callees.
-/
declare_syntax_cat frameTy
declare_syntax_cat frameItem

syntax idlPrim : frameTy
syntax ident : frameTy

syntax "src " str : frameItem
syntax "bindings " str : frameItem
syntax "fn " ident " : " sepBy1(frameTy, " → ") (" calls " sepBy1(ident, ", "))? (" branch " ident)? : frameItem
syntax "record " ident " : " sepBy1(frameTy, " → ") : frameItem
syntax "sum " ident many("|" ident (" : " sepBy1(frameTy, " → "))?) : frameItem

syntax (name := archFrame) "frame " ident " where" manyIndent(frameItem) : command

/-- A frame type's name: a prim's word (`i64`, `string`), else the identifier. -/
def frameTyText : Lean.TSyntax `frameTy → Lean.MacroM (Lean.TSyntax `term)
  | `(frameTy| string $[$_:num]?) => pure (Lean.Syntax.mkStrLit "string")
  | `(frameTy| bytes $[$_:num]?) => pure (Lean.Syntax.mkStrLit "bytes")
  | `(frameTy| i64) => pure (Lean.Syntax.mkStrLit "i64")
  | `(frameTy| f64) => pure (Lean.Syntax.mkStrLit "f64")
  | `(frameTy| bool) => pure (Lean.Syntax.mkStrLit "bool")
  | `(frameTy| $n:ident) => pure (strLit n)
  | _ => Lean.Macro.throwUnsupported

structure FrameAcc where
  srcDir : Lean.TSyntax `term := Lean.Syntax.mkStrLit ""
  mount : Lean.TSyntax `term := Lean.Syntax.mkStrLit "crate"
  fns : Array (Lean.TSyntax `term) := #[]
  records : Array (Lean.TSyntax `term) := #[]
  sums : Array (Lean.TSyntax `term) := #[]

def addFrameItem (acc : FrameAcc) : Lean.TSyntax `frameItem → Lean.MacroM FrameAcc
  | `(frameItem| src $s:str) => pure { acc with srcDir := s }
  | `(frameItem| bindings $s:str) => pure { acc with mount := s }
  | `(frameItem| fn $n:ident : $[$ts:frameTy]→* $[calls $[$cs:ident],*]? $[branch $b:ident]?) => do
    let names ← ts.mapM frameTyText
    let ret := names.back!
    let ins := names.pop
    let cl := (cs.getD #[]).map fun c => strLit c
    let br ← match b with
      | some b => `(some $(strLit b))
      | none => `(none)
    let decl ← `(Dashboard.FrameFn.mk $(strLit n) $(← listTerm ins) $ret $(← listTerm cl) $br)
    pure { acc with fns := acc.fns.push decl }
  | `(frameItem| record $n:ident : $[$ts:frameTy]→*) => do
    let fields ← ts.mapM frameTyText
    pure { acc with records := acc.records.push (← `(Dashboard.Record.mk $(strLit n) $(← listTerm fields))) }
  | `(frameItem| sum $n:ident $[| $cs:ident $[: $[$as:frameTy]→*]?]*) => do
    let mut ctors := #[]
    for c in cs, a in as do
      let args ← (a.getD #[]).mapM frameTyText
      ctors := ctors.push (← `(Dashboard.Ctor.mk $(strLit c) $(← listTerm args)))
    pure { acc with sums := acc.sums.push (← `(Dashboard.Sum.mk $(strLit n) $(← listTerm ctors))) }
  | _ => Lean.Macro.throwUnsupported

/-- `frame m where …` declares `m.frame`; `m` must be a declared module. -/
macro_rules
  | `(frame $n:ident where $items:frameItem*) => do
    let mut acc : FrameAcc := {}
    for it in items do
      acc ← addFrameItem acc it
    let id := Lean.mkIdent (n.getId ++ `frame)
    `(def $id : Dashboard.Frame := {
        moduleName := ($n : Dashboard.Module).name,
        srcDir := $(acc.srcDir),
        mount := $(acc.mount),
        fns := $(← listTerm acc.fns),
        records := $(← listTerm acc.records),
        sums := $(← listTerm acc.sums) })

end Dashboard
