/-
The exporter: walks the environment for every `Dashboard.Settings`,
`Module`, `Root`, `Api` and `Idl` declaration and writes one JSON document in
the shape `dashboard` reads (`config::ArchConfig` plus every `idl::Idl`).
Only the generated runner imports this (it imports `Lean`); API files don't.
-/
import Lean
import Dashboard

open Lean

namespace Dashboard

def Prim.repr : Prim → String
  | .text _ => "string"
  | .blob _ => "bytes"
  | .int64 => "i64"
  | .float64 => "f64"
  | .boolean => "bool"

def Prim.maxLen : Prim → Nat
  | .text n => n
  | .blob n => n
  | _ => 0

def Prim.toJson (p : Prim) : Json :=
  Json.mkObj [("repr", p.repr), ("max_len", p.maxLen)]

def Atom.toJson : Atom → Json
  | .v => "v"
  | .field i => Json.mkObj [("field", i)]
  | .len => "len"
  | .num t => Json.mkObj [("num", t)]
  | .str s => Json.mkObj [("str", s)]
  | .bool b => Json.mkObj [("bool", b)]

def Pred.toJson : Pred → Json
  | .cmp op l r => Json.mkObj [("cmp", Json.arr #[op, l.toJson, r.toJson])]
  | .mem a ls => Json.mkObj [("mem", Json.arr #[a.toJson, Json.arr (ls.toArray.map Atom.toJson)])]
  | .and p q => Json.mkObj [("and", Json.arr #[p.toJson, q.toJson])]
  | .or p q => Json.mkObj [("or", Json.arr #[p.toJson, q.toJson])]
  | .not p => Json.mkObj [("not", p.toJson)]

def TypeDecl.toJson (t : TypeDecl) : Json :=
  let kind : List (String × Json) := match t.kind with
    | .prim p => [("kind", Json.mkObj [("repr", p.repr)]), ("max_len", Lean.toJson p.maxLen)]
    | .refTo a ty => [("kind", Json.mkObj [("ref", Json.arr #[a, ty])]), ("max_len", Lean.toJson (0 : Nat))]
    | .tuple ps => [("kind", Json.mkObj [("tuple", Json.arr (ps.toArray.map Prim.toJson))]), ("max_len", Lean.toJson (0 : Nat))]
  let pred := match t.pred with | some p => p.toJson | none => Json.null
  Json.mkObj ([("name", Json.str t.name), ("display", Json.bool t.display), ("store", Json.bool t.store), ("pred", pred)] ++ kind)

def Token.toJson (t : Token) : Json :=
  let kind : Json := match t.kind with
    | .permanent => "permanent"
    | .versioned c => Json.mkObj [("versioned", c)]
    | .linear c => Json.mkObj [("linear", c)]
  Json.mkObj [("name", t.name), ("repr", t.prim.repr), ("max_len", t.prim.maxLen), ("kind", kind), ("store", t.store)]

def Ctor.toJson (c : Ctor) : Json :=
  Json.mkObj [("name", c.name), ("args", Json.arr (c.args.toArray.map Json.str))]

def Element.toJson (e : Element) : Json :=
  Json.mkObj [("name", e.name), ("variants", Json.arr (e.variants.toArray.map fun v =>
    Json.mkObj [("name", v.name), ("fields", Json.arr (v.fields.toArray.map Prim.toJson))]))]

def Container.toJson (c : Container) : Json :=
  let kind := match c.kind with
    | .generic cs => Json.mkObj [("generic", Json.arr (cs.toArray.map fun k => Json.str k.toString))]
    | .ofElement e a k => Json.mkObj [("of", Json.arr #[e, a, k])]
  Json.mkObj [("name", c.name), ("kind", kind)]

def Instance.toJson (i : Instance) : Json :=
  Json.mkObj [("class", i.cls.toString), ("api", i.owner), ("container", i.seq)]

def Sum.toJson (s : Sum) : Json :=
  Json.mkObj [("name", s.name), ("ctors", Json.arr (s.ctors.toArray.map Ctor.toJson))]

def Fn.toJson (f : Fn) : Json :=
  Json.mkObj [("name", f.name), ("inputs", Json.arr (f.inputs.toArray.map Json.str)), ("returns", f.returns)]

def Idl.toJson (i : Idl) : Json :=
  Json.mkObj [
    ("api", i.apiName),
    ("types", Json.arr (i.types.toArray.map TypeDecl.toJson)),
    ("tokens", Json.arr (i.tokens.toArray.map Token.toJson)),
    ("ctors", Json.arr (i.ctors.toArray.map Ctor.toJson)),
    ("elements", Json.arr (i.elements.toArray.map Element.toJson)),
    ("containers", Json.arr (i.containers.toArray.map Container.toJson)),
    ("instances", Json.arr (i.instances.toArray.map Instance.toJson)),
    ("sums", Json.arr (i.sums.toArray.map Sum.toJson)),
    ("fns", Json.arr (i.fns.toArray.map Fn.toJson))]

def strs (xs : List String) : Json := Json.arr (xs.toArray.map Json.str)

def opt (s : Option String) : Json := match s with | some s => s | none => Json.null

def Module.toJson (m : Module) : Json :=
  Json.mkObj [("name", m.name), ("lang", m.lang.toString), ("paths", strs m.paths),
    ("max_ast", m.max_ast), ("max_bytes", m.max_bytes), ("test_cmd", opt m.test_cmd), ("lint_cmd", opt m.lint_cmd)]

def Root.toJson (r : Root) : Json :=
  Json.mkObj [("name", r.name), ("lang", r.lang.toString), ("paths", strs r.paths),
    ("max_ast", r.max_ast), ("max_bytes", r.max_bytes)]

def Api.toJson (a : Api) : Json :=
  Json.mkObj [("name", a.name), ("generator", a.generator.name), ("consumer", a.consumer.name),
    ("paths", strs a.paths), ("impl", strs a.impl),
    ("max_constructors", a.max_constructors), ("max_types", a.max_types),
    ("max_elements", a.max_elements), ("max_fields", a.max_fields),
    ("max_fns", a.max_fns), ("max_inputs", a.max_inputs),
    ("max_string_len", a.max_string_len), ("max_bytes_len", a.max_bytes_len),
    ("max_rules", a.max_rules), ("max_tokens", a.max_tokens)]

/-- `module` is the Lean module that declared the check as a path
(`Tests`, `Tests/x.worker`): `dashboard` maps it back to the file. -/
def Check.toJson (c : Check) (src : String) : Json :=
  Json.mkObj [("name", c.name), ("cmd", c.cmd), ("runs", c.runs), ("min_pass", c.min_pass),
    ("retry_failed", c.retry_failed), ("retry_on", strs c.retry_on),
    ("covers", strs (c.covers.map Node.name)), ("all", c.all), ("files", strs c.files),
    ("module", src)]

def Suite.toJson (s : Suite) : Json :=
  Json.mkObj [("test_paths", strs s.test_paths), ("list_cmd", opt s.list_cmd)]

/-- A declaration's place: its module (in import order) and source position. -/
structure Place where
  modIdx : Nat
  line : Nat
  col : Nat
  name : Name

def Place.lt (a b : Place) : Bool :=
  a.modIdx < b.modIdx || (a.modIdx == b.modIdx && (a.line < b.line || (a.line == b.line && a.col < b.col)))

/-- The constants of type `ty` (a `Dashboard` structure) in declaration
order: imported modules in import order, then source position. -/
def declsOf (ty : Name) : CoreM (Array Name) := do
  let env ← getEnv
  let mut out : Array Place := #[]
  for (n, c) in env.constants.toList do
    if c.type.isConstOf ty && !n.isInternal && !(`Dashboard).isPrefixOf n then
      let modIdx := ((env.getModuleIdxFor? n).map (·.toNat)).getD 0
      let (line, col) := match ← findDeclarationRanges? n with
        | some r => (r.range.pos.line, r.range.pos.column)
        | none => (0, 0)
      out := out.push { modIdx, line, col, name := n }
  return (out.qsort Place.lt).map (·.name)

unsafe def evalDecl (α : Type) [Inhabited α] (n : Name) : CoreM α := do
  let env ← getEnv
  match env.evalConst α {} n with
  | .ok v => pure v
  | .error e => throwError "{n}: {e}"

@[implemented_by evalDecl]
opaque evalDeclSafe (α : Type) [Inhabited α] (n : Name) : CoreM α

/-- Every declaration, as the JSON `dashboard` loads. -/
def exportAll : CoreM Json := do
  let env ← getEnv
  let settings ← declsOf ``Settings
  let mut errors : Array String := #[]
  let base ← match settings with
    | #[n] => evalDeclSafe Settings n
    | #[] => pure {}
    | ns => do
      errors := errors.push s!"more than one `architecture where` block: {ns}"
      pure {}
  let mut modules := #[]
  for n in ← declsOf ``Module do
    modules := modules.push (Module.toJson (← evalDeclSafe Module n))
  let mut roots := #[]
  for n in ← declsOf ``Root do
    roots := roots.push (Root.toJson (← evalDeclSafe Root n))
  let mut apis := #[]
  for n in ← declsOf ``Api do
    apis := apis.push (Api.toJson (← evalDeclSafe Api n))
  let mut idls : Array (String × Json) := #[]
  for n in ← declsOf ``Idl do
    let i ← evalDeclSafe Idl n
    if idls.any (·.1 == i.apiName) then
      errors := errors.push s!"api {i.apiName}: more than one `idl` block"
    idls := idls.push (i.apiName, i.toJson)
  let mut checks := #[]
  for n in ← declsOf ``Check do
    let src := match env.getModuleIdxFor? n with
      | some i => (env.header.moduleNames[i.toNat]?.map fun m =>
          "/".intercalate (m.components.map (·.toString (escape := false)))).getD ""
      | none => ""
    checks := checks.push (Check.toJson (← evalDeclSafe Check n) src)
  let suiteV ← match ← declsOf ``Suite with
    | #[n] => evalDeclSafe Suite n
    | #[] => pure {}
    | ns => do
      errors := errors.push s!"more than one `suite where` block: {ns}"
      pure {}
  let arch := Json.mkObj [("max_modules", base.max_modules), ("max_roots", base.max_roots),
    ("shared", strs base.shared), ("linked", strs base.linked), ("source_paths", strs base.source_paths),
    ("module", Json.arr modules), ("root", Json.arr roots), ("api", Json.arr apis)]
  return Json.mkObj [("arch", arch), ("idls", Json.mkObj idls.toList),
    ("suite", suiteV.toJson), ("checks", Json.arr checks),
    ("errors", Json.arr (errors.map Json.str))]

/-- Writes the export to `path`: what the generated runner calls. -/
def exportTo (path : String) : CoreM Unit := do
  let j ← exportAll
  IO.FS.writeFile path (j.compress ++ "\n")

/-- Writes the run set of `nodes` as a JSON array of check names: every
declared check plus each module's implicit `test_cmd` check, through
`impacted`. -/
def impactedTo (path : String) (nodes : List Node) : CoreM Unit := do
  let mut checks : Array Check := #[]
  for n in ← declsOf ``Module do
    let m ← evalDeclSafe Module n
    if let some cmd := m.test_cmd then
      checks := checks.push { name := m.name, cmd, covers := [m] }
  for n in ← declsOf ``Check do
    checks := checks.push (← evalDeclSafe Check n)
  let names := (impacted checks.toList nodes).map (·.name)
  IO.FS.writeFile path ((strs names).compress ++ "\n")

end Dashboard
