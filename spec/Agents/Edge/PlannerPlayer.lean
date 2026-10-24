import Agents.Player

/-!
The edge between the planner and the player's task, indexed by the
player's step. The planner's `plan` (or `continue`) is the order that
starts the task, so the planner is present from the task's first step:
`running g p` for the task's plan `p`. Each advice the player brings pairs
with the planner's report, each decision with the player's step that
carries it out. `Sync.planner` is the check.
-/
namespace PlannerPlayer
open Kernel Player

variable (lim : Limits) (failing : Revision → List String)

abbrev P := Planner.State

inductive Sync : (s t : Task) → Step lim failing s t → P → P → Type
  /-- The step sends the planner nothing: it waits as it is. -/
  | holds {s t g} {st : Step lim failing s t} (h : st.peers.contains .planner = false) :
      Sync s t st g g
  -- advice
  | postOut {G p c h} (rev : Revision) (hp : failing rev = [])
      (hl : lim.postStrikes ≤ c.posts + 1) :
      Sync _ _ (.postOut (p := p) (c := c) (h := h) rev hp hl)
        (.running G p) (.advised G p (.stopped (.strikes .post)))
  | postUnsure {G p c h} (why : String) :
      Sync _ _ (.postUnsure (p := p) (c := c) (h := h) why)
        (.running G p) (.advised G p (.doubt why))
  | failedOut {G p c} (rev : Revision) (hf : failing rev ≠ [])
      (hl : lim.suiteStrikes ≤ c.suite + 1) :
      Sync _ _ (.failedOut (p := p) (c := c) rev hf hl)
        (.running G p) (.advised G p (.stopped (.strikes .suite)))
  | contestOut {G p c} (t w : String) (hl : lim.contestRounds < c.contests + 1) :
      Sync _ _ (.contestOut (p := p) (c := c) t w hl)
        (.running G p) (.advised G p (.stopped .contestRounds))
  | blocked {G p c} (why : String) :
      Sync _ _ (.blocked (p := p) (c := c) why) (.running G p) (.advised G p (.stopped .blocked))
  | sendBackOut {G p c n sub} (note : Revision) (hp : failing note = [])
      (hl : lim.reviewStrikes ≤ c.reviews + 1) :
      Sync _ _ (.sendBackOut (p := p) (c := c) (n := n) (sub := sub) note hp hl)
        (.running G p) (.advised G p (.stopped (.strikes .review)))
  | overRounds {G p c n sub} (note : Revision) (hf : failing note ≠ [])
      (hr : lim.reviewRounds < c.rounds + 1) :
      Sync _ _ (.overRounds (p := p) (c := c) (n := n) (sub := sub) note hf hr)
        (.running G p) (.advised G p (.stopped .reviewRounds))
  | recommended {G p c n sub} (trim : List String) :
      Sync _ _ (.recommended (p := p) (c := c) (n := n) (sub := sub) trim)
        (.running G p) (.advised G p (.recommend trim))
  | doubted {G p c k} (why : String) :
      Sync _ _ (.doubted (p := p) (c := c) (k := k) why) (.running G p) (.advised G p (.doubt why))
  -- decisions
  | trimmed {G p c trim tip sub} (rev : Revision) (hp : failing rev = []) (ht : trim ≠ []) :
      Sync _ _ (.trimmed (p := p) (c := c) (trim := trim) (tip := tip) (sub := sub) rev hp ht)
        (.advised G p (.recommend trim)) (.advised G p (.recommend []))
  | accept {G p c t tip sub} (merge : Revision) :
      Sync _ _ (.accept (p := p) (c := c) (t := t) (tip := tip) (sub := sub) merge)
        (.advised G p (.recommend t)) (.landing G p)
  /-- A revised plan: the player continues the task with it. -/
  | continue {G p c w h} (p' : Plan) (hl : c.continues + 1 ≤ lim.continues) :
      Sync _ _ (.continue (p := p) (c := c) (w := w) (h := h) p' hl)
        (.advised G p (.doubt w)) (.running G p')
  | resolve {G p c paths sub} (merge : Revision) :
      Sync _ _ (.resolve (p := p) (c := c) (paths := paths) (sub := sub) merge)
        (.advised G p (.conflict paths)) (.landing G p)
  | dropDeciding {G p c t tip sub} (why : String) (h) :
      Sync _ _ (.drop (s := .deciding p c t tip sub) why h)
        (.advised G p (.recommend t)) (.dropped G p .retired why)
  | dropDoubting {G p c w held} (why : String) (h) :
      Sync _ _ (.drop (s := .doubting p c w held) why h)
        (.advised G p (.doubt w)) (.dropped G p .retired why)
  /-- A stopped task: the drop keeps the cause, for a replan. -/
  | dropStopping {G p cause} (why : String) (h) :
      Sync _ _ (.drop (s := .stopping p cause) why h)
        (.advised G p (.stopped cause)) (.dropped G p cause why)
  | dropResolving {G p c paths sub} (why : String) (h) :
      Sync _ _ (.drop (s := .resolving p c paths sub) why h)
        (.advised G p (.conflict paths)) (.dropped G p .retired why)
  -- the player's merge
  /-- The player's clean merge lands the planner's plan. -/
  | clean {G p c merge sub} (h : failing merge = []) :
      Sync _ _ (.clean (p := p) (c := c) (merge := merge) (sub := sub) h)
        (.landing G p) (.landed p)
  /-- The merge broke the run set: the planner's plan runs again. -/
  | broken {G p c merge sub} (h : failing merge ≠ []) (hl : c.suite + 1 < lim.suiteStrikes) :
      Sync _ _ (.broken (p := p) (c := c) (merge := merge) (sub := sub) h hl)
        (.landing G p) (.running G p)
  | brokenOut {G p c merge sub} (h : failing merge ≠ []) (hl : lim.suiteStrikes ≤ c.suite + 1) :
      Sync _ _ (.brokenOut (p := p) (c := c) (merge := merge) (sub := sub) h hl)
        (.landing G p) (.advised G p (.stopped (.strikes .suite)))
  | conflicted {G p c merge sub} (paths : List String) (hl : c.conflicts + 1 ≤ lim.conflicts) :
      Sync _ _ (.conflicted (p := p) (c := c) (merge := merge) (sub := sub) paths hl)
        (.landing G p) (.advised G p (.conflict paths))
  | conflictOut {G p c merge sub} (paths : List String) (hl : lim.conflicts < c.conflicts + 1) :
      Sync _ _ (.conflictOut (p := p) (c := c) (merge := merge) (sub := sub) paths hl)
        (.landing G p) (.advised G p (.stopped .conflicts))

/-- Each sync is the planner's part: its own steps, or a wait. -/
def Sync.planner : Sync lim failing s t st g g' → Present Planner.Step g g'
  | .holds _ => .inr ⟨rfl⟩
  | .postOut .. => .inl (Steps.one (.report (.stopped (.strikes .post)) rfl))
  | .postUnsure why => .inl (Steps.one (.report (.doubt why) rfl))
  | .failedOut .. => .inl (Steps.one (.report (.stopped (.strikes .suite)) rfl))
  | .contestOut .. => .inl (Steps.one (.report (.stopped .contestRounds) rfl))
  | .blocked _ => .inl (Steps.one (.report (.stopped .blocked) rfl))
  | .sendBackOut .. => .inl (Steps.one (.report (.stopped (.strikes .review)) rfl))
  | .overRounds .. => .inl (Steps.one (.report (.stopped .reviewRounds) rfl))
  | .recommended trim => .inl (Steps.one (.report (.recommend trim) rfl))
  | .doubted why => .inl (Steps.one (.report (.doubt why) rfl))
  | .trimmed rev _ ht => .inl (Steps.one (.decide (.trim rev ht)))
  | .accept _ => .inl (Steps.one (.decide .accept))
  | .continue p' _ => .inl (Steps.one (.decide (.continue p')))
  | .resolve merge => .inl (Steps.one (.decide (.resolve merge)))
  | .dropDeciding why _ => .inl (Steps.one (.decide (.drop why)))
  | .dropDoubting why _ => .inl (Steps.one (.decide (.drop why)))
  | .dropStopping why _ => .inl (Steps.one (.decide (.drop why)))
  | .dropResolving why _ => .inl (Steps.one (.decide (.drop why)))
  | .clean _ => .inl (Steps.one .land)
  | .broken .. => .inl (Steps.one .reopened)
  | .brokenOut .. => .inl (Steps.two .reopened (.report (.stopped (.strikes .suite)) rfl))
  | .conflicted paths _ => .inl (Steps.one (.conflict paths))
  | .conflictOut .. => .inl (Steps.two .reopened (.report (.stopped .conflicts) rfl))

/-
Rejected: the planner lands while the merge breaks the run set.

    example {G p c merge sub} (h : failing merge ≠ []) (hl : c.suite + 1 < lim.suiteStrikes) :
        Sync lim failing _ _ (.broken (p := p) (c := c) (merge := merge) (sub := sub) h hl)
          (.landing G p) (.landed p) :=
      .broken h hl

    error: Type mismatch: .broken h hl has type
      Sync … (Step.broken h hl) (.landing G p) (.running G p)
    but is expected to have type
      Sync … (Step.broken h hl) (.landing G p) (.landed p)
-/

/-- The planner along a run of the player's task. -/
abbrev EdgeRun {s u : Task} (r : Run (Step lim failing) s u) : P → P → Type :=
  Kernel.EdgeRun (Sync lim failing) r

end PlannerPlayer
