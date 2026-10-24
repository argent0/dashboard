import Agents.Hub

/-!
Each role's state is a view of the hub: a function of the status and the
counters. Each role's part in a legal commit is its own steps between the
view before and the view after (`Peer`, or `Present` for the planner, who
is there from the start), and `Hub.project` chains the parts along the
log. So every legal log is, for every role, a run of its own machine
(`Goal.worker`, `Goal.foreman`, `Goal.planner`, `Goal.architect`): the
check is that each part type-checks, for every commit, once.
-/
namespace HubLog
open Kernel

variable (lim : Limits) (o : Oracles)

-- The views ------------------------------------------------------------------

def Phase.worker (s : Spent) : Phase → Option Worker.State
  | .posting _ (some (w, m)) | .doubting _ _ (some (w, m)) => some (.waiting w.job s.rounds m)
  | .serving w => some (.building w.job s.rounds)
  | .asking w k => some (.waiting w.job s.rounds k.held)
  | .deciding w _ _ sub | .resolving w _ sub => some (.waiting w.job s.rounds (.submit sub))
  | .stopping _ _ c true => some (.dropped c)
  | .landed _ => some .accepted
  | .dropped _ _ _ (some c) => some (.dropped c)
  | _ => none

def Phase.foreman (s : Spent) : Phase → Option Foreman.State
  | .posting p _ => some (.toPost ⟨p, s.rounds⟩)
  | .serving w | .deciding w .. | .resolving w .. => some (.posted w.assignment w.job.prompt)
  | .asking w k => some (.owing w.assignment w.job.prompt k)
  | .doubting a why _ => some (.unsure a why)
  | .stopping a pr _ _ => some (.posted a pr)
  | .landed _ => some .accepted
  | .dropped _ c _ _ => some (.dropped c)
  | _ => none

def Status.planner (g : Planner.Goal) (st : Status) : Planner.State :=
  match st.phase with
  | .toPlan => .toPlan g st.answers
  | .designing what => .designing g what st.answers
  | .abandoned why => .abandoned g why
  | .posting p _ => .running g p
  | .serving w | .asking w _ => .running g w.job.plan
  | .deciding w t _ _ => .advised g w.job.plan (.recommend t)
  | .doubting a why _ => .advised g a.plan (.doubt why)
  | .stopping a _ c _ => .advised g a.plan (.stopped c)
  | .resolving w paths _ => .advised g w.job.plan (.conflict paths)
  | .landed p => .landed p
  | .dropped p c why _ => .dropped g p c why

/-- The architect's end an answer records. -/
def _root_.Planner.Answer.final : Planner.Answer → Architect.State
  | .designed w rev => .designed (.need w) rev
  | .refused w r => .refused (.need w) r
  | .declined w why => .declined (.need w) why

def Phase.asked (n : Nat) : Phase → Option Architect.State
  | .designing what => some (.asked (.need what) n)
  | _ => none

/-- The architect of question `k`, the `k`-th need of the goal: its end
once answered, its work while the hub designs it, absent before. -/
def Status.architect (s : Spent) (k : Nat) (st : Status) : Option Architect.State :=
  match st.answers[k]? with
  | some a => some a.final
  | none => if k = st.answers.length then st.phase.asked s.refusals else none

-- The parts ------------------------------------------------------------------

