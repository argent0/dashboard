/-
The suite's syntax (spec §6): `check` and `suite` … `where`, the commands
of `Tests.lean` and `Tests/*.lean`, elaborating to `Architect.Tests.Types`.
Its words are tokens only where `Architect.Tests` is imported, so
`Architecture.lean`, the APIs and a task's `Task.lean` keep `check` and
`suite` free as names.

Vendored as `architecture/Architect/Tests/Syntax.lean`.
-/
import Architect.Syntax
import Architect.Tests.Types

namespace Dashboard

syntax (name := archCheck) "check " ident " where" manyIndent(group(ident " = " term)) : command
syntax (name := archSuite) "suite" " where" manyIndent(group(ident " = " term)) : command

/-- `covers = all` is `Covers.all`; `covers = [a, b]` is `Covers.only` and coerces each element to a
`Component`, so modules and roots mix. A check named like a module is a
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
        ks := ks.push key
        vs := vs.push (← `(Dashboard.Covers.all))
      else
        match val with
        | `([$xs,*]) =>
          let elems ← xs.getElems.mapM fun x => `(($x : Dashboard.Component))
          ks := ks.push key
          vs := vs.push (← `(Dashboard.Covers.only [$elems,*]))
        | _ => Lean.Macro.throwErrorAt val "covers is `all` or a list of modules and roots: covers = [a, b]"
    `(def $n : Dashboard.Check := { name := $s, $[$ks:ident := $vs],* })

macro_rules
  | `(suite where $[$k:ident = $v:term]*) => do
    let id := Lean.mkIdent `suite
    `(def $id : Dashboard.Suite := { $[$k:ident := $v:term],* })

end Dashboard
