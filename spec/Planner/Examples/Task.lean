/-
Tasks as `tasks/<slug>/Task.lean` writes them: it imports `Architecture`
to name modules, so the architecture's syntax is open and its words are
tokens. `#writable` fails when a field or constructor argument of the
named types is spelled like a token; one example per kind writes every
field by its name.
-/
import Lean
import Architect
import Planner.Types

namespace Spec.Examples.Task
open Lean Elab Command Dashboard

/-- The field names of a structure, or the argument names of an
inductive's constructors. -/
def fieldNames (env : Environment) (n : Name) : MetaM (List String) := do
  if isStructure env n then
    return (getStructureFields env n).toList.map (·.toString)
  match env.find? n with
  | some (.inductInfo i) =>
    i.ctors.flatMapM fun c => do
      let some ci := env.find? c | return []
      Meta.forallTelescope ci.type fun xs _ => xs.toList.mapM fun x => return (← x.fvarId!.getUserName).toString
  | _ => return []

elab "#writable " ns:ident* : command => do
  let env ← getEnv
  let tokens := Lean.Parser.getTokenTable env
  for n in ns do
    let name ← liftCoreM (realizeGlobalConstNoOverload n)
    let fs ← liftTermElabM (fieldNames env name)
    let bad := fs.filter fun f => (tokens.find? f).isSome
    unless bad.isEmpty do
      throwErrorAt n m!"{name}: {bad} are tokens here; a director writes them only as «…»"

#writable Header Replace Evidence Dashboard.Task

#writable StrikeLimits

/-- error: Dashboard.Token: [store] are tokens here; a director writes them only as «…» -/
#guard_msgs in
#writable Token

module core where
  lang = rust

def stable : Check :=
  { name := "stable", cmd := "cargo test x", covers := [core], runs := 20, min_pass := 20 }

def header (s : Slug) : Header :=
  { slug := s, title := "Title", phase := "p1", milestone := false, dependsOn := ["alpha"],
    components := [core], brief := "Context for the task agent." }

def task : Task := .task (header "beta")
def spike : Task := .spike { header "probe" with dependsOn := [] }
def flakyFix : Task :=
  .flakyFix (h := header "fix-x") (stable := stable) (fix_paths := ["tests/x.rs"])
    (replace := [{ old := "x", new := "x2" }])
    (evidence := [{ name := "x", cmd := "cargo test x", runs := 20, min_pass := 1, max_pass := 15 }])
def speedup : Task := .speedup (h := header "faster") (slow := ["stable"])

end Spec.Examples.Task
