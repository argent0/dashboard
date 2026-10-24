import Agents.Kernel

/-!
The architect's work. It designs the architecture on `main`: components,
APIs, frames, fixtures and checks. It starts with something to design, a
goal from the operator or a need a planner names, proposes a revision of
`main`, and the player judges it. `maxProposals` is how many proposals the
player judges per question: past it, the last refusal ends the question.
-/
namespace Architect
open Kernel

/-- Something to design. -/
inductive Question
  /-- A goal from the operator. -/
  | goal (text : String)
  /-- A need a planner names while planning. -/
  | need (what : String)

inductive State
  /-- Something to design, with `n` proposals refused so far. The start is
  `n = 0`. -/
  | asked (q : Question) (n : Nat)
  /-- Proposed `rev` as the answer; the player judges it. -/
  | proposing (q : Question) (n : Nat) (rev : Revision)
  /-- `rev` is on `main`. An end. -/
  | designed (q : Question) (rev : Revision)
  /-- Declined, with why. An end. -/
  | declined (q : Question) (why : String)
  /-- The player refused the last proposal it judges, for `r`. An end. -/
  | refused (q : Question) (r : Reason)

variable (maxProposals : Nat)

/-- The architect's legal transitions. -/
inductive Step : State → State → Type
  | propose {q n} (rev : Revision) : Step (.asked q n) (.proposing q n rev)
  | accepted {q n rev} : Step (.proposing q n rev) (.designed q rev)
  /-- Refused, with proposals left: design again. -/
  | refusedAgain {q n rev} (r : Reason) (h : n + 1 < maxProposals) :
      Step (.proposing q n rev) (.asked q (n + 1))
  /-- Refused, with no proposal left. -/
  | refusedLast {q n rev} (r : Reason) (h : maxProposals ≤ n + 1) :
      Step (.proposing q n rev) (.refused q r)
  | decline {q n} (why : String) : Step (.asked q n) (.declined q why)

def State.isStart : State → Bool
  | .asked _ 0 => true
  | _ => false

def State.question : State → Question
  | .asked q _ | .proposing q .. | .designed q _ | .declined q _ | .refused q _ => q

/-- The architect answers the question it started with, and no other. -/
theorem step_keeps_question (st : Step maxProposals s t) : t.question = s.question := by
  cases st <;> rfl

theorem run_keeps_question (r : Run (Step maxProposals) s t) : t.question = s.question :=
  Run.keeps (fun x => x.question = s.question)
    (fun st h => (step_keeps_question maxProposals st).trans h) r rfl

/-- What bounds the architect's work on a question: two steps per proposal
left, and one to end. -/
def State.fuel : State → Nat
  | .asked _ n => 2 * (maxProposals - n) + 2
  | .proposing _ n _ => 2 * (maxProposals - n) + 1
  | _ => 0

/-- Every question ends within `2 * maxProposals + 2` steps. -/
theorem run_bounded (r : Run (Step maxProposals) s t) : r.length ≤ s.fuel maxProposals :=
  Run.bounded (State.fuel maxProposals) (fun st => by cases st <;> simp [State.fuel] <;> omega) r

theorem designed_is_final : Step m (.designed q rev) t → False := nofun
theorem declined_is_final : Step m (.declined q w) t → False := nofun
theorem refused_is_final : Step m (.refused q r) t → False := nofun

end Architect
