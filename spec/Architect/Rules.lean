/-
The structural rules (architect-v3, decision 11): what a manifest must hold
that its data alone decides, as functions returning problems. Each command
checks its own declaration with the `…problems` of what it declares, so an
error lands on the block; `Manifest.problems` adds the rules that read
several files, which the exporter runs (`#wellformed`). Measured rules (AST
and byte budgets, ownership, lints, pinning) read files and stay
`dashboard arch check`'s.

Vendored as `architecture/Architect/Rules.lean`. Plain Lean with no
`syntax`, like `Architect.Types`.
-/
import Architect.Tests.Types

namespace Dashboard

/-- A project's architecture: what `architecture/` declares and the exporter
writes (`config::ArchConfig` plus every `idl::Idl`). -/
structure Manifest where
  settings : Settings := {}
  modules : List Module := []
  roots : List Root := []
  apis : List Api := []
  fixtures : List Fixture := []
  checks : List Check := []
  suite : Suite := {}
  idls : List Idl := []
  frames : List Frame := []
  deriving Repr, DecidableEq, Inhabited

/-- `[msg]` when `ok` fails. -/
def rule (ok : Bool) (msg : String) : List String :=
  if ok then [] else [msg]

/-- Throws every problem as one error: what a command's check runs, so the
error lands on its block. -/
def ensure (ps : List String) : IO Unit :=
  if ps.isEmpty then pure () else throw (IO.userError ("\n".intercalate ps))

-- ---------------------------------------------------------------------------
-- Names and paths
-- ---------------------------------------------------------------------------

/-- A module, root or check name: lowercase letters, digits, `-` and `_`. -/
def nameOk (s : String) : Bool :=
  !s.isEmpty && s.all fun c => c.isLower || c.isDigit || c == '-' || c == '_'

/-- A path with its trailing `/` dropped. -/
def trimSlash (p : String) : String :=
  String.ofList (p.toList.reverse.dropWhile (· == '/')).reverse

/-- A path relative to the project root, with every segment a name. -/
def relPath (p : String) : Bool :=
  let t := trimSlash p
  !t.isEmpty && !t.startsWith "/" && (t.splitOn "/").all fun c => c != "." && c != ".."

/-- `p` is one of `roots` or lies inside one. -/
def under (p : String) (roots : List String) : Bool :=
  let p := trimSlash p
  roots.any fun r => let r := trimSlash r; p == r || p.startsWith (r ++ "/")

/-- One of `a` and `b` lies inside the other. -/
def overlaps (a b : String) : Bool :=
  under a [b] || under b [a]

/-- One problem per path of `ps` written otherwise than relative. -/
def relPaths (who what : String) (ps : List String) : List String :=
  ps.filterMap fun p =>
    if relPath p then none else some s!"{who}: {what} {repr p} is a relative path whose segments are names (no ., .., leading /)"

/-- A component's own files: a non-empty list of relative paths. -/
def ownPaths (who : String) (ps : List String) : List String :=
  rule (!ps.isEmpty) s!"{who}: paths is empty: name its files" ++ relPaths who "path" ps

-- ---------------------------------------------------------------------------
-- What one declaration shows: each command runs its own
-- ---------------------------------------------------------------------------

def Settings.problems (s : Settings) : List String :=
  relPaths "architecture" "shared" s.shared ++
  relPaths "architecture" "linked" s.linked ++
  relPaths "architecture" "source_paths" s.source_paths ++
  relPaths "architecture" "view_rewritable" s.view_rewritable ++
  s.view_rewritable.filterMap fun r =>
    if under r s.shared then none else some s!"architecture: view_rewritable {repr r} lies under a shared path"

def Module.problems (m : Module) : List String :=
  let who := s!"module {m.name}"
  rule (nameOk m.name) s!"{who}: a name is lowercase letters, digits, - and _" ++ ownPaths who m.paths

def Root.problems (r : Root) : List String :=
  let who := s!"root {r.name}"
  rule (nameOk r.name) s!"{who}: a name is lowercase letters, digits, - and _" ++ ownPaths who r.paths

/-- An API joins two components, and its hand-written instances lie in the
generator's files. -/
def Api.problems (a : Api) : List String :=
  let who := s!"api {a.name}"
  rule (a.generator.name != a.consumer.name) s!"{who}: the generator and the consumer are two components" ++
  ownPaths who a.paths ++
  relPaths who "impl" a.impl ++
  a.impl.filterMap fun i =>
    if under i a.generator.paths then none
    else some s!"{who}: impl {repr i} lies inside generator module {a.generator.name}'s paths"

/-- A fixture serves named components from files of its own; a stand-in's
users are its API's two ends. -/
def Fixture.problems (f : Fixture) : List String :=
  let who := s!"fixture {f.name}"
  rule (!f.users.isEmpty) s!"{who}: users is empty: name the modules and roots it serves" ++
  ownPaths who f.paths ++
  f.paths.filterMap (fun p =>
    if overlaps p "architecture" || overlaps p "tasks" then some s!"{who}: path {repr p} lies apart from architecture/ and tasks/"
    else none) ++
  match f.stand_in with
  | none => []
  | some a =>
    let want := [a.generator.name, a.consumer.name]
    let got := f.users.map Component.name
    rule (got.all want.contains && want.all got.contains)
      s!"{who}: stand_in = {a.name}, so users are exactly {a.generator.name} and {a.consumer.name} (the stand-in compiles only in their views)"

