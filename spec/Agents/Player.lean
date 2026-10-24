import Agents.Worker
import Agents.Foreman
import Agents.Planner

/-!
The player's work. It runs two machines.

- `Task`: one per order from the planner, to start a plan or continue a
  revised one. It hands the foreman its post, launches the worker, runs
  every check, relays asks and answers, tells the planner what it must
  decide, and does every merge.
- `Review`: one per architect question. It judges each revision the
  architect proposes for `main`.

`failing rev` is the check oracle: the checks of the task's run set that
fail at `rev`. `archOk rev` is the architecture oracle: `arch check` and
the ownership rules at `rev`, with the reason of a refusal.

Every loop of a task spends from `Spent`, and a step that would spend past
`Limits` stops the task instead; the counters never reset over a task's
life (spec §10). So every task ends (`run_bounded`).
-/
namespace Player
open Kernel

/-- The player's limits. Open: `continues`, `conflicts` and `proposals`
have no roadmap key yet. -/
structure Limits where
  /-- Send-backs allowed per task. -/
  reviewRounds : Nat := 2
  /-- Contests allowed per task. -/
  contestRounds : Nat := 2
  /-- A post strike at this count stops the task. -/
  postStrikes : Nat := 2
  /-- A send-back strike at this count stops the task. -/
  reviewStrikes : Nat := 2
  /-- A suite strike at this count stops the task. -/
  suiteStrikes : Nat := 2
  /-- Orders to continue allowed per task. -/
  continues : Nat := 2
  /-- Merge conflicts allowed per task. -/
  conflicts : Nat := 2
  /-- Proposals judged per architect question. -/
  proposals : Nat := 3

/-- What a task has spent so far. `rounds` is the round: the send-backs. -/
structure Spent where
  rounds : Nat := 0
  contests : Nat := 0
  posts : Nat := 0
  reviews : Nat := 0
  suite : Nat := 0
  continues : Nat := 0
  conflicts : Nat := 0

/-- Every counter within its limit. -/
structure Spent.Within (lim : Limits) (c : Spent) : Prop where
  rounds : c.rounds ≤ lim.reviewRounds
  contests : c.contests ≤ lim.contestRounds
  posts : c.posts ≤ lim.postStrikes
  reviews : c.reviews ≤ lim.reviewStrikes
  suite : c.suite ≤ lim.suiteStrikes
  continues : c.continues ≤ lim.continues
  conflicts : c.conflicts ≤ lim.conflicts

/-- What is left to spend, summed. -/
def Spent.left (lim : Limits) (c : Spent) : Nat :=
  (lim.reviewRounds - c.rounds) + (lim.contestRounds - c.contests) +
  (lim.postStrikes - c.posts) + (lim.reviewStrikes - c.reviews) +
  (lim.suiteStrikes - c.suite) + (lim.continues - c.continues) +
  (lim.conflicts - c.conflicts)

/-- What the planner orders. -/
inductive Order
  | start (p : Plan)
  | continue (p : Plan)

def Order.plan : Order → Plan
  | .start p | .continue p => p

/-- The worker's move a foreman ask answers. -/
def askHeld : Foreman.Ask → Worker.Move
  | .review _ sub => .submit sub
  | .contest _ t w => .contest t w