macro "branch " h:ident " : " e:term : tactic =>
  `(tactic| by_cases $h : $e <;> simp only [$h:ident, ↓reduceIte])

/-- The worker's part in each legal commit. -/
def workerPart (h : Allowed lim o ph s c) (ok : Ok ph s) :
    Peer Worker.Step Worker.State.isStart (ph.worker s) ((ph.next lim o s c).worker (s.add o c)) := by
  cases h with
  | ask | plan | abandon | decline => exact .inr ⟨rfl⟩
  | propose rev =>
    exact .inr ⟨by simp only [Phase.next, Spent.add]; cases o.archOk rev <;> dsimp only <;> (try split) <;> rfl⟩
  | @post p held s rev =>
    simp only [Phase.next, Spent.add]
    branch hf : o.failing rev = []
    · branch hl : s.posts + 1 < lim.postStrikes
      · exact .inr ⟨rfl⟩
      · rcases held with _ | ⟨w, m⟩
        · exact .inr ⟨rfl⟩
        · exact .inl (Steps.one (.answer (.dropped (.strikes .post))))
    · rcases held with _ | ⟨w, m⟩
      · exact .inl ⟨by simp [Worker.State.isStart, ok rfl]⟩
      · exact .inl (Steps.one (.answer (.restarted ⟨p, rev, o.failing rev, w.job.notes⟩)))
  | @postUnsure p held s why =>
    rcases held with _ | ⟨w, m⟩ <;> exact .inr ⟨rfl⟩
  | submit rev =>
    simp only [Phase.next, Spent.add]
    branch hf : o.failing rev = []
    · exact .inl (Steps.one (.move (.submit rev)))
    · branch hl : s.suite + 1 < lim.suiteStrikes
      · exact .inl (Steps.two (.move (.submit rev)) (.answer (.refused .suite 0)))
      · exact .inl (Steps.two (.move (.submit rev)) (.answer (.dropped (.strikes .suite))))
  | contest t why =>
    simp only [Phase.next, Spent.add]
    branch hl : s.contests + 1 ≤ lim.contestRounds
    · exact .inl (Steps.one (.move (.contest t why)))
    · exact .inl (Steps.two (.move (.contest t why)) (.answer (.dropped .contestRounds)))
  | block why => exact .inl (Steps.two (.move (.block why)) (.answer (.dropped .blocked)))
  | sendBack note =>
    simp only [Phase.next, Spent.add]
    branch hf : o.failing note = []
    · branch hl : s.reviews + 1 < lim.reviewStrikes
      · exact .inr ⟨rfl⟩
      · exact .inl (Steps.one (.answer (.dropped (.strikes .review))))
    · branch hl : s.rounds + 1 ≤ lim.reviewRounds
      · exact .inl (Steps.one (.answer (.sentBack note (o.failing note))))
      · exact .inl (Steps.one (.answer (.dropped .reviewRounds)))
  | recommend | trim => exact .inr ⟨rfl⟩
  | @askUnsure w k s why => cases k <;> exact .inr ⟨rfl⟩
  | reply rev => exact .inl (Steps.one (.answer (.replied rev)))
  | @«continue» a why held s p hl => rcases held with _ | ⟨w, m⟩ <;> exact .inr ⟨rfl⟩
  | @accept w t tip sub s | @resolve w _ sub s tip =>
    simp only [Phase.next, Spent.add, land, Spent.land]
    rcases hm : o.merge tip with m | paths
    · simp only []
      branch hf : o.failing m = []
      · exact .inl (Steps.one (.answer .accepted))
      · branch hl : s.suite + 1 < lim.suiteStrikes
        · exact .inl (Steps.one (.answer (.refused .suite 0)))
        · exact .inl (Steps.one (.answer (.dropped (.strikes .suite))))
    · simp only []
      branch hl : s.conflicts + 1 ≤ lim.conflicts
      · exact .inr ⟨rfl⟩
      · exact .inl (Steps.one (.answer (.dropped .conflicts)))
  | @drop ph s why hd =>
    cases ph with
    | deciding | resolving => exact .inl (Steps.one (.answer (.dropped .retired)))
    | doubting a w held =>
      rcases held with _ | ⟨w, m⟩
      · exact .inr ⟨rfl⟩
      · exact .inl (Steps.one (.answer (.dropped .retired)))
    | stopping a pr c wk => cases wk <;> exact .inr ⟨rfl⟩
    | _ => simp [Phase.awaitsPlanner] at hd

/-- The foreman's part in each legal commit. -/
def foremanPart (h : Allowed lim o ph s c) :
    Peer Foreman.Step Foreman.State.isStart (ph.foreman s) ((ph.next lim o s c).foreman (s.add o c)) := by
  cases h with
  | ask | abandon | decline => exact .inr ⟨rfl⟩
  | plan => exact .inl ⟨rfl⟩
  | propose rev =>
    exact .inr ⟨by simp only [Phase.next, Spent.add]; cases o.archOk rev <;> dsimp only <;> (try split) <;> rfl⟩
  | @post p held s rev =>
    simp only [Phase.next, Spent.add]
    branch hf : o.failing rev = []
    · branch hl : s.posts + 1 < lim.postStrikes
      · exact .inl (Steps.two (.post rev) (.repost ⟨p, s.rounds⟩))
      · exact .inl (Steps.one (.post rev))
    · exact .inl (Steps.one (.post rev))
  | postUnsure why => exact .inl (Steps.one (.postUnsure why))
  | @submit w s rev =>
    simp only [Phase.next, Spent.add]
    branch hf : o.failing rev = []
    · exact .inl (Steps.one (.asked (.review s.rounds rev)))
    · branch hl : s.suite + 1 < lim.suiteStrikes
      · exact .inr ⟨rfl⟩
      · exact .inr ⟨rfl⟩
  | @contest w s t why =>
    simp only [Phase.next, Spent.add]
    branch hl : s.contests + 1 ≤ lim.contestRounds
    · exact .inl (Steps.one (.asked (.contest s.rounds t why)))
    · exact .inr ⟨rfl⟩
  | block => exact .inr ⟨rfl⟩
  | @sendBack w n sub s note =>
    simp only [Phase.next, Spent.add]
    branch hf : o.failing note = []
    · branch hl : s.reviews + 1 < lim.reviewStrikes
      · exact .inl (Steps.two (.respond (.sentBack note)) (.asked (.review n sub)))
      · exact .inl (Steps.one (.respond (.sentBack note)))
    · branch hl : s.rounds + 1 ≤ lim.reviewRounds
      · exact .inl (Steps.one (.respond (.sentBack note)))
      · exact .inl (Steps.one (.respond (.sentBack note)))
  | recommend t => exact .inl (Steps.one (.respond (.recommend t)))
  | reply rev => exact .inl (Steps.one (.respond (.replied rev)))
  | @askUnsure w k s why => cases k <;> exact .inl (Steps.one (.respond (.unsure why)))
  | trim => exact .inr ⟨rfl⟩
  | @«continue» a why held s p hl => exact .inl (Steps.one (.revised ⟨p, s.rounds⟩))
  | @accept w t tip sub s | @resolve w _ sub s tip =>
    simp only [Phase.next, Spent.add, land, Spent.land]
    rcases hm : o.merge tip with m | paths
    · simp only []
      branch hf : o.failing m = []
      · exact .inl (Steps.one .accept)
      · branch hl : s.suite + 1 < lim.suiteStrikes
        · exact .inr ⟨rfl⟩
        · exact .inr ⟨rfl⟩
    · simp only []
      branch hl : s.conflicts + 1 ≤ lim.conflicts
      · exact .inr ⟨rfl⟩
      · exact .inr ⟨rfl⟩
  | @drop ph s why hd =>
    cases ph with
    | deciding | resolving | doubting => exact .inl (Steps.one (.drop .retired rfl))
    | stopping a pr c wk => exact .inl (Steps.one (.drop c rfl))
    | _ => simp [Phase.awaitsPlanner] at hd

/-- The planner's part in each legal commit. -/
def plannerPart (g : Planner.Goal) (h : Allowed lim o ph s c) :
    Present Planner.Step (Status.planner g ⟨ph, ans⟩)
      ((Status.next lim o ⟨ph, ans⟩ s c).planner g) := by
  cases h with
  | ask what => exact .inl (Steps.one (.need what))
  | plan p => exact .inl (Steps.one (.plan p))
  | abandon why => exact .inl (Steps.one (.abandon why))
  | @propose what s rev =>
    simp only [Status.next, Phase.next, answer?]
    rcases ha : o.archOk rev with r | u
    · simp only []
      branch hl : s.refusals + 1 < lim.proposals
      · exact .inr ⟨rfl⟩
      · exact .inl (Steps.one (.refused r))
    · exact .inl (Steps.one (.designed rev))
  | decline why => exact .inl (Steps.one (.declined why))
  | @post p held s rev =>
    simp only [Status.next, Phase.next]
    branch hf : o.failing rev = []
    · branch hl : s.posts + 1 < lim.postStrikes
      · exact .inr ⟨rfl⟩
      · exact .inl (Steps.one (.report (.stopped (.strikes .post)) rfl))
    · exact .inr ⟨rfl⟩
  | postUnsure why => exact .inl (Steps.one (.report (.doubt why) rfl))
  | @submit w s rev =>
    simp only [Status.next, Phase.next]
    branch hf : o.failing rev = []
    · exact .inr ⟨rfl⟩
    · branch hl : s.suite + 1 < lim.suiteStrikes
      · exact .inr ⟨rfl⟩
      · exact .inl (Steps.one (.report (.stopped (.strikes .suite)) rfl))
  | @contest w s t why =>
    simp only [Status.next, Phase.next]
    branch hl : s.contests + 1 ≤ lim.contestRounds
    · exact .inr ⟨rfl⟩
    · exact .inl (Steps.one (.report (.stopped .contestRounds) rfl))
  | block why => exact .inl (Steps.one (.report (.stopped .blocked) rfl))
  | @sendBack w n sub s note =>
    simp only [Status.next, Phase.next]
    branch hf : o.failing note = []
    · branch hl : s.reviews + 1 < lim.reviewStrikes
      · exact .inr ⟨rfl⟩
      · exact .inl (Steps.one (.report (.stopped (.strikes .review)) rfl))
    · branch hl : s.rounds + 1 ≤ lim.reviewRounds
      · exact .inr ⟨rfl⟩
      · exact .inl (Steps.one (.report (.stopped .reviewRounds) rfl))
  | recommend t => exact .inl (Steps.one (.report (.recommend t) rfl))
  | reply => exact .inr ⟨rfl⟩
  | @askUnsure w k s why => cases k <;> exact .inl (Steps.one (.report (.doubt why) rfl))
  | trim rev ht => exact .inl (Steps.one (.decide (.trim rev ht)))
  | «continue» p => exact .inl (Steps.one (.decide (.continue p)))
  | @accept w t tip sub s =>
    simp only [Status.next, Phase.next, land]
    rcases hm : o.merge tip with m | paths
    · simp only []
      branch hf : o.failing m = []
      · exact .inl (Steps.two (.decide .accept) .land)
      · branch hl : s.suite + 1 < lim.suiteStrikes
        · exact .inl (Steps.two (.decide .accept) .reopened)
        · exact .inl (.mk (.decide .accept) (.two .reopened (.report (.stopped (.strikes .suite)) rfl)))
    · simp only []
      branch hl : s.conflicts + 1 ≤ lim.conflicts
      · exact .inl (Steps.two (.decide .accept) (.conflict paths))
      · exact .inl (.mk (.decide .accept) (.two .reopened (.report (.stopped .conflicts) rfl)))
  | @resolve w paths sub s rev =>
    simp only [Status.next, Phase.next, land]
    rcases hm : o.merge rev with m | paths'
    · simp only []
      branch hf : o.failing m = []
      · exact .inl (Steps.two (.decide (.resolve rev)) .land)
      · branch hl : s.suite + 1 < lim.suiteStrikes
        · exact .inl (Steps.two (.decide (.resolve rev)) .reopened)
        · exact .inl (.mk (.decide (.resolve rev))
            (.two .reopened (.report (.stopped (.strikes .suite)) rfl)))
    · simp only []
      branch hl : s.conflicts + 1 ≤ lim.conflicts
      · exact .inl (Steps.two (.decide (.resolve rev)) (.conflict paths'))
      · exact .inl (.mk (.decide (.resolve rev)) (.two .reopened (.report (.stopped .conflicts) rfl)))
  | @drop ph s why hd =>
    cases ph with
    | deciding | resolving | doubting | stopping => exact .inl (Steps.one (.decide (.drop why)))
    | _ => simp [Phase.awaitsPlanner] at hd

/-- Question `k` reads the same before and after a commit that answers
nothing, from a phase that designs nothing, into one that designs nothing. -/
theorem architect_holds {s s' : Spent} {k : Nat} {ans : List Planner.Answer} {ph ph' : Phase}
    (h : ph.asked s.refusals = none) (h' : ph'.asked s'.refusals = none) :
    Status.architect s' k ⟨ph', ans⟩ = Status.architect s k ⟨ph, ans⟩ := by
  simp only [Status.architect, h, h']

/-- Question `k`'s view, when it is the one the hub designs. -/
theorem architect_at {s : Spent} {ans : List Planner.Answer} {ph : Phase} :
    Status.architect s ans.length ⟨ph, ans⟩ = ph.asked s.refusals := by
  simp [Status.architect]

/-- Question `k`'s view, once answered by `x`. -/
theorem architect_answered {s : Spent} {ans : List Planner.Answer} {ph : Phase} {x} :
    Status.architect s ans.length ⟨ph, ans ++ [x]⟩ = some x.final := by
  simp [Status.architect]

/-- Any other question reads the same while the answers stay. -/
theorem architect_same {s s' : Spent} {k : Nat} {ans : List Planner.Answer} {ph ph' : Phase}
    (hk : k ≠ ans.length) :
    Status.architect s' k ⟨ph', ans⟩ = Status.architect s k ⟨ph, ans⟩ := by
  simp [Status.architect, hk]

/-- Any other question reads the same when the hub records an answer to
this one, and no question is designed after it. -/
theorem architect_append {s s' : Spent} {k : Nat} {ans : List Planner.Answer} {ph ph' : Phase}
    {x} (hk : k ≠ ans.length) (h' : ph'.asked s'.refusals = none) :
    Status.architect s' k ⟨ph', ans ++ [x]⟩ = Status.architect s k ⟨ph, ans⟩ := by
  rcases Nat.lt_or_gt_of_ne hk with hk | hk
  · simp [Status.architect, List.getElem?_append_left hk, List.getElem?_eq_getElem hk]
  · have h1 : (ans ++ [x])[k]? = none := List.getElem?_eq_none (by simp; omega)
    have h2 : ans[k]? = none := List.getElem?_eq_none (by omega)
    simp only [Status.architect, h1, h2, h', Nat.ne_of_gt hk, ↓reduceIte]
    split <;> rfl

/-- The architect of question `k`'s part in each legal commit. -/
def architectPart (k : Nat) (h : Allowed lim o ph s c) :
    Peer (Architect.Step lim.proposals) Architect.State.isStart
      (Status.architect s k ⟨ph, ans⟩)
      (Status.architect (s.add o c) k (Status.next lim o ⟨ph, ans⟩ s c)) := by
  cases h with
  | @ask s what hl =>
    simp only [Status.next, answer?, Phase.next, Spent.add]
    by_cases hk : k = ans.length
    · subst hk; rw [architect_at, architect_at]; exact .inl ⟨rfl⟩
    · exact .inr ⟨architect_same hk⟩
  | @propose what s rev =>
    simp only [Status.next, answer?, Phase.next, Spent.add]
    rcases ha : o.archOk rev with r | u
    · simp only []
      branch hl : s.refusals + 1 < lim.proposals
      · by_cases hk : k = ans.length
        · subst hk; rw [architect_at, architect_at]
          exact .inl (Steps.two (.propose rev) (.refusedAgain r hl))
        · exact .inr ⟨architect_same hk⟩
      · by_cases hk : k = ans.length
        · subst hk; rw [architect_at, architect_answered]
          exact .inl (Steps.two (.propose rev) (.refusedLast r (by omega)))
        · exact .inr ⟨architect_append hk rfl⟩
    · simp only []
      by_cases hk : k = ans.length
      · subst hk; rw [architect_at, architect_answered]
        exact .inl (Steps.two (.propose rev) .accepted)
      · exact .inr ⟨architect_append hk rfl⟩
  | @decline what s why =>
    simp only [Status.next, answer?, Phase.next, Spent.add]
    by_cases hk : k = ans.length
    · subst hk; rw [architect_at, architect_answered]; exact .inl (Steps.one (.decline why))
    · exact .inr ⟨architect_append hk rfl⟩
  | @drop ph s why hd =>
    cases ph <;> simp [Phase.awaitsPlanner] at hd <;> exact .inr ⟨rfl⟩
  | _ =>
    refine .inr ⟨architect_holds ?_ ?_⟩
    all_goals (try simp only [Phase.next, Spent.add, land, Spent.land])
    all_goals (repeat' split) <;> rfl

-- The projections ------------------------------------------------------------

/-- The worker's view of a log. -/
abbrev workerView (l : Log) : Option Worker.State := (status lim o l).phase.worker (spent o l)
abbrev foremanView (l : Log) : Option Foreman.State := (status lim o l).phase.foreman (spent o l)
abbrev plannerView (g : Planner.Goal) (l : Log) : Planner.State := (status lim o l).planner g
abbrev architectView (k : Nat) (l : Log) : Option Architect.State :=
  (status lim o l).architect (spent o l) k

/-- No question has an architect before the log starts. -/
theorem architect_empty (k : Nat) : architectView lim o k [] = none := by
  simp only [architectView, status, Status.architect, List.getElem?_nil]; by_cases k = 0 <;> simp [*, Phase.asked]

/-- Every legal log is a run of the worker's machine, from no worker. -/
def Goal.worker {l : Log} (h : Goal lim o l) :
    Run (Peer Worker.Step Worker.State.isStart) none (workerView lim o l) :=
  Hub.project (workerView lim o) _ (fun h lg => workerPart lim o lg (Hub.ok lim o h)) h

def Goal.foreman {l : Log} (h : Goal lim o l) :
    Run (Peer Foreman.Step Foreman.State.isStart) none (foremanView lim o l) :=
  Hub.project (foremanView lim o) _ (fun _ lg => foremanPart lim o lg) h

/-- Every legal log is a run of the planner's machine, from its start. -/
def Goal.planner (g : Planner.Goal) {l : Log} (h : Goal lim o l) :
    Run (Present Planner.Step) (.toPlan g []) (plannerView lim o g l) :=
  Hub.project (plannerView lim o g) _ (fun _ lg => plannerPart lim o g lg) h

/-- Every legal log is a run of each question's architect, from absent. -/
def Goal.architect (k : Nat) {l : Log} (h : Goal lim o l) :
    Run (Peer (Architect.Step lim.proposals) Architect.State.isStart)
      (architectView lim o k []) (architectView lim o k l) :=
  Hub.project (architectView lim o k) _ (fun _ lg => architectPart lim o k lg) h

end HubLog