/-- A check runs at least once, passes at least once in `runs`, retries only
with a budget, covers something, and owns relative files. Each `retry_on`
entry is a non-empty substring of the failing output it retries. -/
def Check.problems (c : Check) : List String :=
  let who := s!"check {c.name}"
  rule (c.runs ≥ 1) s!"{who}: runs is at least 1 (got {c.runs})" ++
  rule (1 ≤ c.min_pass && c.min_pass ≤ c.runs) s!"{who}: min_pass lies between 1 and runs ({c.runs}), got {c.min_pass}" ++
  rule (c.retry_on.isEmpty || c.retry_failed ≥ 1) s!"{who}: retry_on needs retry_failed ≥ 1" ++
  rule (!c.retry_on.contains "") s!"{who}: a retry_on entry is a non-empty substring" ++
  rule (c.covers != .only []) s!"{who}: covers is empty: write covers = all or covers = [module, …]" ++
  relPaths who "files" c.files

/-- `bindings` is a Rust path: `::`-separated names. -/
def rustPath (s : String) : Bool :=
  (s.splitOn "::").all fun seg => !seg.isEmpty && seg.all fun c => c.isAlphanum || c == '_'

/-- A frame is Rust so far, writes under its module's files, and mounts its
bindings at a Rust path. -/
def Frame.placement (f : Frame) (m : Module) : List String :=
  let who := s!"frame {m.name}"
  rule (m.lang == .rust) s!"{who}: frames are Rust only so far; {m.name} is {m.lang.toString}" ++
  (if !relPath f.srcDir then [s!"{who}: src {repr f.srcDir} is a relative directory (src \"crates/{m.name}/src\")"]
   else rule (under f.srcDir m.paths) s!"{who}: src {repr f.srcDir} lies inside module {m.name}'s paths") ++
  rule (rustPath f.mount) s!"{who}: bindings {repr f.mount} is a Rust path (crate::apis)"

-- ---------------------------------------------------------------------------
-- One IDL against its API (architect-v3 § APIs are signatures, § No tunnels)
-- ---------------------------------------------------------------------------

/-- A type, token, element, container or sum name: an ASCII capital, then
letters and digits. -/
def camelOk (s : String) : Bool :=
  match s.toList with
  | c :: cs => c.isUpper && cs.all Char.isAlphanum
  | [] => false

/-- A fn name: an ASCII lowercase letter, then letters and digits. -/
def fnNameOk (s : String) : Bool :=
  match s.toList with
  | c :: cs => c.isLower && cs.all Char.isAlphanum
  | [] => false

def Prim.isText : Prim → Bool
  | .text _ => true
  | .blob _ => true
  | _ => false

/-- A string's or bytes' bound; 0 when none was given. -/
def Prim.bound : Prim → Nat
  | .text n => n
  | .blob n => n
  | _ => 0

def Atom.isLiteral : Atom → Bool
  | .num _ | .str _ | .bool _ => true
  | _ => false

/-- An atom of a `where` predicate over a type with components `ps`. -/
def Atom.problems (ps : List Prim) : Atom → List String
  | .v => rule (ps.length == 1) "a product is compared by component: v.1, v.2, …"
  | .field i =>
    if ps.length < 2 then [s!"v.{i} is for a product type"]
    else rule (i != 0 && i ≤ ps.length) s!"v.{i}: the product has {ps.length} components"
  | .len => rule (match ps with | [p] => p.isText | _ => false) "`.length` is for a string or bytes type"
  | _ => []

def Pred.problems (ps : List Prim) : Pred → List String
  | .cmp _ l r => l.problems ps ++ r.problems ps
  | .mem a ls => a.problems ps ++
    rule (!a.isLiteral && !ls.isEmpty && ls.all Atom.isLiteral) "`∈` compares v with a non-empty list of literals"
  | .and p q | .or p q => p.problems ps ++ q.problems ps
  | .not p => p.problems ps

/-- Comparisons and memberships (the `max_rules` measure). -/
def Pred.leaves : Pred → Nat
  | .cmp .. | .mem .. => 1
  | .and p q | .or p q => p.leaves + q.leaves
  | .not p => p.leaves

def TokenKind.failure? : TokenKind → Option String
  | .permanent => none
  | .versioned c | .linear c => some c

def TokenKind.word : TokenKind → String
  | .permanent => "permanent"
  | .versioned _ => "versioned"
  | .linear _ => "linear"

/-- The raw components of a type: one for a scalar, each of a product. -/
def TypeDecl.prims (t : TypeDecl) : List Prim :=
  match t.kind with
  | .prim p _ => [p]
  | .tuple ps => ps
  | .refTo .. => []

namespace Idl

/-- Every constructor: the default sum's, then the named sums'. -/
def allCtors (i : Idl) : List Ctor := i.ctors ++ i.sums.flatMap (·.ctors)

