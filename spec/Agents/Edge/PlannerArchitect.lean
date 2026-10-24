import Agents.Planner
import Agents.Architect

/-!
The edge between the planner and the architect of one need, indexed by
the planner's step: a planner that names several needs has one edge per
need, each with its own architect. The planner's `need what` starts the
architect on `what`; the architect's end answers the planner, and the
architect's run between the two is the sync's evidence (its steps pair
with the player's review in `ArchitectPlayer`). `Sync.architect` is the
check.
-/
namespace PlannerArchitect
open Kernel

variable (maxProposals : Nat) (what : String)

abbrev A := Option Architect.State

inductive Sync : (s t : Planner.State) → Planner.Step s t → A → A → Type
  /-- The step names another need, or none: this architect waits as it is. -/
  | holds {s t a} {st : Planner.Step s t} (h : st.need? ≠ some what) : Sync s t st a a
  /-- The planner names the need: the architect starts with it to design. -/
  | need {g} : Sync _ _ (.need (g := g) what) none (some (.asked (.need what) 0))
  /-- The architect's revision is on `main`: the planner plans on it. -/
  | designed {g a rev} (r : Steps (Architect.Step maxProposals) a (.designed (.need what) rev)) :
      Sync _ _ (.designed (g := g) (w := what) rev) (some a) (some (.designed (.need what) rev))
  | declined {g a why} (r : Steps (Architect.Step maxProposals) a (.declined (.need what) why)) :
      Sync _ _ (.declined (g := g) (w := what) why) (some a) (some (.declined (.need what) why))
  /-- The player refused the architect's last proposal: the planner hears it
  as a decline, with why. -/
  | refused {g a r} (why : String) (run : Steps (Architect.Step maxProposals) a (.refused (.need what) r)) :
      Sync _ _ (.declined (g := g) (w := what) why) (some a) (some (.refused (.need what) r))

/-- Each sync is the architect's part: its start, its run to its end, or a
wait. -/
def Sync.architect : Sync maxProposals what s t st a a' →
    Peer (Architect.Step maxProposals) Architect.State.isStart a a'
  | .holds _ => .inr ⟨rfl⟩
  | .need => .inl ⟨rfl⟩
  | .designed r => .inl r
  | .declined r => .inl r
  | .refused _ r => .inl r

/-
Rejected: one architect for a second need of the same name.

    example {g rev} (m : Nat) (what : String) :
        Sync m what _ _ (.need (g := g) what)
          (some (.designed (.need what) rev)) (some (.asked (.need what) 0)) :=
      .need

    error: Type mismatch: .need has type
      Sync … (Planner.Step.need what) none (some (.asked (.need what) 0))
    but is expected to have type
      Sync … (Planner.Step.need what) (some (.designed (.need what) rev)) (some (.asked (.need what) 0))
-/

/-- The architect of `what` along a run of the planner. -/
abbrev EdgeRun {s u : Planner.State} (r : Run Planner.Step s u) : A → A → Type :=
  Kernel.EdgeRun (Sync maxProposals what) r

end PlannerArchitect
