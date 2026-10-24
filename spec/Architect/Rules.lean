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
with a budget, covers something, and owns relative files. `retry_on`'s
patterns are regexes, `dashboard`'s to read. -/
def Check.problems (c : Check) : List String :=
  let who := s!"check {c.name}"
  rule (c.runs ≥ 1) s!"{who}: runs is at least 1 (got {c.runs})" ++
  rule (1 ≤ c.min_pass && c.min_pass ≤ c.runs) s!"{who}: min_pass lies between 1 and runs ({c.runs}), got {c.min_pass}" ++
  rule (c.retry_on.isEmpty || c.retry_failed ≥ 1) s!"{who}: retry_on needs retry_failed ≥ 1" ++
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
  let frames :=
    m.frames.flatMap fun f => match m.modules.find? (·.name == f.moduleName) with
      | some x => f.placement x
      | none => [s!"frame {f.moduleName}: a frame belongs to a declared module"]
  own ++ names ++ refs ++ graph ++ paths ++ totals ++ idls ++ frames

/-- The manifest holds every structural rule. -/
def Manifest.wellformed (m : Manifest) : Bool := m.problems.isEmpty

end Dashboard