def constructors (i : Idl) : Nat := i.allCtors.length
def slots (i : Idl) : Nat := (i.allCtors.map (·.args.length)).sum
def elementVariants (i : Idl) : Nat := (i.elements.map (·.variants.length)).sum
def elementFields (i : Idl) : Nat := (i.elements.flatMap (·.variants) |>.map (·.fields.length)).sum
def inputs (i : Idl) : Nat := (i.fns.map (·.inputs.length)).sum
def rules (i : Idl) : Nat := (i.types.filterMap (·.pred) |>.map Pred.leaves).sum

/-- Every bounded prim the API carries: types, tokens and element fields. -/
def prims (i : Idl) : List Prim :=
  i.types.flatMap TypeDecl.prims ++ i.tokens.map Token.prim ++
  (i.elements.flatMap Element.variants).flatMap Variant.fields

/-- The largest string `max_len`, and the largest bytes `max_len`. -/
def longest (i : Idl) : Nat × Nat :=
  i.prims.foldl (fun (s, b) p => match p with
    | .text n => (max s n, b)
    | .blob n => (s, max b n)
    | _ => (s, b)) (0, 0)

/-- `(class, container)` of every generic's classes, then every instance. -/
def classes (i : Idl) : List (Class × String) :=
  i.containers.flatMap (fun c => match c.kind with
    | .generic cs => cs.map (·, c.name)
    | .ofElement .. => []) ++
  i.instances.map fun x => (x.cls, x.seq)

def isSum (i : Idl) (n : String) : Bool := i.sums.any (·.name == n)
def isElement (i : Idl) (n : String) : Bool := i.elements.any (·.name == n)
def isGeneric (i : Idl) (n : String) : Bool :=
  i.containers.any fun c => c.name == n && match c.kind with | .generic _ => true | _ => false

/-- The fns taking `input` whose returned sum lacks `ctor`. -/
def fnsMissing (i : Idl) (input ctor : String) : List Fn :=
  i.fns.filter fun f => f.inputs.contains input &&
    !(i.sums.find? (·.name == f.returns) |>.any fun s => s.ctors.any (·.name == ctor))

/-- What one argument (of a constructor, or a fn input) may name: a declared
type, never an element or a generic container. -/
def argProblems (i : Idl) (names : List String) (who : String) (a : String) : List String :=
  rule (names.contains a) s!"{who}: unknown type {repr a}" ++
  rule (!i.isElement a) s!"{who}: element {a} is reachable only through a container" ++
  rule (!i.isGeneric a) s!"{who}: {a} is generic; instantiate it with `container ... of=`"