/-- One task, as the player runs it. `c` is what it has spent; `held` the
worker's move it has yet to answer. -/
inductive Task
  /-- Something to do: an order. The start is `ordered (.start p) {} none`. -/
  | ordered (o : Order) (c : Spent) (held : Option Worker.Move)
  /-- The foreman posts. -/
  | posting (p : Plan) (c : Spent) (held : Option Worker.Move)
  /-- The post fails as it must; the worker gets its job. -/
  | launching (p : Plan) (c : Spent) (prompt : Revision) (failing : List String)
      (held : Option Worker.Move)
  /-- The worker builds round `c.rounds`. -/
  | serving (p : Plan) (c : Spent)
  /-- Asked the foreman `k`. -/
  | asking (p : Plan) (c : Spent) (k : Foreman.Ask)
  /-- The planner decides on the foreman's recommendation for the worker's
  `sub`, which drops `trim`; `tip` is what accept merges: `sub`, or the trim
  commit on it. -/
  | deciding (p : Plan) (c : Spent) (trim : List String) (tip sub : Revision)
  /-- The planner decides on the foreman's doubt. -/
  | doubting (p : Plan) (c : Spent) (why : String) (held : Option Worker.Move)
  /-- The task stopped for `cause`; the planner decides. -/
  | stopping (p : Plan) (cause : Cause)
  /-- Merging `merge`, the player's merge commit, into `main`; holds the
  worker's `sub`. -/
  | merging (p : Plan) (c : Spent) (merge sub : Revision)
  /-- The planner resolves a conflict on `paths`. -/
  | resolving (p : Plan) (c : Spent) (paths : List String) (sub : Revision)
  /-- On `main`. An end. -/
  | landed (p : Plan)
  /-- Dropped for `cause`. An end. -/
  | dropped (p : Plan) (cause : Cause)

def Task.isStart : Task → Bool
  | .ordered (.start _) ⟨0, 0, 0, 0, 0, 0, 0⟩ none => true
  | _ => false

def Task.awaitsPlanner : Task → Bool
  | .deciding .. | .doubting .. | .stopping .. | .resolving .. => true
  | _ => false

def Task.plan : Task → Plan
  | .ordered o .. => o.plan
  | .posting p .. | .launching p .. | .serving p .. | .asking p .. | .deciding p ..
  | .doubting p .. | .stopping p _ | .merging p .. | .resolving p .. | .landed p
  | .dropped p _ => p

/-- What a drop records: a stop keeps its cause; any other drop retires. -/
def Task.cause : Task → Cause
  | .stopping _ c => c
  | _ => .retired

def Task.spent : Task → Spent
  | .ordered _ c _ | .posting _ c _ | .launching _ c .. | .serving _ c | .asking _ c _
  | .deciding _ c .. | .doubting _ c .. | .merging _ c .. | .resolving _ c .. => c
  | _ => {}

def Task.round (s : Task) : Nat := s.spent.rounds

variable (lim : Limits) (failing : Revision → List String)

