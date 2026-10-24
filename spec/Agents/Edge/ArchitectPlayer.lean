import Agents.Architect
import Agents.Player

/-!
The edge between the architect and the player, indexed by the player's
review step. The architect's first proposal starts the review, so the
architect is present from the review's first step: `proposing q 0 rev`
for the review's `checking rev 0`. Every review step answers the architect
or hears from it. The architect's limit is the player's: `lim.proposals`.
`Sync.architect` is the check.
-/
namespace ArchitectPlayer
open Kernel Player

variable (lim : Limits) (archOk : Revision → Except Reason Unit)

abbrev A := Architect.State

inductive Sync : (s t : Review) → ReviewStep lim archOk s t → A → A → Type
  | accepted {q rev n} (h : archOk rev = .ok ()) :
      Sync _ _ (.accept (rev := rev) (n := n) h) (.proposing q n rev) (.designed q rev)
  | refused {q rev n} (r : Reason) (h : archOk rev = .error r) (hl : n + 1 < lim.proposals) :
      Sync _ _ (.refuse r h hl) (.proposing q n rev) (.asked q (n + 1))
  | refusedLast {q rev n} (r : Reason) (h : archOk rev = .error r)
      (hl : lim.proposals ≤ n + 1) :
      Sync _ _ (.refuseLast r h hl) (.proposing q n rev) (.refused q r)
  | proposed {q rev n} (rev' : Revision) :
      Sync _ _ (.proposed (rev := rev) (n := n) rev') (.asked q n) (.proposing q n rev')
  /-- The architect declines after a refusal: the review ends withdrawn. -/
  | withdraw {q rev n} (why : String) :
      Sync _ _ (.withdraw (rev := rev) (n := n)) (.asked q n) (.declined q why)

/-- Each sync is the architect's own step. -/
def Sync.architect : Sync lim archOk s t st a a' → Steps (Architect.Step lim.proposals) a a'
  | .accepted _ => Steps.one .accepted
  | .refused r _ hl => Steps.one (.refusedAgain r hl)
  | .refusedLast r _ hl => Steps.one (.refusedLast r hl)
  | .proposed rev' => Steps.one (.propose rev')
  | .withdraw why => Steps.one (.decline why)

/-
Rejected: the architect counts a refused revision as designed.

    example {q rev n} (r : Reason) (h : archOk rev = .error r) (hl : n + 1 < lim.proposals) :
        Sync lim archOk _ _ (.refuse r h hl) (.proposing q n rev) (.designed q rev) :=
      .refused r h hl

    error: Type mismatch: .refused r h hl has type
      Sync … (.refuse r h hl) (.proposing q n rev) (.asked q (n + 1))
    but is expected to have type
      Sync … (.refuse r h hl) (.proposing q n rev) (.designed q rev)
-/

/-- The architect along a run of the player's review. -/
abbrev EdgeRun {s u : Review} (r : Run (ReviewStep lim archOk) s u) : A → A → Type :=
  Kernel.EdgeRun (Sync lim archOk) r

end ArchitectPlayer