/-- The IDL's own rules: identifiers, products, refinements, elements and
containers, signatures, usage, token failures, and that it declares
something. Unprefixed; `problems` adds the API. -/
def shape (i : Idl) : List String :=
  let decl := i.types.map (·.name) ++ i.tokens.map (·.name) ++ i.elements.map (·.name) ++ i.containers.map (·.name)
  let what := i.types.map (fun _ => "type") ++ i.tokens.map (fun _ => "token") ++
    i.elements.map (fun _ => "element") ++ i.containers.map (fun _ => "container")
  let dup (xs : List String) (k : Nat) : Bool := (xs.take k).contains xs[k]!
  let idents := (List.range decl.length).filterMap fun k =>
    let n := decl[k]!
    if camelOk n && !dup decl k then none else some s!"{what[k]!} {repr n}: a CamelCase identifier, declared once"
  let types := i.types.flatMap fun t =>
    (match t.kind with
      | .tuple ps => rule (2 ≤ ps.length && ps.length ≤ 4) s!"type {t.name}: a product has two to four components"
      | _ => []) ++
    match t.pred with
    | none => []
    | some p => (match t.kind with
        | .refTo .. => [s!"type {t.name}: a ref carries its own refinement; `where` is for a repr or a product"]
        | _ => (p.problems t.prims).map (s!"type {t.name}: {·}"))
  let elements := i.elements.flatMap fun e =>
    let vs := e.variants.map (·.name)
    ((List.range vs.length).filterMap fun k =>
      if camelOk vs[k]! && !dup vs k then none
      else some s!"element {e.name}: variant {repr vs[k]!}: a CamelCase identifier, declared once") ++
    rule (!e.variants.isEmpty) s!"element {e.name}: at least one variant" ++
    rule (i.containers.any fun c => match c.kind with | .ofElement x .. => x == e.name | _ => false)
      s!"element {e.name}: a container holds it (an element is reachable only through one)"
  let containers := i.containers.flatMap fun c => match c.kind with
    | .generic cs =>
      rule (cs.contains .Foldable) s!"container {c.name}: a generic's classes include Foldable, its primitive" ++
      rule i.instances.isEmpty s!"container {c.name}: an api owning a container adds no instances to another's"
    | .ofElement x .. => rule (i.isElement x) s!"container {c.name}: of={repr x} is an element of this api"
  let instances :=
    (i.instances.filter (·.cls == .Foldable)).map (fun x =>
      s!"instance Foldable for {x.owner}.{x.seq}: Foldable is declared by the api owning the container") ++
    rule (i.instances.all fun x => some x.owner == (i.instances.head?.map (·.owner))) "instances: all for containers of one api"
  let ctorNames := i.allCtors.map (·.name)
  let ctorIdents := (List.range ctorNames.length).filterMap fun k =>
    if camelOk ctorNames[k]! && !dup ctorNames k then none
    else some s!"constructor {repr ctorNames[k]!}: a CamelCase identifier, declared once across the api's sums"
  let ctors := i.ctors.flatMap fun c =>
    (c.args.filter (!i.isSum ·)).flatMap (i.argProblems decl s!"constructor {c.name}")
  let sumNames := i.sums.map (·.name)
  let sums := (List.range i.sums.length).flatMap fun k =>
    let s := i.sums[k]!
    let earlier := sumNames.take k
    rule (camelOk s.name && !decl.contains s.name && !earlier.contains s.name)
      s!"sum {repr s.name}: a CamelCase identifier, declared once" ++
    rule (!s.ctors.isEmpty) s!"sum {s.name}: at least one constructor" ++
    s.ctors.flatMap fun c => c.args.flatMap fun a =>
      if i.isSum a then
        rule (earlier.contains a) s!"sum {s.name}: constructor {c.name}: sum {a} is declared before {s.name} (a sum never contains itself)"
      else i.argProblems decl s!"sum {s.name}: constructor {c.name}" a
  let fnNames := i.fns.map (·.name)
  let fns := (List.range i.fns.length).flatMap fun k =>
    let f := i.fns[k]!
    rule (fnNameOk f.name && !(fnNames.take k).contains f.name) s!"fn {repr f.name}: a camelCase identifier, declared once" ++
    (f.inputs.filter (!i.isSum ·)).flatMap (i.argProblems decl s!"fn {f.name}") ++
    rule (i.isSum f.returns) s!"fn {f.name}: returns {repr f.returns}, a sum of this api"
  let used := i.allCtors.flatMap (·.args) ++ i.fns.flatMap fun f => f.returns :: f.inputs
  let usage :=
    (i.sums.filter (!used.contains ·.name)).map (s!"sum {·.name}: declared but no fn returns or takes it") ++
    (i.types.filter (!used.contains ·.name)).map (s!"type {·.name}: declared but never used") ++
    (i.tokens.filter (!used.contains ·.name)).map (s!"token {·.name}: declared but never used") ++
    (i.containers.filter fun c => (match c.kind with | .ofElement .. => true | _ => false) && !used.contains c.name).map
      (s!"container {·.name}: declared but never used")
  let tokens := i.tokens.flatMap fun t => match t.kind.failure? with
    | none => []
    | some c => (i.fnsMissing t.name c).map fun f =>
      s!"fn {f.name}: takes {t.name} ({t.kind.word}), so {f.returns} must have a `{c}` constructor: the fact may have changed by the call"
  let something := rule (!i.ctors.isEmpty || !i.fns.isEmpty || !i.classes.isEmpty) "no constructors"
  idents ++ types ++ elements ++ containers ++ instances ++ ctorIdents ++ ctors ++ sums ++ fns ++ usage ++ tokens ++ something

/-- No tunnels: every string and bytes is bounded, within the API's caps. -/
def tunnels (i : Idl) (a : Api) : List String :=
  let cap := if a.max_string_len > 0 then a.max_string_len else 256
  let (s, b) := i.longest
  i.types.flatMap (fun t => match t.kind with
    | .prim p _ => rule (!p.isText || p.bound > 0) s!"type {t.name}: a string or bytes type needs a max_len (a tunnel otherwise)"
    | .tuple ps => (List.range ps.length).filterMap fun k =>
      if ps[k]!.isText && ps[k]!.bound == 0 then some s!"type {t.name}: component {k + 1} needs a max_len (`string 64`)" else none
    | .refTo .. => []) ++
  i.tokens.flatMap (fun t =>
    rule (!t.prim.isText || t.prim.bound > 0) s!"token {t.name}: a string or bytes token needs a max_len") ++
  i.elements.flatMap (fun e => e.variants.flatMap fun v => (List.range v.fields.length).filterMap fun k =>
    if v.fields[k]!.isText && v.fields[k]!.bound == 0 then
      some s!"element {e.name}: variant {v.name}: field {k} needs a max_len (`string 64`)" else none) ++
  rule (s ≤ cap) s!"a string max_len {s} exceeds the api's max_string_len {cap} (a raise: commit if it is no tunnel)" ++
  rule (a.max_bytes_len == 0 || b ≤ a.max_bytes_len) s!"a bytes max_len {b} exceeds the api's max_bytes_len {a.max_bytes_len}"

/-- The API's budgets on its IDL; a budget of 0 is off. -/
def budgets (i : Idl) (a : Api) : List String :=
  [("constructors", i.constructors, a.max_constructors), ("T slots", i.slots, a.max_types),
   ("element variants", i.elementVariants, a.max_elements), ("element fields", i.elementFields, a.max_fields),
   ("fns", i.fns.length, a.max_fns), ("fn inputs", i.inputs, a.max_inputs),
   ("refinement rules", i.rules, a.max_rules), ("tokens", i.tokens.length, a.max_tokens)].filterMap
    fun (what, used, max) => if max == 0 || used ≤ max then none else some s!"{what} {used} exceeds budget {max}"

