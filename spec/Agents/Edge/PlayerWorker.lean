import Agents.Player

/-!
The edge between the player and the worker of a task, indexed by the
player's step. Each `Sync` pairs one player step with the worker's part in
it: the worker starts, moves, or waits (`holds`, when the step sends it
nothing). `Sync.worker` turns each pair into the worker's legal steps; that
it type-checks is the whole of the check. The worker's state names the
player's plan and round, so a pairing holds for this task's worker alone.
-/
namespace PlayerWorker
open Kernel Player

variable (lim : Limits) (failing : Revision → List String)

abbrev W := Option Worker.State

inductive Sync : (s t : Task) → Step lim failing s t → W → W → Type
  /-- The step sends the worker nothing: it waits as it is. -/
  | holds {s t w} {st : Step lim failing s t} (h : st.peers.contains .worker = false) :
      Sync s t st w w
  /-- The player launches the worker on its job. -/
  | launch {p c prompt fl} (h0 : c.rounds = 0) :
      Sync _ _ (.launch (p := p) (c := c) (prompt := prompt) (fl := fl))
        none (some (.building ⟨p, prompt, fl, []⟩ 0))
  /-- A continued plan: the waiting worker goes on with the revised job. -/
  | restart {p c prompt fl m j} :
      Sync _ _ (.restart (p := p) (c := c) (prompt := prompt) (fl := fl) (m := m))
        (some (.waiting j c.rounds m)) (some (.building ⟨p, prompt, fl, j.notes⟩ c.rounds))
  | submit {p c pr fl ns} (rev : Revision) (hp : failing rev = []) :
      Sync _ _ (.submitted (p := p) (c := c) rev hp)
        (some (.building ⟨p, pr, fl, ns⟩ c.rounds))
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit rev)))
  | fail {p c pr fl ns} (rev : Revision) (hf : failing rev ≠ [])
      (hl : c.suite + 1 < lim.suiteStrikes) :
      Sync _ _ (.failed (p := p) (c := c) rev hf hl)
        (some (.building ⟨p, pr, fl, ns⟩ c.rounds)) (some (.building ⟨p, pr, fl, ns⟩ c.rounds))
  | failOut {p c pr fl ns} (rev : Revision) (hf : failing rev ≠ [])
      (hl : lim.suiteStrikes ≤ c.suite + 1) :
      Sync _ _ (.failedOut (p := p) (c := c) rev hf hl)
        (some (.building ⟨p, pr, fl, ns⟩ c.rounds)) (some (.dropped (.strikes .suite)))
  | contest {p c pr fl ns} (t w : String) (hl : c.contests + 1 ≤ lim.contestRounds) :
      Sync _ _ (.contested (p := p) (c := c) t w hl)
        (some (.building ⟨p, pr, fl, ns⟩ c.rounds))
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.contest t w)))
  | contestOut {p c pr fl ns} (t w : String) (hl : lim.contestRounds < c.contests + 1) :
      Sync _ _ (.contestOut (p := p) (c := c) t w hl)
        (some (.building ⟨p, pr, fl, ns⟩ c.rounds)) (some (.dropped .contestRounds))
  | block {p c pr fl ns} (why : String) :
      Sync _ _ (.blocked (p := p) (c := c) why)
        (some (.building ⟨p, pr, fl, ns⟩ c.rounds)) (some (.dropped .blocked))
  /-- The note goes back with the checks it makes fail: the next round. -/
  | sentBack {p c n sub pr fl ns} (note : Revision) (hf : failing note ≠ [])
      (hr : c.rounds + 1 ≤ lim.reviewRounds) :
      Sync _ _ (.sentBack (p := p) (c := c) (n := n) (sub := sub) note hf hr)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub)))
        (some (.building ⟨p, pr, failing note, ns ++ [note]⟩ (c.rounds + 1)))
  | sendBackOut {p c n sub pr fl ns} (note : Revision) (hp : failing note = [])
      (hl : lim.reviewStrikes ≤ c.reviews + 1) :
      Sync _ _ (.sendBackOut (p := p) (c := c) (n := n) (sub := sub) note hp hl)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub)))
        (some (.dropped (.strikes .review)))
  | overRounds {p c n sub pr fl ns} (note : Revision) (hf : failing note ≠ [])
      (hr : lim.reviewRounds < c.rounds + 1) :
      Sync _ _ (.overRounds (p := p) (c := c) (n := n) (sub := sub) note hf hr)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub))) (some (.dropped .reviewRounds))
  | replied {p c n t w pr fl ns} (rev : Revision) :
      Sync _ _ (.replied (p := p) (c := c) (n := n) (t := t) (w := w) rev)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.contest t w)))
        (some (.building ⟨p, pr, fl, ns ++ [rev]⟩ c.rounds))
  /-- A continued order the foreman's posts run out on: the held move's
  answer is the stop. -/
  | postOut {p c m j} (rev : Revision) (hp : failing rev = [])
      (hl : lim.postStrikes ≤ c.posts + 1) :
      Sync _ _ (.postOut (p := p) (c := c) (h := some m) rev hp hl)
        (some (.waiting j c.rounds m)) (some (.dropped (.strikes .post)))
  /-- Landed: the worker's submission is accepted. -/
  | clean {p c merge sub pr fl ns} (h : failing merge = []) :
      Sync _ _ (.clean (p := p) (c := c) (merge := merge) (sub := sub) h)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub))) (some .accepted)
  /-- The merge broke the run set: a suite strike, and the worker builds
  again in the same round. -/
  | broken {p c merge sub pr fl ns} (h : failing merge ≠ [])
      (hl : c.suite + 1 < lim.suiteStrikes) :
      Sync _ _ (.broken (p := p) (c := c) (merge := merge) (sub := sub) h hl)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub)))
        (some (.building ⟨p, pr, fl, ns⟩ c.rounds))
  | brokenOut {p c merge sub pr fl ns} (h : failing merge ≠ [])
      (hl : lim.suiteStrikes ≤ c.suite + 1) :
      Sync _ _ (.brokenOut (p := p) (c := c) (merge := merge) (sub := sub) h hl)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub))) (some (.dropped (.strikes .suite)))
  | conflictOut {p c merge sub pr fl ns} (paths : List String)
      (hl : lim.conflicts < c.conflicts + 1) :
      Sync _ _ (.conflictOut (p := p) (c := c) (merge := merge) (sub := sub) paths hl)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub))) (some (.dropped .conflicts))
  | dropDeciding {p c t tip sub pr fl ns} (why : String) (h) :
      Sync _ _ (.drop (s := .deciding p c t tip sub) why h)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub))) (some (.dropped .retired))
  | dropResolving {p c paths sub pr fl ns} (why : String) (h) :
      Sync _ _ (.drop (s := .resolving p c paths sub) why h)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds (.submit sub))) (some (.dropped .retired))
  | dropDoubting {p c w m pr fl ns} (why : String) (h) :
      Sync _ _ (.drop (s := .doubting p c w (some m)) why h)
        (some (.waiting ⟨p, pr, fl, ns⟩ c.rounds m)) (some (.dropped .retired))

