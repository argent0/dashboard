import Agents.Player

/-!
The edge between the player and the foreman of a task, indexed by the
player's step. Each `Sync` pairs one player step with the foreman's part
in it: the foreman starts, moves, or waits (`holds`). `Sync.foreman` is
the check. The foreman's assignment names the player's plan, and its asks
the player's round.
-/
namespace PlayerForeman
open Kernel Player

variable (lim : Limits) (failing : Revision → List String)

abbrev F := Option Foreman.State

inductive Sync : (s t : Task) → Step lim failing s t → F → F → Type
  /-- The step sends the foreman nothing: it waits as it is. -/
  | holds {s t f} {st : Step lim failing s t} (h : st.peers.contains .foreman = false) :
      Sync s t st f f
  /-- A plan to start: the foreman starts with its post to do. -/
  | start {p c} :
      Sync _ _ (.assign (o := .start p) (c := c) (h := none))
        none (some (.toPost ⟨p, c.rounds⟩))
  /-- A plan to continue: the unsure foreman posts again for it. -/
  | continue {p c h a w} :
      Sync _ _ (.assign (o := .continue p) (c := c) (h := h))
        (some (.unsure a w)) (some (.toPost ⟨p, c.rounds⟩))
  | posted {p c h} (rev : Revision) (hf : failing rev ≠ []) :
      Sync _ _ (.posted (p := p) (c := c) (h := h) rev hf)
        (some (.toPost ⟨p, c.rounds⟩)) (some (.posted ⟨p, c.rounds⟩ rev))
  /-- The post passes, so it tests nothing: refused, post again. -/
  | postRefused {p c h} (rev : Revision) (hp : failing rev = [])
      (hl : c.posts + 1 < lim.postStrikes) :
      Sync _ _ (.postRefused (p := p) (c := c) (h := h) rev hp hl)
        (some (.toPost ⟨p, c.rounds⟩)) (some (.toPost ⟨p, c.rounds⟩))
  | postOut {p c h} (rev : Revision) (hp : failing rev = [])
      (hl : lim.postStrikes ≤ c.posts + 1) :
      Sync _ _ (.postOut (p := p) (c := c) (h := h) rev hp hl)
        (some (.toPost ⟨p, c.rounds⟩)) (some (.posted ⟨p, c.rounds⟩ rev))
  | postUnsure {p c h} (why : String) :
      Sync _ _ (.postUnsure (p := p) (c := c) (h := h) why)
        (some (.toPost ⟨p, c.rounds⟩)) (some (.unsure ⟨p, c.rounds⟩ why))
  | askReview {p c n pr} (rev : Revision) (hp : failing rev = []) :
      Sync _ _ (.submitted (p := p) (c := c) rev hp)
        (some (.posted ⟨p, n⟩ pr)) (some (.owing ⟨p, n⟩ pr (.review c.rounds rev)))
  | askContest {p c n pr} (t w : String) (hl : c.contests + 1 ≤ lim.contestRounds) :
      Sync _ _ (.contested (p := p) (c := c) t w hl)
        (some (.posted ⟨p, n⟩ pr)) (some (.owing ⟨p, n⟩ pr (.contest c.rounds t w)))
  | sentBack {p c n sub a pr} (note : Revision) (hf : failing note ≠ [])
      (hr : c.rounds + 1 ≤ lim.reviewRounds) :
      Sync _ _ (.sentBack (p := p) (c := c) (n := n) (sub := sub) note hf hr)
        (some (.owing a pr (.review n sub))) (some (.posted a pr))
  /-- The send-back's test passes on the work: refused, review again. -/
  | sendBackRefused {p c n sub a pr} (note : Revision) (hp : failing note = [])
      (hl : c.reviews + 1 < lim.reviewStrikes) :
      Sync _ _ (.sendBackRefused (p := p) (c := c) (n := n) (sub := sub) note hp hl)
        (some (.owing a pr (.review n sub))) (some (.owing a pr (.review n sub)))
  | sendBackOut {p c n sub a pr} (note : Revision) (hp : failing note = [])
      (hl : lim.reviewStrikes ≤ c.reviews + 1) :
      Sync _ _ (.sendBackOut (p := p) (c := c) (n := n) (sub := sub) note hp hl)
        (some (.owing a pr (.review n sub))) (some (.posted a pr))
  | overRounds {p c n sub a pr} (note : Revision) (hf : failing note ≠ [])
      (hr : lim.reviewRounds < c.rounds + 1) :
      Sync _ _ (.overRounds (p := p) (c := c) (n := n) (sub := sub) note hf hr)
        (some (.owing a pr (.review n sub))) (some (.posted a pr))
  | recommended {p c n sub a pr} (trim : List String) :
      Sync _ _ (.recommended (p := p) (c := c) (n := n) (sub := sub) trim)
        (some (.owing a pr (.review n sub))) (some (.posted a pr))
  | replied {p c n t w a pr} (rev : Revision) :
      Sync _ _ (.replied (p := p) (c := c) (n := n) (t := t) (w := w) rev)
        (some (.owing a pr (.contest n t w))) (some (.posted a pr))
  | doubted {p c k a pr} (why : String) :
      Sync _ _ (.doubted (p := p) (c := c) (k := k) why)
        (some (.owing a pr k)) (some (.unsure a why))
  | clean {p c merge sub a pr} (h : failing merge = []) :
      Sync _ _ (.clean (p := p) (c := c) (merge := merge) (sub := sub) h)
        (some (.posted a pr)) (some .accepted)
  /-- The planner drops the task: the waiting foreman is dropped with the
  task's cause. -/
  | drop {s f} (why : String) (h : s.awaitsPlanner = true) (hw : f.waits = true) :
      Sync _ _ (.drop (s := s) why h) (some f) (some (.dropped s.cause))