/-- Every problem of the IDL of `a` that the IDL and `a` show: what the
`idl` command checks. -/
def problems (i : Idl) (a : Api) : List String :=
  (i.shape ++ i.tunnels a ++ i.budgets a).map (s!"api {a.name}: {·}")

end Idl

-- ---------------------------------------------------------------------------
-- APIs that ref each other (architect-v3 § Containers, § Refs)
-- ---------------------------------------------------------------------------

def Component.lang : Component → Lang
  | .ofModule m => m.lang
  | .ofRoot r => r.lang

/-- The API's two ends speak different languages, so it carries wire code. -/
def Api.wire (a : Api) : Bool := a.generator.lang != a.consumer.lang

/-- The API shares a component with `t`. -/
def Api.linked (a t : Api) : Bool :=
  [a.generator.name, a.consumer.name].any fun n => n == t.generator.name || n == t.consumer.name

def TypeKind.adapters : TypeKind → List Adapter
  | .prim _ as | .refTo _ _ as => as
  | .tuple _ => []

namespace Idl

/-- `(api, type)` of every `ref`, with the local type's declaration. -/
def refs (i : Idl) : List (String × String × TypeDecl) :=
  i.types.filterMap fun t => match t.kind with
    | .refTo a ty _ => some (a, ty, t)
    | _ => none

/-- `(api, container)` of every container `ref` and instance `for`. -/
def containerRefs (i : Idl) : List (String × String) :=
  i.containers.filterMap (fun c => match c.kind with
    | .ofElement _ a k => some (a, k)
    | .generic _ => none) ++
  i.instances.map fun x => (x.owner, x.seq)

def declares (i : Idl) (n : String) : Bool :=
  i.types.any (·.name == n) || i.tokens.any (·.name == n)

end Idl

/-- The rules between an API and the APIs it refs: each target shares a
component with it and declares what it names; a ref forwards adapters the
target declares and keeps a token's kind; a container a fn takes is folded
by the generator, which consumes its class API. -/
def Manifest.refProblems (m : Manifest) (a : Api) (i : Idl) : List String :=
  let who := s!"api {a.name}"
  let api? (n : String) := m.apis.find? (·.name == n)
  let idl? (n : String) := m.idls.find? (·.apiName == n)
  let inputs := i.fns.flatMap fun f => f.inputs.flatMap fun x =>
    x :: (i.sums.filter (·.name == x)).flatMap fun s => s.ctors.flatMap (·.args)
  let consume := i.containers.flatMap fun c => match c.kind with
    | .ofElement _ target seq => match api? target with
      | some t =>
        rule (!inputs.contains c.name || t.consumer.name == a.generator.name)
          s!"{who}: a fn takes a container of {target}.{seq}, so generator {a.generator.name} must consume {target} (it is for {t.consumer.name})"
      | none => []
    | .generic _ => []
  let containers := i.containerRefs.flatMap fun (target, c) => match api? target with
    | none => [s!"{who}: ref to unknown api {repr target}"]
    | some t =>
      rule (a.linked t) s!"{who}: ref {target}.{c}: {target} shares no module with {a.name}" ++
      rule (target != a.name) s!"{who}: refs its own container {c}" ++
      match idl? target with
      | some ti => rule (ti.isGeneric c) s!"{who}: ref {target}.{c}: {target} owns no such generic container"
      | none => []
  let refs := i.refs.flatMap fun (target, ty, decl) => match api? target with
    | none => [s!"{who}: ref to unknown api {repr target}"]
    | some t =>
      rule (a.linked t) s!"{who}: ref {target}.{ty}: {target} shares no module with {a.name}" ++
      rule (target != a.name) s!"{who}: refs its own type {ty}" ++
      rule (!a.wire || t.wire) s!"{who}: crosses languages but refs {target}.{ty}, whose api does not" ++
      match idl? target with
      | none => []
      | some ti =>
        if !ti.declares ty then [s!"{who}: ref {target}.{ty}: {target} declares no such type"] else
        let has : List Adapter := match ti.types.find? (·.name == ty), ti.tokens.find? (·.name == ty) with
          | some d, _ => d.kind.adapters
          | none, some k => if k.store then [.store] else []
          | none, none => []
        let forwards := [(Adapter.display, "display"), (Adapter.store, "store")].flatMap fun (ad, flag) =>
          rule (!decl.kind.adapters.contains ad || has.contains ad)
            s!"{who}: type {decl.name}: {flag} forwards {target}.{ty}'s, which declares no {flag}"
        let kept := match ti.tokens.find? (·.name == ty) with
          | some k => match k.kind.failure? with
            | some c => (i.fnsMissing decl.name c).map fun f =>
              s!"{who}: fn {f.name}: takes {decl.name} ({target}.{ty}, {k.kind.word}), so {f.returns} must have a `{c}` constructor"
            | none => []
          | none => []
        forwards ++ kept
  consume ++ containers ++ refs