/-- Each sync is the worker's part: its start, its own steps, or a wait. -/
def Sync.worker : Sync lim failing s t st w w' → Peer Worker.Step Worker.State.isStart w w'
  | .holds _ => .inr ⟨rfl⟩
  | .launch _ => .inl ⟨rfl⟩
  | .restart (p := p) (prompt := prompt) (fl := fl) (j := j) =>
    .inl (Steps.one (.answer (.restarted ⟨p, prompt, fl, j.notes⟩)))
  | .submit rev _ => .inl (Steps.one (.move (.submit rev)))
  | .fail (c := c) rev _ _ =>
    .inl (Steps.two (.move (.submit rev)) (.answer (.refused .suite (lim.suiteStrikes - (c.suite + 1)))))
  | .failOut rev _ _ => .inl (Steps.two (.move (.submit rev)) (.answer (.dropped (.strikes .suite))))
  | .contest t w _ => .inl (Steps.one (.move (.contest t w)))
  | .contestOut t w _ => .inl (Steps.two (.move (.contest t w)) (.answer (.dropped .contestRounds)))
  | .block why => .inl (Steps.two (.move (.block why)) (.answer (.dropped .blocked)))
  | .sentBack note _ _ => .inl (Steps.one (.answer (.sentBack note (failing note))))
  | .sendBackOut _ _ _ => .inl (Steps.one (.answer (.dropped (.strikes .review))))
  | .overRounds _ _ _ => .inl (Steps.one (.answer (.dropped .reviewRounds)))
  | .replied rev => .inl (Steps.one (.answer (.replied rev)))
  | .postOut _ _ _ => .inl (Steps.one (.answer (.dropped (.strikes .post))))
  | .clean _ => .inl (Steps.one (.answer .accepted))
  | .broken (c := c) _ _ =>
    .inl (Steps.one (.answer (.refused .suite (lim.suiteStrikes - (c.suite + 1)))))
  | .brokenOut _ _ => .inl (Steps.one (.answer (.dropped (.strikes .suite))))
  | .conflictOut _ _ => .inl (Steps.one (.answer (.dropped .conflicts)))
  | .dropDeciding _ _ => .inl (Steps.one (.answer (.dropped .retired)))
  | .dropResolving _ _ => .inl (Steps.one (.answer (.dropped .retired)))
  | .dropDoubting _ _ => .inl (Steps.one (.answer (.dropped .retired)))

/-
Rejected: a step that talks to the worker, passed off as a wait.

    example {p c merge sub w} (h : failing merge = []) :
        Sync lim failing _ _ (.clean (p := p) (c := c) (merge := merge) (sub := sub) h) w w :=
      .holds rfl

    error: Application type mismatch: The argument rfl has type ?m = ?m
    but is expected to have type
      (Step.peers lim failing (Step.clean h)).contains Role.worker = false
-/

/-- The worker along a run of the player's task. -/
abbrev EdgeRun {s u : Task} (r : Run (Step lim failing) s u) : W → W → Type :=
  Kernel.EdgeRun (Sync lim failing) r

end PlayerWorker
