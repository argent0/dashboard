import Agents.Kernel

/-!
The planner's work. It starts with something to plan: a goal, or a
dropped task to replan. It may ask the architect for a design first, then
hands the player a plan, and decides on what the player reports back.
-/
namespace Planner
open Kernel

/-- Something to plan. -/
inductive Goal
  | fromOperator (text : String)
  /-- A task that was dropped for `cause`, to plan again. The planner starts
  this machine from a `dropped` end it chooses to replan (`State.replan`);
  the operator may hand it one too. -/
  | replan (slug : Slug) (cause : Cause)

/-- What the player reports about a running plan. -/
inductive Advice
  /-- The foreman recommends acceptance; `trim` names the task checks it
  would drop from the suite (spec §6.5). -/
  | recommend (trim : List String)
  /-- The foreman is unsure. -/
  | doubt (why : String)
  /-- The task stopped. -/
  | stopped (c : Cause)
  /-- Landing it hit a merge conflict on `paths`. -/
  | conflict (paths : List String)

/-- The cause a drop on this advice records: a stop keeps its own. -/
def Advice.cause : Advice → Cause
  | .stopped c => c
  | _ => .retired

/-- The planner's possible decisions on each advice. -/
inductive Decision : Advice → Type
  | accept {t} : Decision (.recommend t)
  /-- Commit `rev`, which drops the checks of `t` (`dashboard trim`, spec
  §6.5); the player runs the run set on it, and acceptance waits again. -/
  | trim {t} (rev : Revision) (h : t ≠ []) : Decision (.recommend t)
  /-- Continue with a revised plan. -/
  | continue {w} (p : Plan) : Decision (.doubt w)
  | resolve {paths} (merge : Revision) : Decision (.conflict paths)
  | drop {a} (why : String) : Decision a

/-- Advice the player reports from the work; a conflict comes from landing. -/
def Advice.isReport : Advice → Bool
  | .conflict _ => false
  | _ => true

inductive State
  /-- Something to plan. The start. -/
  | toPlan (g : Goal)
  /-- Asked the architect for `what`. -/
  | designing (g : Goal) (what : String)
  /-- The player runs plan `p`. -/
  | running (g : Goal) (p : Plan)
  /-- Advice `a` waits for a decision. -/
  | advised (g : Goal) (p : Plan) (a : Advice)
  /-- Accepted or resolved; the player lands it. -/
  | landing (g : Goal) (p : Plan)
  /-- `p` is on `main`. An end. -/
  | landed (p : Plan)
  /-- `p` was dropped for `c`, with why. An end. -/
  | dropped (g : Goal) (p : Plan) (c : Cause) (why : String)
  /-- Given up before any plan ran, with why. An end. -/
  | abandoned (g : Goal) (why : String)

def after (g : Goal) (p : Plan) : (a : Advice) → Decision a → State
  | _, .accept => .landing g p
  | _, .trim .. => .advised g p (.recommend [])
  | _, .continue p' => .running g p'
  | _, .resolve _ => .landing g p
  | a, .drop why => .dropped g p a.cause why

/-- The planner's legal transitions. -/
inductive Step : State → State → Type
  | need {g} (what : String) : Step (.toPlan g) (.designing g what)
  /-- The architect answered: designed, or declined. -/
  | designed {g w} (rev : Revision) : Step (.designing g w) (.toPlan g)
  | declined {g w} (why : String) : Step (.designing g w) (.toPlan g)
  | plan {g} (p : Plan) : Step (.toPlan g) (.running g p)
  | abandon {g} (why : String) : Step (.toPlan g) (.abandoned g why)
  | report {g p} (a : Advice) (h : a.isReport = true) : Step (.running g p) (.advised g p a)
  | conflict {g p} (paths : List String) : Step (.landing g p) (.advised g p (.conflict paths))
  | decide {g p a} (d : Decision a) : Step (.advised g p a) (after g p a d)
  | land {g p} : Step (.landing g p) (.landed p)
  /-- The player's merge broke the run set: the work goes on. -/
  | reopened {g p} : Step (.landing g p) (.running g p)

/-- The need a step names to the architect, if any. -/
def Step.need? : Step s t → Option String
  | .need what => some what
  | .designed (w := w) _ | .declined (w := w) _ => some w
  | _ => none

def State.isStart : State → Bool
  | .toPlan _ => true
  | _ => false

/-- What a dropped end hands a new planner machine to start from. -/
def State.replan : State → Option Goal
  | .dropped _ p c _ => some (.replan p.slug c)
  | _ => none

/-- A decision always moves on: no advice is decided twice. -/
theorem decision_moves_on (g : Goal) (p : Plan) (a : Advice) (d : Decision a) :
    after g p a d ≠ .advised g p a := by
  cases d with
  | trim rev h => intro e; simp [after] at e; exact h e
  | _ => simp [after]

/-- A stop's cause reaches the dropped end, and so the replan. -/
theorem drop_keeps_cause (g : Goal) (p : Plan) (c : Cause) (why : String) :
    (after g p (.stopped c) (.drop why)).replan = some (.replan p.slug c) := rfl

theorem landed_is_final : Step (.landed p) t → False := nofun
theorem dropped_is_final : Step (.dropped g p c w) t → False := nofun
theorem abandoned_is_final : Step (.abandoned g w) t → False := nofun

end Planner