/-- APIs whose refs lead back to themselves: their generated modules would
be recursive. -/
def Manifest.refCycles (m : Manifest) : List String :=
  let targets (n : String) : List String :=
    match m.idls.find? (·.apiName == n) with
    | some i => i.refs.map (·.1) ++ i.containerRefs.map (·.1)
    | none => []
  let rec reach (fuel : Nat) (seen frontier : List String) : List String :=
    match fuel, frontier with
    | 0, _ | _, [] => seen
    | fuel + 1, n :: rest =>
      let new := (targets n).filter (!seen.contains ·)
      reach fuel (seen ++ new) (rest ++ new)
  m.apis.filterMap fun a =>
    let first := targets a.name
    if (reach (m.apis.length * m.apis.length + 1) first first).contains a.name
    then some s!"api {a.name}: ref cycle back to itself" else none

-- ---------------------------------------------------------------------------
-- Frames against the APIs their module sees (architect-v3 § Frames)
-- ---------------------------------------------------------------------------

/-- A resolved type of a frame signature. -/
inductive FrameTy
  | prim (word : String)
  | loc (name : String)
  | api (owner name : String)
  deriving Repr, DecidableEq, Inhabited

/-- `nutrition-db` as the Rust type `NutritionDb`: the default sum's name. -/
def pascal (s : String) : String :=
  String.join ((s.splitOn "-").flatMap (·.splitOn "_") |>.filter (!·.isEmpty) |>.map String.capitalize)

/-- `insertProduct` as the Rust fn `insert_product`: a frame fn provides an
API fn of this name. -/
def snake (s : String) : String :=
  String.ofList (s.toList.flatMap fun c => if c.isUpper then ['_', c.toLower] else [c])

namespace Idl

/-- Every sum a binding declares, with its constructors: the named ones,
then the default sum (`pascal api`). -/
def sumList (i : Idl) : List (String × List Ctor) :=
  i.sums.map (fun s => (s.name, s.ctors)) ++ (if i.ctors.isEmpty then [] else [(pascal i.apiName, i.ctors)])

/-- Every type name the binding declares; a generic container takes a type
argument, so only an instantiated one names a type. -/
def typeNames (i : Idl) : List String :=
  i.types.map (·.name) ++ i.tokens.map (·.name) ++ i.sumList.map (·.1) ++ i.elements.map (·.name) ++
  i.containers.filterMap fun c => match c.kind with | .ofElement .. => some c.name | _ => none

end Idl

/-- The APIs `module` sees: those it generates or consumes, and every API
their refs reach. -/
def Manifest.visibleApis (m : Manifest) (module : String) : List Api :=
  let direct := (m.apis.filter fun a => a.generator.name == module || a.consumer.name == module).map (·.name)
  let targets (n : String) : List String := match m.idls.find? (·.apiName == n) with
    | some i => i.refs.map (·.1) ++ i.containerRefs.map (·.1)
    | none => []
  let rec grow (fuel : Nat) (seen frontier : List String) : List String :=
    match fuel, frontier with
    | 0, _ | _, [] => seen
    | fuel + 1, n :: rest =>
      let new := (targets n).filter fun t => m.apis.any (·.name == t) && !seen.contains t
      grow fuel (seen ++ new) (rest ++ new)
  let names := grow (m.apis.length * m.apis.length + 1) direct direct
  m.apis.filter (names.contains ·.name)

