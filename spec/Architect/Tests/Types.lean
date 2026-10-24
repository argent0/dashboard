/-
The suite's data (spec §6, architect-v3 § Tests): the types `Tests.lean`
and `Tests/*.lean` declare and the exporter writes, and `impacted`, the
reference run set.

Vendored as `architecture/Architect/Tests/Types.lean`. Plain Lean with no
`syntax`, like `Architect.Types`.
-/
import Architect.Types

namespace Dashboard


/-- What a check tests. `only cs` names modules and roots: the check runs
in a view of their files and joins the run set of a task on any of them.
`all` (written `covers = all`) runs it in the whole worktree for every task. -/
inductive Covers
  | all
  | only (cs : List Component)
  deriving Repr, DecidableEq, Inhabited

/-- A suite check. `files` are its own test files, beyond `test_paths`. -/
structure Check where
  name : String
  cmd : String
  runs : Nat := 1
  min_pass : Nat := 1
  retry_failed : Nat := 0
  retry_on : List String := []
  covers : Covers := .only []
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
  checks.filter fun c => match c.covers with
    | .all => true
    | .only cs => cs.any fun n => components.any (·.name == n.name)

end Dashboard
