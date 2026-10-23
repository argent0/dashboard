/-
The suite's data (spec §6, architect-v3 § Tests): the types `Tests.lean`
and `Tests/*.lean` declare and the exporter writes, and `impacted`, the
reference run set.

Vendored as `architecture/Architect/Tests/Types.lean`. Plain Lean with no
`syntax`, like `Architect.Types`.
-/
import Architect.Types

namespace Dashboard


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
  covers : List Component := []
  all : Bool := false
  files : List String := []
  deriving Repr, DecidableEq, Inhabited

/-- The `suite where` block of `Tests.lean`. -/
structure Suite where
  test_paths : List String := []
  list_cmd : Option String := none
  deriving Repr, DecidableEq, Inhabited

/-- The run set of a task on `components` (spec §6.6): every check covering one
of them, plus every `covers = all` check. The reference definition. -/
def impacted (checks : List Check) (components : List Component) : List Check :=
  checks.filter fun c => c.all || c.covers.any fun n => components.any (·.name == n.name)

end Dashboard