/-- The rules of a frame that its module's APIs decide: every type name
resolves, local types are declared once and never contain themselves, each
call is a frame fn or a fn of an API the module consumes, `branch` names one
sum input, and a frame fn standing for an API fn has its signature. Rust's
naming (snake_case, names the skeleton takes) is the generator's to check. -/
def Manifest.frameProblems (m : Manifest) (f : Frame) : List String :=
  let mod := f.moduleName
  let who := s!"frame {mod}"
  let idl? (n : String) := m.idls.find? (·.apiName == n)
  let visible := (m.visibleApis mod).map (·.name)
  let locals := f.records.map (·.name) ++ f.sums.map (·.name)
  let prims := ["i64", "f64", "bool", "string", "bytes"]
  let declaredBy (n : String) : List String :=
    visible.filter fun a => (idl? a).any (·.typeNames.contains n)
  let resolve (n : String) : Except String FrameTy :=
    if prims.contains n then .ok (.prim n)
    else if locals.contains n then .ok (.loc n)
    else match n.splitOn "." with
      | [a, ty] =>
        if !visible.contains a then .error s!"{n}: module {mod} sees no api {a}"
        else if (idl? a).any (·.typeNames.contains ty) then .ok (.api a ty)
        else .error s!"{n}: {a} declares no type {ty}"
      | _ => match declaredBy n with
        | [a] => .ok (.api a n)
        | [] => .error s!"unknown type {n}: a prim (i64, f64, bool, string, bytes), a type of the frame, or of an api {mod} sees"
        | as => .error s!"{n} is declared by {" and ".intercalate as}: write api.{n}"
  let unresolved (n : String) : List String :=
    match resolve n with | .error e => [s!"{who}: {e}"] | .ok _ => []
  let isSum : FrameTy → Bool
    | .loc n => f.sums.any (·.name == n)
    | .api a n => (idl? a).any fun i => i.sumList.any (·.1 == n)
    | .prim _ => false
  let dup (xs : List String) (k : Nat) : Bool := (xs.take k).contains xs[k]!
  let declared :=
    ((List.range locals.length).filter (dup locals ·)).map (fun k => s!"{who}: type {locals[k]!} is declared twice") ++
    (let fs := f.fns.map (·.name)
     ((List.range fs.length).filter (dup fs ·)).map fun k => s!"{who}: fn {fs[k]!} is declared twice")
  let names :=
    f.records.flatMap (·.fields.flatMap unresolved) ++
    f.sums.flatMap (·.ctors.flatMap (·.args.flatMap unresolved)) ++
    f.fns.flatMap fun g => (g.inputs ++ [g.returns]).flatMap unresolved
  let calls := f.fns.flatMap fun g => g.callees.flatMap fun c =>
    if f.fns.any (·.name == c) then [] else
    match c.splitOn "." with
    | [a, fn] =>
      if !m.apis.any (fun x => x.name == a && x.consumer.name == mod) then [s!"{who}: fn {g.name} calls {c}: {mod} consumes no api {a}"]
      else rule ((idl? a).any (·.fns.any (·.name == fn))) s!"{who}: fn {g.name} calls {c}: {a} has no fn {fn}"
    | _ => [s!"{who}: fn {g.name} calls {c}: no frame fn {c} (an api fn is written api.fn)"]
  let branches := f.fns.flatMap fun g => match g.branchOn with
    | none => []
    | some b =>
      let r := resolve b
      let same := g.inputs.filter fun x => match resolve x, r with | .ok p, .ok q => p == q | _, _ => false
      if same.isEmpty then [s!"{who}: fn {g.name} branches on {b}, which is none of its inputs"]
      else if !(match r with | .ok t => isSum t | .error _ => false) then [s!"{who}: fn {g.name} branches on {b}, which is no sum"]
      else rule (same.length == 1) s!"{who}: fn {g.name} takes two {b}s: branch on one sum input"
  let parts (n : String) : List String :=
    ((f.records.find? (·.name == n)).map (·.fields) |>.getD []) ++
    ((f.sums.find? (·.name == n)).map (·.ctors.flatMap (·.args)) |>.getD []) |>.filter locals.contains
  let rec reaches (fuel : Nat) (seen frontier : List String) : List String :=
    match fuel, frontier with
    | 0, _ | _, [] => seen
    | fuel + 1, n :: rest =>
      let new := (parts n).filter (!seen.contains ·)
      reaches fuel (seen ++ new) (rest ++ new)
  let cycles := locals.filterMap fun l =>
    if (reaches (locals.length + 1) (parts l) (parts l)).contains l then some s!"{who}: type {l} contains itself" else none
  let provided := (m.apis.filter (·.generator.name == mod)).flatMap fun a => match idl? a.name with
    | none => []
    | some i =>
      let mine := i.fns.filterMap fun x => (f.fns.find? (·.name == snake x.name)).map (x, ·)
      if i.fns.isEmpty || mine.length < i.fns.length then [] else
      mine.filterMap fun (x, g) =>
        let want := (x.inputs ++ [x.returns]).map fun t => resolve s!"{a.name}.{t}"
        let got := (g.inputs ++ [g.returns]).map resolve
        let same := want.length == got.length && (want.zip got).all fun
          | (.ok p, .ok q) => p == q
          | _ => false
        if same then none
        else some s!"{who}: fn {g.name} differs from {a.name}'s {x.name}: {" → ".intercalate x.inputs} -> {x.returns}"
  declared ++ names ++ calls ++ branches ++ cycles ++ provided

-- ---------------------------------------------------------------------------
-- The manifest whole: the rules that read several declarations
-- ---------------------------------------------------------------------------

/-- Each name of `xs` that an earlier one already has. -/
def repeats (xs : List String) : List String :=
  (xs.foldl (fun (acc : List String × List String) x =>
    if acc.1.contains x then (acc.1, acc.2 ++ [x]) else (acc.1 ++ [x], acc.2)) ([], [])).2

/-- A cycle of the generator → consumer graph over `nodes`, closed (its
first node repeated last), if any. Kahn's peeling leaves exactly the nodes
with a predecessor among the rest; walking predecessors from one of them
revisits a node within `rest.length` steps. -/
def cycle (nodes : List String) (edges : List (String × String)) : Option (List String) :=
  let rec peel (fuel : Nat) (rest : List String) : List String :=
    match fuel with
    | 0 => rest
    | fuel + 1 =>
      let free := rest.filter fun n => !edges.any fun (g, c) => c == n && rest.contains g
      if free.isEmpty then rest else peel fuel (rest.filter (!free.contains ·))
  let rest := peel nodes.length nodes
  let pred (n : String) : String :=
    ((edges.find? fun (g, c) => c == n && rest.contains g).map (·.1)).getD n
  let rec walk (fuel : Nat) (path : List String) (n : String) : List String :=
    match fuel with
    | 0 => path
    | fuel + 1 =>
      match path.idxOf? n with
      | some i => (path.drop i ++ [n]).reverse
      | none => walk fuel (path ++ [n]) (pred n)
  match rest with
  | [] => none
  | n :: _ => some (walk (rest.length + 1) [] n)