/-- The player's legal transitions on a task. -/
inductive Step : Task → Task → Type
  -- the planner's order
  | assign {o c h} : Step (.ordered o c h) (.posting o.plan c h)
  -- the foreman's post
  | posted {p c h} (rev : Revision) (hf : failing rev ≠ []) :
      Step (.posting p c h) (.launching p c rev (failing rev) h)
  | postRefused {p c h} (rev : Revision) (hp : failing rev = [])
      (hl : c.posts + 1 < lim.postStrikes) :
      Step (.posting p c h) (.posting p { c with posts := c.posts + 1 } h)
  | postOut {p c h} (rev : Revision) (hp : failing rev = [])
      (hl : lim.postStrikes ≤ c.posts + 1) :
      Step (.posting p c h) (.stopping p (.strikes .post))
  | postUnsure {p c h} (why : String) : Step (.posting p c h) (.doubting p c why h)
  -- the worker's job
  | launch {p c prompt fl} : Step (.launching p c prompt fl none) (.serving p c)
  | restart {p c prompt fl m} : Step (.launching p c prompt fl (some m)) (.serving p c)
  -- the worker's moves
  | submitted {p c} (rev : Revision) (hp : failing rev = []) :
      Step (.serving p c) (.asking p c (.review c.rounds rev))
  | failed {p c} (rev : Revision) (hf : failing rev ≠ [])
      (hl : c.suite + 1 < lim.suiteStrikes) :
      Step (.serving p c) (.serving p { c with suite := c.suite + 1 })
  | failedOut {p c} (rev : Revision) (hf : failing rev ≠ [])
      (hl : lim.suiteStrikes ≤ c.suite + 1) :
      Step (.serving p c) (.stopping p (.strikes .suite))
  | contested {p c} (t w : String) (hl : c.contests + 1 ≤ lim.contestRounds) :
      Step (.serving p c) (.asking p { c with contests := c.contests + 1 } (.contest c.rounds t w))
  | contestOut {p c} (t w : String) (hl : lim.contestRounds < c.contests + 1) :
      Step (.serving p c) (.stopping p .contestRounds)
  | blocked {p c} (why : String) : Step (.serving p c) (.stopping p .blocked)
  -- the foreman's answers
  | sentBack {p c n sub} (note : Revision) (hf : failing note ≠ [])
      (hr : c.rounds + 1 ≤ lim.reviewRounds) :
      Step (.asking p c (.review n sub)) (.serving p { c with rounds := c.rounds + 1 })
  /-- The send-back's new test passes on the worker's code, so it tests
  nothing: a review strike, and the foreman reviews again. -/
  | sendBackRefused {p c n sub} (note : Revision) (hp : failing note = [])
      (hl : c.reviews + 1 < lim.reviewStrikes) :
      Step (.asking p c (.review n sub)) (.asking p { c with reviews := c.reviews + 1 } (.review n sub))
  | sendBackOut {p c n sub} (note : Revision) (hp : failing note = [])
      (hl : lim.reviewStrikes ≤ c.reviews + 1) :
      Step (.asking p c (.review n sub)) (.stopping p (.strikes .review))
  | overRounds {p c n sub} (note : Revision) (hf : failing note ≠ [])
      (hr : lim.reviewRounds < c.rounds + 1) :
      Step (.asking p c (.review n sub)) (.stopping p .reviewRounds)
  | recommended {p c n sub} (trim : List String) :
      Step (.asking p c (.review n sub)) (.deciding p c trim sub sub)
  | replied {p c n t w} (rev : Revision) : Step (.asking p c (.contest n t w)) (.serving p c)
  | doubted {p c k} (why : String) :
      Step (.asking p c k) (.doubting p c why (some (askHeld k)))
  -- the planner's decisions
  /-- The planner's trim commit `rev` (spec §6.5); the run set passes on it. -/
  | trimmed {p c trim tip sub} (rev : Revision) (hp : failing rev = []) (ht : trim ≠ []) :
      Step (.deciding p c trim tip sub) (.deciding p c [] rev sub)
  /-- The planner accepts: the player merges `tip` onto `main` as `merge`. -/
  | accept {p c t tip sub} (merge : Revision) :
      Step (.deciding p c t tip sub) (.merging p c merge sub)
  | continue {p c w h} (p' : Plan) (hl : c.continues + 1 ≤ lim.continues) :
      Step (.doubting p c w h) (.ordered (.continue p') { c with continues := c.continues + 1 } h)
  | resolve {p c paths sub} (merge : Revision) :
      Step (.resolving p c paths sub) (.merging p c merge sub)
  | drop {s} (why : String) (h : s.awaitsPlanner = true) : Step s (.dropped s.plan s.cause)
  -- its own merge
  | clean {p c merge sub} (h : failing merge = []) : Step (.merging p c merge sub) (.landed p)
  /-- The merge breaks the run set: a suite strike, and the worker builds
  again. -/
  | broken {p c merge sub} (h : failing merge ≠ []) (hl : c.suite + 1 < lim.suiteStrikes) :
      Step (.merging p c merge sub) (.serving p { c with suite := c.suite + 1 })
  | brokenOut {p c merge sub} (h : failing merge ≠ []) (hl : lim.suiteStrikes ≤ c.suite + 1) :
      Step (.merging p c merge sub) (.stopping p (.strikes .suite))
  | conflicted {p c merge sub} (paths : List String) (hl : c.conflicts + 1 ≤ lim.conflicts) :
      Step (.merging p c merge sub) (.resolving p { c with conflicts := c.conflicts + 1 } paths sub)
  | conflictOut {p c merge sub} (paths : List String) (hl : lim.conflicts < c.conflicts + 1) :
      Step (.merging p c merge sub) (.stopping p .conflicts)

/-- The states that hold a worker's move: the worker waits on the player. -/
def Task.holdsMove : Task → Bool
  | .deciding .. | .resolving .. | .merging .. | .asking .. => true
  | .doubting _ _ _ h | .ordered _ _ h | .posting _ _ h | .launching _ _ _ _ h => h.isSome
  | _ => false

/-- The roles a step sends a message to. A peer missing here waits through
the step, and an edge pairs the step with that peer only by `holds`. -/
def Step.peers : Step lim failing s t → List Role
  | .assign | .posted .. | .postRefused .. => [.foreman]
  | .postOut (h := h) .. => [.foreman, .planner] ++ (if h.isSome then [.worker] else [])
  | .postUnsure .. => [.foreman, .planner]
  | .launch | .restart => [.worker]
  | .submitted .. | .contested .. => [.worker, .foreman]
  | .failed .. => [.worker]
  | .failedOut .. | .contestOut .. | .blocked .. => [.worker, .planner]
  | .sentBack .. | .replied .. => [.foreman, .worker]
  | .sendBackRefused .. => [.foreman]
  | .sendBackOut .. | .overRounds .. => [.foreman, .worker, .planner]
  | .recommended .. | .doubted .. => [.foreman, .planner]
  | .trimmed .. | .accept .. | .continue .. | .resolve .. | .conflicted .. => [.planner]
  | .drop (s := s) .. => [.planner, .foreman] ++ (if s.holdsMove then [.worker] else [])
  | .clean .. => [.planner, .foreman, .worker]
  | .broken .. | .brokenOut .. | .conflictOut .. => [.planner, .worker]

/-- A task starts with nothing spent. -/
theorem start_spent {s : Task} (h : s.isStart = true) : s.spent = {} := by
  match s, h with
  | .ordered (.start _) ⟨0, 0, 0, 0, 0, 0, 0⟩ none, _ => rfl

/-- Every step keeps every counter within its limit. -/
theorem step_within (st : Step lim failing s t) (hs : s.spent.Within lim) :
    t.spent.Within lim := by
  have ⟨_, _, _, _, _, _, _⟩ := hs
  cases st <;> (try simp only [Task.spent] at *) <;> constructor <;> simp_all <;> omega

/-- Along every run from a start, every counter stays within its limit. -/
theorem run_within (r : Run (Step lim failing) s t) (hs : s.isStart = true) :
    t.spent.Within lim := by
  refine Run.keeps (fun x => x.spent.Within lim) (step_within lim failing) r ?_
  rw [start_spent hs]; constructor <;> simp

/-- No step takes the round past the send-backs allowed. -/
theorem rounds_bounded (r : Run (Step lim failing) s t) (hs : s.isStart = true) :
    t.round ≤ lim.reviewRounds :=
  (run_within lim failing r hs).rounds

/-- A task's place in one loop: each step forward lowers it, and each step
back spends. -/
def Task.stage : Task → Nat
  | .ordered .. => 10
  | .posting .. => 9
  | .launching .. => 8
  | .serving .. => 7
  | .asking _ _ (.contest ..) => 8
  | .asking _ _ (.review ..) => 6
  | .deciding _ _ t .. => if t = [] then 4 else 5
  | .doubting .. => 3
  | .resolving .. => 3
  | .merging .. => 2
  | .stopping .. => 1
  | .landed _ | .dropped .. => 0

/-- What a task has left to spend; a stopped or ended task, nothing. -/
def Task.left : Task → Nat
  | .ordered _ c _ | .posting _ c _ | .launching _ c .. | .serving _ c | .asking _ c _
  | .deciding _ c .. | .doubting _ c .. | .merging _ c .. | .resolving _ c .. => c.left lim
  | _ => 0

/-- What bounds a task's steps: what is left to spend, by loop, and the
place in the loop. -/
def Task.fuel (s : Task) : Nat := 11 * s.left lim + s.stage

theorem step_fuel (st : Step lim failing s t) : t.fuel lim < s.fuel lim := by
  cases st with
  | @doubted p c k why => cases k <;> simp [Task.fuel, Task.stage, Task.left]
  | @drop s why h =>
    cases s <;> simp_all [Task.awaitsPlanner, Task.fuel, Task.stage, Task.left] <;> (try split) <;> omega
  | _ => simp_all [Task.fuel, Task.stage, Task.left, Spent.left] <;> (try split) <;> omega

/-- Every task ends, stops or lands within `fuel` steps of its start. -/
theorem run_bounded (r : Run (Step lim failing) s t) : r.length ≤ s.fuel lim :=
  Run.bounded (Task.fuel lim) (step_fuel lim failing) r

theorem landed_is_final : Step lim failing (.landed p) t → False := by
  intro st; cases st with
  | drop _ h => simp [Task.awaitsPlanner] at h

theorem dropped_is_final : Step lim failing (.dropped p c) t → False := by
  intro st; cases st with
  | drop _ h => simp [Task.awaitsPlanner] at h

/-- A drop keeps the cause the task stopped for. -/
example : Step lim failing (.stopping p c) (.dropped p c) := .drop (s := .stopping p c) "" rfl

-- The architect's proposals --------------------------------------------------

variable (archOk : Revision → Except Reason Unit)

/-- One architect question, as the player judges its proposals. `n` counts
the refused ones. -/
inductive Review
  /-- Something to do: judge `rev`. The start is `n = 0`. -/
  | checking (rev : Revision) (n : Nat)
  /-- Refused `rev`; waits for the architect's next proposal. -/
  | awaitingProposal (rev : Revision) (n : Nat)
  /-- `rev` is on `main`. An end. -/
  | accepted (rev : Revision)
  /-- Refused `rev`, the last proposal judged, for `r`. An end. -/
  | closed (rev : Revision) (r : Reason)
  /-- The architect declined after refusal `rev`. An end. -/
  | withdrawn (rev : Revision)

def Review.isStart : Review → Bool
  | .checking _ 0 => true
  | _ => false

inductive ReviewStep : Review → Review → Type
  | accept {rev n} (h : archOk rev = .ok ()) : ReviewStep (.checking rev n) (.accepted rev)
  | refuse {rev n} (r : Reason) (h : archOk rev = .error r) (hl : n + 1 < lim.proposals) :
      ReviewStep (.checking rev n) (.awaitingProposal rev (n + 1))
  | refuseLast {rev n} (r : Reason) (h : archOk rev = .error r)
      (hl : lim.proposals ≤ n + 1) : ReviewStep (.checking rev n) (.closed rev r)
  | proposed {rev n} (rev' : Revision) :
      ReviewStep (.awaitingProposal rev n) (.checking rev' n)
  | withdraw {rev n} : ReviewStep (.awaitingProposal rev n) (.withdrawn rev)

/-- Only a revision the oracle passes reaches `main`. -/
theorem accepted_passes (st : ReviewStep lim archOk s (.accepted rev)) :
    archOk rev = .ok () := by
  cases st; assumption

def Review.fuel : Review → Nat
  | .checking _ n => 2 * (lim.proposals - n) + 2
  | .awaitingProposal _ n => 2 * (lim.proposals - n) + 3
  | _ => 0

/-- Every question's review ends within `fuel` steps. -/
theorem review_bounded (r : Run (ReviewStep lim archOk) s t) : r.length ≤ s.fuel lim :=
  Run.bounded (Review.fuel lim) (fun st => by cases st <;> simp [Review.fuel] <;> omega) r

end Player
