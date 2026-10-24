import Agents.Edge.PlayerWorker
import Agents.Edge.PlayerForeman
import Agents.Edge.PlannerPlayer
import Agents.Edge.ArchitectPlayer
import Agents.Edge.PlannerArchitect

/-!
The composition of one task: one run of the player's task, the spine, and
one edge run per peer over that same run. It holds no state of its own.
A step the spine shares with several peers (the player's `clean` lands
the planner, accepts the foreman and answers the worker) is one event in
every edge, by its type.

The design loop composes the same way: `ArchitectPlayer.EdgeRun` over the
player's review, and `PlannerArchitect.EdgeRun` over the planner's run,
one per need.
-/
namespace Loop
open Kernel Player

/-- One task's loop: the planner present from the start, as `running g p`
for the task's plan; the foreman and the worker absent until the player
starts them. -/
structure Loop (lim : Limits) (failing : Revision → List String) {s t : Task}
    (r : Run (Step lim failing) s t) where
  start : s.isStart = true
  goal : Planner.Goal
  plannerEnd : Planner.State
  planner : PlannerPlayer.EdgeRun lim failing r (.running goal s.plan) plannerEnd
  foremanEnd : Option Foreman.State
  foreman : PlayerForeman.EdgeRun lim failing r none foremanEnd
  workerEnd : Option Worker.State
  worker : PlayerWorker.EdgeRun lim failing r none workerEnd

-- An accepted task, start to land ---------------------------------------------

namespace Example

/-- The run set fails at the post and at the first merge. -/
def failing (rev : Revision) : List String :=
  if rev = "post" ∨ rev = "m1" then ["t"] else []

def lim : Limits := {}
def p : Plan := { slug := ⟨"a"⟩ }

theorem post_fails : failing "post" ≠ [] := by decide
theorem sub_passes : failing "sub" = [] := by decide
theorem m1_breaks : failing "m1" ≠ [] := by decide
theorem m2_passes : failing "m2" = [] := by decide
theorem strike_left : ({} : Spent).suite + 1 < lim.suiteStrikes := by decide

/-- Assign, post, launch, submit, recommend, accept, land. -/
def run : Run (Step lim failing) (.ordered (.start p) {} none) (.landed p) :=
  .next .assign <| .next (.posted "post" post_fails) <| .next .launch <|
  .next (.submitted "sub" sub_passes) <| .next (.recommended []) <| .next (.accept "m2") <|
  .next (.clean m2_passes) .stay

def loop : Loop lim failing run where
  start := rfl
  goal := .fromOperator "ship a"
  plannerEnd := .landed p
  planner :=
    .next (.holds rfl) <| .next (.holds rfl) <| .next (.holds rfl) <| .next (.holds rfl) <|
    .next (.recommended []) <| .next (.accept "m2") <| .next (.clean m2_passes) .stay
  foremanEnd := some .accepted
  foreman :=
    .next .start <| .next (.posted "post" post_fails) <| .next (.holds rfl) <|
    .next (.askReview "sub" sub_passes) <| .next (.recommended []) <| .next (.holds rfl) <|
    .next (.clean m2_passes) .stay
  workerEnd := some .accepted
  worker :=
    .next (.holds rfl) <| .next (.holds rfl) <| .next (.launch rfl) <|
    .next (.submit "sub" sub_passes) <| .next (.holds rfl) <| .next (.holds rfl) <|
    .next (.clean m2_passes) .stay

/-- The spent counters after the broken merge's strike. -/
def c1 : Spent := { suite := 1 }

/-- The first merge breaks the run set: a suite strike, the planner's plan
runs again, the worker submits again, and the second merge lands. -/
def broken : Run (Step lim failing) (.ordered (.start p) {} none) (.landed p) :=
  .next .assign <| .next (.posted "post" post_fails) <| .next .launch <|
  .next (.submitted "sub" sub_passes) <| .next (.recommended []) <| .next (.accept "m1") <|
  .next (.broken m1_breaks strike_left) <|
  .next (.submitted (c := c1) "sub" sub_passes) <| .next (.recommended []) <|
  .next (.accept "m2") <| .next (.clean m2_passes) .stay

def brokenLoop : Loop lim failing broken where
  start := rfl
  goal := .fromOperator "ship a"
  plannerEnd := .landed p
  planner :=
    .next (.holds rfl) <| .next (.holds rfl) <| .next (.holds rfl) <| .next (.holds rfl) <|
    .next (.recommended []) <| .next (.accept "m1") <| .next (.broken m1_breaks strike_left) <|
    .next (.holds rfl) <| .next (.recommended []) <| .next (.accept "m2") <|
    .next (.clean m2_passes) .stay
  foremanEnd := some .accepted
  foreman :=
    .next .start <| .next (.posted "post" post_fails) <| .next (.holds rfl) <|
    .next (.askReview "sub" sub_passes) <| .next (.recommended []) <| .next (.holds rfl) <|
    .next (.holds rfl) <| .next (.askReview "sub" sub_passes) <| .next (.recommended []) <|
    .next (.holds rfl) <| .next (.clean m2_passes) .stay
  workerEnd := some .accepted
  worker :=
    .next (.holds rfl) <| .next (.holds rfl) <| .next (.launch rfl) <|
    .next (.submit "sub" sub_passes) <| .next (.holds rfl) <| .next (.holds rfl) <|
    .next (.broken m1_breaks strike_left) <| .next (.submit "sub" sub_passes) <|
    .next (.holds rfl) <| .next (.holds rfl) <| .next (.clean m2_passes) .stay

end Example

end Loop