/-- Every problem of the manifest: each declaration's own, then the rules
that read several. -/
def Manifest.problems (m : Manifest) : List String :=
  let modules := m.modules.map (·.name)
  let roots := m.roots.map (·.name)
  let components := modules ++ roots
  let apiPaths := m.apis.flatMap (·.paths)
  let owned := m.modules.flatMap (·.paths) ++ m.roots.flatMap (·.paths) ++ apiPaths
  let declared (who : String) (n : String) : List String :=
    rule (components.contains n) s!"{who}: {n} is a declared module or root"
  let own :=
    m.settings.problems ++ m.modules.flatMap Module.problems ++ m.roots.flatMap Root.problems ++
    m.apis.flatMap Api.problems ++ m.fixtures.flatMap Fixture.problems ++ m.checks.flatMap Check.problems
  let names :=
    (repeats components).map (s!"{·}: one module or root per name") ++
    (repeats (m.apis.map (·.name))).map (s!"api {·}: one api per name") ++
    (repeats (m.fixtures.map (·.name))).map (s!"fixture {·}: one fixture per name") ++
    (repeats (m.checks.map (·.name))).map (s!"check {·}: one check per name") ++
    (m.checks.filter fun c => modules.contains c.name).map
      (s!"check {·.name}: a module's implicit check has this name; name it apart")
  let refs :=
    m.apis.flatMap (fun a => declared s!"api {a.name}" a.generator.name ++ declared s!"api {a.name}" a.consumer.name) ++
    m.fixtures.flatMap (fun f => f.users.flatMap (declared s!"fixture {f.name}" ·.name)) ++
    m.checks.flatMap fun c => match c.covers with
      | .all => []
      | .only cs => cs.flatMap (declared s!"check {c.name}" ·.name)
  let pairs := m.apis.map fun a => (a.generator.name, a.consumer.name)
  let graph :=
    ((List.range m.apis.length).filterMap fun i =>
      let a := m.apis[i]!
      if (pairs.take i).contains (a.generator.name, a.consumer.name) then
        some s!"api {a.name}: a second api for {a.generator.name} → {a.consumer.name} (one api per connected pair)"
      else none) ++
    match cycle components pairs with
    | some c => [s!"the module graph has a cycle: {" → ".intercalate c} (generator → consumer is a DAG)"]
    | none => []
  let paths :=
    m.apis.flatMap (fun a => a.impl.filterMap fun i =>
      if under i apiPaths then some s!"api {a.name}: impl {repr i} lies outside every api's paths, which are pinned" else none) ++
    m.settings.linked.flatMap (fun l => match owned.find? (overlaps l ·) with
      | some p => [s!"architecture: linked {repr l} overlaps {repr p}: a link stays apart from module, root and api paths"]
      | none => []) ++
    m.fixtures.flatMap fun f =>
      let others :=
        m.modules.map (fun x => (s!"module {x.name}", x.paths)) ++ m.roots.map (fun x => (s!"root {x.name}", x.paths)) ++
        m.apis.map (fun x => (s!"api {x.name}", x.paths)) ++
        (m.fixtures.filter (·.name != f.name)).map (fun x => (s!"fixture {x.name}", x.paths)) ++
        [("shared", m.settings.shared), ("linked", m.settings.linked),
         ("test_paths", m.suite.test_paths)] ++
        m.checks.map (fun c => (s!"check {c.name}", c.files))
      f.paths.flatMap fun p => others.filterMap fun (who, qs) => (qs.find? (overlaps p ·)).map fun q =>
        s!"fixture {f.name}: path {repr p} overlaps {who}'s {repr q}: a fixture's files are its own"
  let totals :=
    rule (m.settings.max_modules == 0 || m.modules.length ≤ m.settings.max_modules)
      s!"modules: {m.modules.length} exceeds the total modules budget {m.settings.max_modules}" ++
    rule (m.settings.max_roots == 0 || m.roots.length ≤ m.settings.max_roots)
      s!"roots: {m.roots.length} exceeds the total roots budget {m.settings.max_roots}"
  let idls :=
    m.apis.flatMap (fun a => match (m.idls.filter (·.apiName == a.name)).length with
      | 0 => [s!"api {a.name}: an `idl {a.name}` block declares it (Apis/{a.name}.lean)"]
      | 1 => []
      | _ => [s!"api {a.name}: one `idl` block per api"]) ++
    (m.idls.filter fun i => !m.apis.any (·.name == i.apiName)).map
      (s!"Apis/{·.apiName}.lean: an idl names an api the manifest declares")
  let apiIdls :=
    m.idls.flatMap fun i => match m.apis.find? (·.name == i.apiName) with
      | some a => i.problems a ++ m.refProblems a i
      | none => []
  let frames :=
    m.frames.flatMap fun f => match m.modules.find? (·.name == f.moduleName) with
      | some x => f.placement x ++ m.frameProblems f
      | none => [s!"frame {f.moduleName}: a frame belongs to a declared module"]
  own ++ names ++ refs ++ graph ++ paths ++ totals ++ idls ++ apiIdls ++ m.refCycles ++ frames

/-- The manifest holds every structural rule. -/
def Manifest.wellformed (m : Manifest) : Bool := m.problems.isEmpty

end Dashboard
