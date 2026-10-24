/-
The plan's data (spec §8.2, §10, §14, §15): the types a project's tasks
package writes, `tasks/Roadmap.lean` (phases, limits, the player's settings)
and `tasks/<slug>/Task.lean` (one task). It names the architecture's nodes,
so it imports `Architect.Types` (and `Architect.Tests.Types` for a flaky
fix's check); it imports no syntax, so
`tasks/Roadmap.lean`, importing it alone, writes every field by its name
(`strikes := { suite := 3 }`).

Vendored as `architecture/Planner/Types.lean`.
-/
import Architect.Types
import Architect.Tests.Types

namespace Dashboard

-- ---------------------------------------------------------------------------
-- The tasks package (spec §8.2, §10, §14, §15): `tasks/Roadmap.lean` and
-- `tasks/<slug>/Task.lean`
-- ---------------------------------------------------------------------------

/-- A task's name, fixed at branch creation and never reused (spec §8.1).
Its spelling is the decoder's concern; the model compares slugs only. -/
structure Slug where
  name : String
  deriving Repr, DecidableEq, Inhabited

instance : Coe String Slug := ⟨Slug.mk⟩

/-- A phase of the roadmap. -/
structure Phase where
  id : String
  title : Option String := none
  goal : Option String := none
  deriving Repr, DecidableEq, Inhabited

/-- `[limits.strikes]` (spec §10): one counter per rejection reason. -/
structure StrikeLimits where
  history : Nat := 2
  ownership : Nat := 2
  pins : Nat := 2
  scope : Nat := 2
  suite : Nat := 2
  plan : Nat := 2
  post : Nat := 2
  review : Nat := 2
  accept : Nat := 2
  retire : Nat := 2
  claims : Nat := 2
  /-- A flaky-test-fix post whose evidence shows no flake: the first miss
  retires the task. -/
  evidence : Nat := 1
  /-- A suite past its `[budget]`. -/
  budget : Nat := 2
  deriving Repr, DecidableEq, Inhabited

/-- `[limits]` (spec §10, §17, §18): send-backs, contests and relaunches
beyond these escalate. -/
structure Limits where
  max_review_rounds : Nat := 2
  max_contest_rounds : Nat := 2
  max_relaunches : Nat := 2
  strikes : StrikeLimits := {}
  deriving Repr, DecidableEq, Inhabited

/-- `[budget]`: seconds the project suite may take; 0 is off. -/
structure Budget where
  check_timeout : Nat := 0
  suite_warn : Nat := 0
  suite_fail : Nat := 0
  deriving Repr, DecidableEq, Inhabited

/-- Whether a jailed check reaches the network. -/
inductive Network
  | offline
  | online
  deriving Repr, DecidableEq, Inhabited

/-- `[sandbox]` (spec §20): where every check runs. `jail` runs it in the
jail, with `writable` the paths it may write beyond its worktree; `off` runs
it on the host. -/
inductive Sandbox
  | off
  | jail (network : Network := .offline) (writable : List String := [])
  deriving Repr, DecidableEq, Inhabited

/-- `[player] runs` (spec §4). -/
inductive Runs
  | sequential
  | parallel
  | queued
  deriving Repr, DecidableEq, Inhabited

/-- `[player]`: the commands that launch sessions and the lock files a task
agent may commit. -/
structure Player where
  agent_cmd : String := "claude --dangerously-skip-permissions"
  task_agent_cmd : String := "claude --dangerously-skip-permissions"
  idle_after : Nat := 900
  lock_files : List String := []
  lock_regen : List (String × String) := []
  runs : Runs := .sequential
  deriving Repr, DecidableEq, Inhabited

/-- `tasks/Roadmap.lean`: the roadmap's settings, on `main` only. -/
structure Roadmap where
  title : Option String := none
  phases : List Phase := []
  docs_paths : List String := []
  limits : Limits := {}
  player : Player := {}
  sandbox : Sandbox := .jail
  budget : Budget := {}
  deriving Repr, DecidableEq, Inhabited

/-- What every task declares, whatever its kind. `components` names the modules
and roots the task changes: its worker's scope and its run set (spec §6.6).
`dependsOn` names slugs: a dependency's declaration lives on its own branch
until it is accepted. `brief` is pasted into the task agent's prompt (§17). -/
structure Header where
  slug : Slug
  title : String
  phase : String
  milestone : Bool := false
  dependsOn : List Slug := []
  components : List Component := []
  brief : String := ""
  deriving Repr, DecidableEq, Inhabited

/-- One replacement of a flaky-test-fix: the flaky test and its stable twin. -/
structure Replace where
  old : String
  new : String
  deriving Repr, DecidableEq, Inhabited

/-- Evidence of a flake on `main` (spec §15): of `runs` runs of `cmd`,
between `min_pass` and `max_pass` pass. -/
structure Evidence where
  name : String
  cmd : String
  runs : Nat := 1
  min_pass : Nat := 1
  max_pass : Nat
  deriving Repr, DecidableEq, Inhabited

/-- A task declaration, one constructor per kind (spec §8.2, §14, §15, §15.1).
A kind's own fields live only on its constructor. -/
inductive Task
  /-- Ordinary code that merges into `main`; its task check is the checks its
  post adds to `architecture/Tests/<slug>.lean`. -/
  | task (h : Header)
  /-- An answer to a pinned question, backed by claims; never merges (§14). -/
  | spike (h : Header)
  /-- Replaces a flaky test with a stable one (§15): `fix_paths` are unpinned,
  `check` is the check proving the replacement stable, `evidence` proves
  the flake. -/
  | flakyFix (h : Header) (check : Check) (fix_paths : List String)
      (replace : List Replace) (evidence : List Evidence)
  /-- Makes the `slow` suite checks faster (§15.1). -/
  | speedup (h : Header) (slow : List String)
  deriving Repr, DecidableEq, Inhabited

def Task.header : Task → Header
  | .task h | .spike h | .flakyFix h .. | .speedup h _ => h

end Dashboard