/-- Each sync is the foreman's part: its start, its own steps, or a wait. -/
def Sync.foreman : Sync lim failing s t st f f' → Peer Foreman.Step Foreman.State.isStart f f'
  | .holds _ => .inr ⟨rfl⟩
  | .start => .inl ⟨rfl⟩
  | .continue (p := p) (c := c) => .inl (Steps.one (.revised ⟨p, c.rounds⟩))
  | .posted rev _ => .inl (Steps.one (.post rev))
  | .postRefused (p := p) (c := c) rev _ _ =>
    .inl (Steps.two (.post rev) (.repost ⟨p, c.rounds⟩))
  | .postOut rev _ _ => .inl (Steps.one (.post rev))
  | .postUnsure why => .inl (Steps.one (.postUnsure why))
  | .askReview (c := c) rev _ => .inl (Steps.one (.asked (.review c.rounds rev)))
  | .askContest (c := c) t w _ => .inl (Steps.one (.asked (.contest c.rounds t w)))
  | .sentBack note _ _ => .inl (Steps.one (.respond (.sentBack note)))
  | .sendBackRefused (n := n) (sub := sub) note _ _ =>
    .inl (Steps.two (.respond (.sentBack note)) (.asked (.review n sub)))
  | .sendBackOut note _ _ => .inl (Steps.one (.respond (.sentBack note)))
  | .overRounds note _ _ => .inl (Steps.one (.respond (.sentBack note)))
  | .recommended trim => .inl (Steps.one (.respond (.recommend trim)))
  | .replied rev => .inl (Steps.one (.respond (.replied rev)))
  | .doubted why => .inl (Steps.one (.respond (.unsure why)))
  | .clean _ => .inl (Steps.one .accept)
  | .drop (s := s) _ _ hw => .inl (Steps.one (.drop s.cause hw))

/-
Rejected: the foreman accepts while the player asks it to review.

    example {p c n pr} (rev : Revision) (hp : failing rev = []) :
        Sync lim failing _ _ (.submitted (p := p) (c := c) rev hp)
          (some (.posted ⟨p, n⟩ pr)) (some .accepted) :=
      .askReview rev hp

    error: Type mismatch: .askReview rev hp has type
      Sync … (some (.posted ⟨p, n⟩ pr)) (some (.owing ⟨p, n⟩ pr (.review c.rounds rev)))
    but is expected to have type
      Sync … (some (.posted ⟨p, n⟩ pr)) (some .accepted)
-/

/-- The foreman along a run of the player's task. -/
abbrev EdgeRun {s u : Task} (r : Run (Step lim failing) s u) : F → F → Type :=
  Kernel.EdgeRun (Sync lim failing) r

end PlayerForeman
