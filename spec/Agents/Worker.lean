import Agents.Kernel

/-!
The worker's work. One worker per task. It starts with something to do, a
job: the plan, the prompt and the checks that fail. Each commit it makes
is a request; the reply has a type fixed by that request. It ends when the
task is accepted or dropped.
-/
namespace Worker
open Kernel

/-- What the worker is handed. `notes` are the foreman's notes and contest
replies, oldest first. -/
structure Job where
  plan : Plan
  prompt : Revision
  failing : List String
  notes : List Revision := []

/-- A commit the worker makes: a request the hub answers. -/
inductive Move
  /-- The work, for review. -/
  | submit (rev : Revision)
  | contest (test why : String)
  | block (why : String)

/-- The hub's possible replies to each move. -/
inductive Reply : Move → Type
  /-- Back, with the foreman's note and the checks that now fail. -/
  | sentBack {rev} (note : Revision) (failing : List String) : Reply (.submit rev)
  /-- Refused: why, and strikes left. -/
  | refused {rev} (r : Reason) (strikesLeft : Nat) : Reply (.submit rev)
  | accepted {rev} : Reply (.submit rev)
  | replied {t w} (reply : Revision) : Reply (.contest t w)
  /-- Go on with a revised job, in the same round. -/
  | restarted {m} (j : Job) : Reply m
  | dropped {m} (c : Cause) : Reply m

inductive State
  /-- Something to do: build job `j`, round `round`. The start is round 0. -/
  | building (j : Job) (round : Nat)
  /-- Made move `m`; its reply is due. -/
  | waiting (j : Job) (round : Nat) (m : Move)
  /-- The task was accepted. An end. -/
  | accepted
  /-- The task was dropped. An end. -/
  | dropped (c : Cause)

/-- Where reply `r` to move `m` leaves the worker. -/
def after (j : Job) (n : Nat) : (m : Move) → Reply m → State
  | _, .sentBack note fl => .building { j with failing := fl, notes := j.notes ++ [note] } (n + 1)
  | _, .refused .. => .building j n
  | _, .accepted => .accepted
  | _, .replied r => .building { j with notes := j.notes ++ [r] } n
  | _, .restarted j' => .building j' n
  | _, .dropped c => .dropped c

/-- The worker's legal transitions. -/
inductive Step : State → State → Type
  | move {j n} (m : Move) : Step (.building j n) (.waiting j n m)
  | answer {j n m} (r : Reply m) : Step (.waiting j n m) (after j n m r)

def State.isStart : State → Bool
  | .building _ 0 => true
  | .building _ (_ + 1) | .waiting .. | .accepted | .dropped _ => false

/-- While it waits, the worker's only step is the reply to its move. -/
theorem waiting_steps (st : Step (.waiting j n m) t) : ∃ r : Reply m, t = after j n m r := by
  cases st with
  | answer r => exact ⟨r, rfl⟩

/-- A send-back hands the worker its note and the checks that now fail. -/
theorem sentBack_kept (j : Job) (n : Nat) (rev note : Revision) (fl : List String) :
    after j n (.submit rev) (.sentBack note fl) =
      .building { j with failing := fl, notes := j.notes ++ [note] } (n + 1) := rfl

def State.round : State → Nat
  | .building _ n | .waiting _ n _ => n
  | .accepted | .dropped _ => 0

def State.job? : State → Option Job
  | .building j _ | .waiting j _ _ => some j
  | .accepted | .dropped _ => none

def State.live : State → Prop
  | .building .. | .waiting .. => True
  | .accepted | .dropped _ => False

/-- The round never goes back while the worker is live. -/
theorem step_round (st : Step s t) (ht : t.live) : s.round ≤ t.round := by
  cases st with
  | move => exact Nat.le_refl _
  | answer r =>
    cases r <;> simp_all [after, State.round, State.live]

/-- Along a run that ends live, the round never goes back. -/
theorem run_round (r : Run Step s u) (hu : u.live) : s.round ≤ u.round := by
  induction r with
  | stay => exact Nat.le_refl _
  | next st r ih =>
    rename_i t _
    have ht : t.live := by
      cases r with
      | stay => exact hu
      | next st' _ => cases st' <;> simp [State.live]
    exact Nat.le_trans (step_round st ht) (ih hu)

theorem accepted_is_final : Step .accepted t → False := nofun
theorem dropped_is_final : Step (.dropped c) t → False := nofun

end Worker
