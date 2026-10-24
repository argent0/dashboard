import Agents.Planner
import Agents.Architect
import Agents.Foreman
import Agents.Worker

/-!
The hub of one goal (decision 12). Every event of the loop is a commit to
the hub, pushed by one role; the log holds them newest first. The player is
the hub's sole writer: what it does, the checks and the merges, is the
effect a legal commit has on the hub, read through the oracles (`Oracles`).

The hub's status is a function of the log: `status l` folds each commit's
effect into where the goal stands, the architect's answers, `main`, and
the counters (spec §10). A commit is legal when its author may make it at
the status of the log before it (`Allowed`): the role that pushes it, a
design on `main`, a trim on the tip, a resolution of the recorded merge,
a continuation of the same task. Its effect then reads the oracles: a
commit outside its author's paths, a post whose tests pass, a submission
that fails the run set and a send-back whose test passes are refused for a
strike (`Commit.refusal`); a landing merges onto `main` through `merge`;
a proposal passes `archOk` or is refused. Past a limit, the effect is the
stop. `Hub Legal l` is a legal log: each commit is legal at the log before
it. Each role's state is a view of it (`Agents/Views.lean`).
-/
namespace HubLog
open Kernel

/-- What merging a revision onto `main` gives: the merge commit, or the
paths in conflict. -/
inductive Merge
  | onto (merge : Revision)
  | conflict (paths : List String)

/-- A commit to the hub of one goal. -/
inductive Commit
  -- the planner's
  /-- Names a need to the architect. -/
  | ask (what : String)
  | plan (p : Plan)
  | abandon (why : String)
  /-- Lands `tip`, the work the foreman recommends (or its trim). -/
  | accept (tip : Revision)
  /-- Drops the checks the foreman names (spec §6.5). -/
  | trim (rev : Revision)
  | continue (p : Plan)
  /-- Lands `merge`, the planner's resolution of a conflict. -/
  | resolve (merge : Revision)
  | drop (why : String)
  -- the architect's
  | propose (rev : Revision)
  | decline (why : String)
  -- the foreman's
  /-- The prompt and the failing tests. -/
  | post (rev : Revision)
  | unsure (why : String)
  /-- A note on the submission, with a new failing test. -/
  | sendBack (note : Revision)
  | recommend (trim : List String)
  | reply (rev : Revision)
  -- the worker's
  | submit (rev : Revision)
  | contest (test why : String)
  | block (why : String)

def Commit.author : Commit → Role
  | .ask _ | .plan _ | .abandon _ | .accept _ | .trim _ | .continue _ | .resolve _
  | .drop _ => .planner
  | .propose _ | .decline _ => .architect
  | .post _ | .unsure _ | .sendBack _ | .recommend _ | .reply _ => .foreman
  | .submit _ | .contest .. | .block _ => .worker

/-- A commit as it reaches the hub: pushed by `role`, on a ref of its own. -/
structure Push where
  role : Role
  commit : Commit

/-- The log, newest first. -/
abbrev Log := List Push

/-- What the hub reads a commit through. `base`: `main` when the goal
starts. `parents rev`: the commits `rev` sits on. `failing rev`: the checks
of the run set that fail at `rev`. `merge main rev`: merging `rev` onto
`main`. `archOk rev`: `arch check` and the ownership rules at `rev`.
`owns r rev`: the reason `rev`, pushed by `r`, leaves `r`'s paths, if it
does (spec §5, §8.3, §17): the foreman's `Foreman.inBounds`, the worker's
pins (`pins`) and scope (`scope`), the planner's trim and resolution
(`ownership`). Open: these path rules become rules over the tree in
stage 3. -/
structure Oracles where
  base : Revision
  parents : Revision → List Revision
  failing : Revision → List String
  merge : Revision → Revision → Merge
  archOk : Revision → Except Reason Unit
  owns : Role → Revision → Option Reason

/-- The hub's limits; `strikes` is the roadmap's `[limits.strikes]`. Open:
`continues`, `conflicts`, `proposals` and `needs` have no roadmap key yet. -/
structure Limits where
  /-- Send-backs allowed per task. -/
  reviewRounds : Nat := 2
  /-- Contests allowed per task. -/
  contestRounds : Nat := 2
  /-- A strike at its reason's count stops the task. -/
  strikes : Dashboard.StrikeLimits := {}
  /-- Orders to continue allowed per task. -/
  continues : Nat := 2
  /-- Merge conflicts allowed per task. -/
  conflicts : Nat := 2
  /-- Proposals judged per architect question. -/
  proposals : Nat := 3
  /-- Questions to the architect per goal. -/
  needs : Nat := 2

/-- The strike limit of reason `r`. -/
def Limits.strike (lim : Limits) : Reason → Nat
  | .ownership => lim.strikes.ownership
  | .pins => lim.strikes.pins
  | .scope => lim.strikes.scope
  | .post => lim.strikes.post
  | .review => lim.strikes.review
  | .suite => lim.strikes.suite

/-- A legal log: each commit is legal at the log before it. -/
inductive Hub (Legal : Log → Push → Type) : Log → Type
  | empty : Hub Legal []
  | commit {l c} : Hub Legal l → Legal l c → Hub Legal (c :: l)

def Hub.length {Legal : Log → Push → Type} : {l : Log} → Hub Legal l → Nat
  | _, .empty => 0
  | _, .commit h _ => h.length + 1

/-- A role's view along a legal log: a view `v` of each log, and the role's
part `R` in each commit, give a run of parts from the empty log's view to
the log's. This is the projection; that `part` type-checks is the check. -/
def Hub.project {Legal : Log → Push → Type} {V : Type} (v : Log → V) (R : V → V → Type)
    (part : ∀ {l c}, Hub Legal l → Legal l c → R (v l) (v (c :: l))) :
    {l : Log} → Hub Legal l → Run R (v []) (v l)
  | _, .empty => .stay
  | _, .commit h lg => (Hub.project v R part h).snoc (part h lg)

/-- The worker's move a foreman ask answers. -/
def _root_.Foreman.Ask.held : Foreman.Ask → Worker.Move
  | .review _ sub => .submit sub
  | .contest _ t w => .contest t w

/-- A task's work on the hub: the worker's job, and the round of the
foreman's post. -/
structure Work where
  job : Worker.Job
  postedAt : Nat

def Work.assignment (w : Work) : Foreman.Assignment := ⟨w.job.plan, w.postedAt⟩

/-- The work after a note that makes `fl` fail. -/
def Work.sentBack (w : Work) (note : Revision) (fl : List String) : Work :=
  { w with job := { w.job with failing := fl, notes := w.job.notes ++ [note] } }

def Work.replied (w : Work) (rev : Revision) : Work :=
  { w with job := { w.job with notes := w.job.notes ++ [rev] } }

/-- A worker's move the hub holds, with its work: the worker waits on it. -/
abbrev Held := Option (Work × Worker.Move)

def notesOf : Held → List Revision
  | some (w, _) => w.job.notes
  | none => []

/-- Where the goal stands on the hub. -/
inductive Phase
  /-- Something to plan. The start. -/
  | toPlan
  /-- The architect designs `what`. -/
  | designing (what : String)
  /-- An end. -/
  | abandoned (why : String)
  /-- The foreman posts for `p`. -/
  | posting (p : Plan) (held : Held)
  /-- The worker builds. -/
  | serving (w : Work)
  /-- The foreman owes the answer to `k`. -/
  | asking (w : Work) (k : Foreman.Ask)
  /-- The planner decides on the recommendation for `sub`; `tip` is what
  an accept lands: `sub`, or the trim on it. -/
  | deciding (w : Work) (trim : List String) (tip sub : Revision)
  /-- The planner decides on the foreman's doubt. -/
  | doubting (a : Foreman.Assignment) (why : String) (held : Held)
  /-- Stopped for `cause`; `worker` tells whether a worker was there. -/
  | stopping (a : Foreman.Assignment) (prompt : Revision) (cause : Cause) (worker : Bool)
  /-- The planner resolves a conflict on `paths`: merging `tip` onto
  `onto`, the `main` it was recorded against (spec §11). The worker waits
  on `sub`. -/
  | resolving (w : Work) (paths : List String) (sub tip onto : Revision)
  /-- On `main`. An end. -/
  | landed (p : Plan)
  /-- Dropped for `cause`; `worker` holds the worker's end cause. An end. -/
  | dropped (p : Plan) (cause : Cause) (why : String) (worker : Option Cause)

def Phase.awaitsPlanner : Phase → Bool
  | .deciding .. | .doubting .. | .stopping .. | .resolving .. => true
  | .toPlan | .designing _ | .abandoned _ | .posting .. | .serving _ | .asking .. | .landed _
  | .dropped .. => false

/-- The ends: no commit is legal there. -/
def Phase.final : Phase → Bool
  | .abandoned _ | .landed _ | .dropped .. => true
  | .toPlan | .designing _ | .posting .. | .serving _ | .asking .. | .deciding .. | .doubting ..
  | .stopping .. | .resolving .. => false

/-- The stop of work `w` for `c`. -/
def Work.stop (w : Work) (c : Cause) : Phase := .stopping w.assignment w.job.prompt c true

/-- Strikes, one counter per reason (spec §10). They never reset. -/
structure Strikes where
  ownership : Nat := 0
  pins : Nat := 0
  scope : Nat := 0
  post : Nat := 0
  review : Nat := 0
  suite : Nat := 0

def Strikes.get (k : Strikes) : Reason → Nat
  | .ownership => k.ownership
  | .pins => k.pins
  | .scope => k.scope
  | .post => k.post
  | .review => k.review
  | .suite => k.suite

/-- One more strike for `r`. -/
def Strikes.add (k : Strikes) : Reason → Strikes
  | .ownership => { k with ownership := k.ownership + 1 }
  | .pins => { k with pins := k.pins + 1 }
  | .scope => { k with scope := k.scope + 1 }
  | .post => { k with post := k.post + 1 }
  | .review => { k with review := k.review + 1 }
  | .suite => { k with suite := k.suite + 1 }

/-- What a goal has spent: each a count of commits in the log. -/
structure Spent where
  /-- Questions asked. -/
  needs : Nat := 0
  /-- Proposals refused since the last question. -/
  refusals : Nat := 0
  /-- Send-backs whose test fails: the round. -/
  rounds : Nat := 0
  contests : Nat := 0
  strikes : Strikes := {}
  continues : Nat := 0
  conflicts : Nat := 0

/-- The hub's status: where the goal stands, the architect's answers
(oldest first), `main`, and what the goal has spent. -/
structure Status where
  phase : Phase := .toPlan
  answers : List Planner.Answer := []
  main : Revision
  spent : Spent := {}

variable (lim : Limits) (o : Oracles)

/-- The reason the hub refuses commit `c`, if any (spec §10): its author's
paths first (`owns`), then its evidence. A refused commit counts a strike
for its reason. -/
def Commit.refusal : Commit → Option Reason
  | .post rev => match o.owns .foreman rev with
    | some r => some r
    | none => if o.failing rev = [] then some .post else none
  | .submit rev => match o.owns .worker rev with
    | some r => some r
    | none => if o.failing rev = [] then none else some .suite
  | .sendBack note => match o.owns .foreman note with
    | some r => some r
    | none => if o.failing note = [] then some .review else none
  | .reply rev => o.owns .foreman rev
  | .ask _ | .plan _ | .abandon _ | .accept _ | .trim _ | .continue _ | .resolve _ | .drop _
  | .propose _ | .decline _ | .unsure _ | .recommend _ | .contest .. | .block _ => none

/-- What landing `rev` onto `main` `m` spends. -/
def Spent.land (s : Spent) (m rev : Revision) : Spent :=
  match o.merge m rev with
  | .onto m' => if o.failing m' = [] then s else { s with strikes := s.strikes.add .suite }
  | .conflict _ => { s with conflicts := s.conflicts + 1 }

/-- What a commit the hub takes spends, at `main` `m`. -/
def Spent.spend (s : Spent) (m : Revision) : Commit → Spent
  | .ask _ => { s with needs := s.needs + 1, refusals := 0 }
  | .propose rev => match o.archOk rev with
    | .ok _ => s
    | .error _ => { s with refusals := s.refusals + 1 }
  | .contest .. => { s with contests := s.contests + 1 }
  | .sendBack _ => { s with rounds := s.rounds + 1 }
  | .continue _ => { s with continues := s.continues + 1 }
  | .accept rev | .resolve rev => s.land o m rev
  | .plan _ | .abandon _ | .trim _ | .drop _ | .decline _ | .post _ | .unsure _ | .recommend _
  | .reply _ | .submit _ | .block _ => s

/-- What a commit spends, at `main` `m`: a strike if refused. -/
def Spent.add (s : Spent) (m : Revision) (c : Commit) : Spent :=
  match c.refusal o with
  | some r => { s with strikes := s.strikes.add r }
  | none => s.spend o m c

/-- A strike for `r`: `stay` while strikes are left, else `stop`. -/
def strike (s : Spent) (r : Reason) (stay stop : Phase) : Phase :=
  if s.strikes.get r + 1 < lim.strike r then stay else stop

/-- Landing `tip` onto `main` `m` for work `w`, whose worker waits on `sub`:
on `main`, a broken run set, or a conflict recorded against `m`. -/
def land (s : Spent) (m : Revision) (w : Work) (sub tip : Revision) : Phase :=
  match o.merge m tip with
  | .onto m' =>
    if o.failing m' = [] then .landed w.job.plan
    else strike lim s .suite (.serving w) (w.stop (.strikes .suite))
  | .conflict paths =>
    if s.conflicts + 1 ≤ lim.conflicts then .resolving w paths sub tip m
    else w.stop .conflicts

/-- A commit's effect on the phase, at `main` `m`. Each phase names the
commits it takes; any other leaves it as it is, and `Allowed` admits none. -/
def Phase.next (s : Spent) (m : Revision) : Phase → Commit → Phase
  | .toPlan, .ask what => .designing what
  | .toPlan, .plan p => .posting p none
  | .toPlan, .abandon why => .abandoned why
  | .toPlan, _ => .toPlan
  | .designing what, .propose rev => match o.archOk rev with
    | .ok _ => .toPlan
    | .error _ => if s.refusals + 1 < lim.proposals then .designing what else .toPlan
  | .designing _, .decline _ => .toPlan
  | .designing what, _ => .designing what
  | .abandoned why, _ => .abandoned why
  | .posting p h, .post rev => match Commit.refusal o (.post rev) with
    | some r => strike lim s r (.posting p h) (.stopping ⟨p, s.rounds⟩ rev (.strikes r) h.isSome)
    | none => .serving ⟨⟨p, rev, o.failing rev, notesOf h⟩, s.rounds⟩
  | .posting p h, .unsure why => .doubting ⟨p, s.rounds⟩ why h
  | .posting p h, _ => .posting p h
  | .serving w, .submit rev => match Commit.refusal o (.submit rev) with
    | some r => strike lim s r (.serving w) (w.stop (.strikes r))
    | none => .asking w (.review s.rounds rev)
  | .serving w, .contest t why =>
    if s.contests + 1 ≤ lim.contestRounds then .asking w (.contest s.rounds t why)
    else w.stop .contestRounds
  | .serving w, .block _ => w.stop .blocked
  | .serving w, _ => .serving w
  | .asking w (.review n sub), .sendBack note => match Commit.refusal o (.sendBack note) with
    | some r => strike lim s r (.asking w (.review n sub)) (w.stop (.strikes r))
    | none =>
      if s.rounds + 1 ≤ lim.reviewRounds then .serving (w.sentBack note (o.failing note))
      else w.stop .reviewRounds
  | .asking w (.review _ sub), .recommend t => .deciding w t sub sub
  | .asking w (.contest n t why), .reply rev => match Commit.refusal o (.reply rev) with
    | some r => strike lim s r (.asking w (.contest n t why)) (w.stop (.strikes r))
    | none => .serving (w.replied rev)
  | .asking w k, .unsure why => .doubting w.assignment why (some (w, k.held))
  | .asking w k, _ => .asking w k
  | .deciding w _ _ sub, .trim rev => .deciding w [] rev sub
  | .deciding w _ _ sub, .accept tip => land lim o s m w sub tip
  | .deciding w .., .drop why => .dropped w.job.plan .retired why (some .retired)
  | .deciding w t tip sub, _ => .deciding w t tip sub
  | .doubting _ _ h, .continue p => .posting p h
  | .doubting a _ h, .drop why => .dropped a.plan .retired why (h.map fun _ => .retired)
  | .doubting a why h, _ => .doubting a why h
  | .stopping a _ c wk, .drop why => .dropped a.plan c why (if wk then some c else none)
  | .stopping a pr c wk, _ => .stopping a pr c wk
  | .resolving w _ sub _ _, .resolve rev => land lim o s m w sub rev
  | .resolving w .., .drop why => .dropped w.job.plan .retired why (some .retired)
  | .resolving w paths sub tip onto, _ => .resolving w paths sub tip onto
  | .landed p, _ => .landed p
  | .dropped p c why wc, _ => .dropped p c why wc

/-- The architect's answer a commit makes, if any. -/
def answer? (s : Spent) : Phase → Commit → Option Planner.Answer
  | .designing what, .propose rev => match o.archOk rev with
    | .ok _ => some (.designed what rev)
    | .error r => if s.refusals + 1 < lim.proposals then none else some (.refused what r)
  | .designing what, .decline why => some (.declined what why)
  | .designing _, _ => none
  | .toPlan, _ | .abandoned _, _ | .posting .., _ | .serving _, _ | .asking .., _
  | .deciding .., _ | .doubting .., _ | .stopping .., _ | .resolving .., _ | .landed _, _
  | .dropped .., _ => none

/-- Where `main` moves with a commit at `main` `m`: to a design the hub
takes, or to a landing's merge. -/
def Commit.main? (m : Revision) : Commit → Option Revision
  | .propose rev => match o.archOk rev with
    | .ok _ => some rev
    | .error _ => none
  | .accept rev | .resolve rev => match o.merge m rev with
    | .onto m' => if o.failing m' = [] then some m' else none
    | .conflict _ => none
  | .ask _ | .plan _ | .abandon _ | .trim _ | .continue _ | .drop _ | .decline _ | .post _
  | .unsure _ | .sendBack _ | .recommend _ | .reply _ | .submit _ | .contest .. | .block _ => none

def Status.next (st : Status) (p : Push) : Status where
  phase := st.phase.next lim o st.spent st.main p.commit
  answers := match answer? lim o st.spent st.phase p.commit with
    | some a => st.answers ++ [a]
    | none => st.answers
  main := (Commit.main? o st.main p.commit).getD st.main
  spent := st.spent.add o st.main p.commit

/-- The status of a log. -/
def status : Log → Status
  | [] => { main := o.base }
  | p :: l => (status l).next lim o p

/-- Who may push what, where the goal stands, at `main` `m`. -/
inductive Allowed : Phase → Spent → Revision → Push → Type
  | ask {s m} (what : String) (h : s.needs + 1 ≤ lim.needs) :
      Allowed .toPlan s m ⟨.planner, .ask what⟩
  | plan {s m} (p : Plan) : Allowed .toPlan s m ⟨.planner, .plan p⟩
  | abandon {s m} (why : String) : Allowed .toPlan s m ⟨.planner, .abandon why⟩
  /-- A design is a commit on `main`. -/
  | propose {what s m} (rev : Revision) (h : o.parents rev = [m]) :
      Allowed (.designing what) s m ⟨.architect, .propose rev⟩
  | decline {what s m} (why : String) : Allowed (.designing what) s m ⟨.architect, .decline why⟩
  | post {p h s m} (rev : Revision) : Allowed (.posting p h) s m ⟨.foreman, .post rev⟩
  | postUnsure {p h s m} (why : String) : Allowed (.posting p h) s m ⟨.foreman, .unsure why⟩
  | submit {w s m} (rev : Revision) : Allowed (.serving w) s m ⟨.worker, .submit rev⟩
  | contest {w s m} (t why : String) : Allowed (.serving w) s m ⟨.worker, .contest t why⟩
  | block {w s m} (why : String) : Allowed (.serving w) s m ⟨.worker, .block why⟩
  | sendBack {w n sub s m} (note : Revision) :
      Allowed (.asking w (.review n sub)) s m ⟨.foreman, .sendBack note⟩
  | recommend {w n sub s m} (t : List String) :
      Allowed (.asking w (.review n sub)) s m ⟨.foreman, .recommend t⟩
  | reply {w n t why s m} (rev : Revision) :
      Allowed (.asking w (.contest n t why)) s m ⟨.foreman, .reply rev⟩
  | askUnsure {w k s m} (why : String) : Allowed (.asking w k) s m ⟨.foreman, .unsure why⟩
  /-- The planner's trim sits on the tip, stays in its paths and passes
  the run set (spec §6.5). -/
  | trim {w t tip sub s m} (rev : Revision) (ht : t ≠ []) (hp : o.failing rev = [])
      (hparent : o.parents rev = [tip]) (ho : o.owns .planner rev = none) :
      Allowed (.deciding w t tip sub) s m ⟨.planner, .trim rev⟩
  /-- The planner lands the tip the hub holds. -/
  | accept {w t tip sub s m} : Allowed (.deciding w t tip sub) s m ⟨.planner, .accept tip⟩
  /-- The same task goes on, with a revised plan. -/
  | continue {a why h s m} (p : Plan) (hs : p.slug = a.plan.slug)
      (hl : s.continues + 1 ≤ lim.continues) :
      Allowed (.doubting a why h) s m ⟨.planner, .continue p⟩
  /-- The resolution merges exactly the recorded `main` into the tip, and
  stays in its paths (spec §11). -/
  | resolve {w paths sub tip onto s m} (rev : Revision) (h : o.parents rev = [tip, onto])
      (ho : o.owns .planner rev = none) :
      Allowed (.resolving w paths sub tip onto) s m ⟨.planner, .resolve rev⟩
  | drop {ph s m} (why : String) (h : ph.awaitsPlanner = true) :
      Allowed ph s m ⟨.planner, .drop why⟩

/-- A push is legal when its author may make it at the log before it. -/
abbrev Legal (l : Log) (p : Push) : Type :=
  Allowed lim o (status lim o l).phase (status lim o l).spent (status lim o l).main p

/-- The legal logs of one goal. -/
abbrev Goal := Hub (Legal lim o)

/-- Every legal push comes from its commit's author. -/
theorem Allowed.author (h : Allowed lim o ph s m p) : p.role = p.commit.author := by
  cases h <;> rfl

-- Progress -------------------------------------------------------------------

/-- Every phase short of an end admits a push. -/
def Phase.move {s m} : (ph : Phase) → ph.final = false → (p : Push) × Allowed lim o ph s m p
  | .toPlan, _ => ⟨_, .abandon ""⟩
  | .designing _, _ => ⟨_, .decline ""⟩
  | .posting .., _ => ⟨_, .postUnsure ""⟩
  | .serving _, _ => ⟨_, .block ""⟩
  | .asking .., _ => ⟨_, .askUnsure ""⟩
  | .deciding .., _ => ⟨_, .accept⟩
  | .doubting .., _ => ⟨_, .drop "" rfl⟩
  | .stopping .., _ => ⟨_, .drop "" rfl⟩
  | .resolving .., _ => ⟨_, .drop "" rfl⟩
  | .abandoned _, h | .landed _, h | .dropped .., h => nomatch h

/-- A legal log that admits no push is at an end. With `Hub.bounded`,
every goal reaches an end. -/
theorem Hub.ends {l : Log} (h : ∀ p, Legal lim o l p → False) :
    (status lim o l).phase.final = true := by
  cases hf : (status lim o l).phase.final
  · exact (h _ (Phase.move lim o _ hf).2).elim
  · rfl

-- What every legal log keeps -------------------------------------------------

/-- The phases with no worker. -/
def Phase.workerless : Phase → Bool
  | .toPlan | .designing _ | .abandoned _ => true
  | .posting _ h | .doubting _ _ h => h.isNone
  | .stopping _ _ _ wk => !wk
  | .dropped _ _ _ wc => wc.isNone
  | .serving _ | .asking .. | .deciding .. | .resolving .. | .landed _ => false

/-- No worker, no round. -/
def Ok (ph : Phase) (s : Spent) : Prop := ph.workerless = true → s.rounds = 0

theorem allowed_ok (h : Allowed lim o ph s m p) (ok : Ok ph s) :
    Ok (ph.next lim o s m p.commit) (s.add o m p.commit) := by
  intro hw
  cases h with
  | drop why hd =>
    cases ph with
    | doubting a w held =>
      cases held <;> simp_all [Phase.next, Spent.add, Spent.spend, Commit.refusal,
        Phase.workerless, Ok]
    | stopping a pr c wk =>
      cases wk <;> simp_all [Phase.next, Spent.add, Spent.spend, Commit.refusal,
        Phase.workerless, Ok]
    | _ => simp_all [Phase.next, Phase.workerless, Phase.awaitsPlanner, Ok]
  | _ =>
    simp only [Phase.next, Spent.add, Spent.spend, land, Spent.land, strike, Work.stop]
      at hw ⊢ <;>
      (repeat' split at hw) <;> (repeat' split) <;> simp_all [Phase.workerless, Ok]

theorem Hub.ok : {l : Log} → Goal lim o l → Ok (status lim o l).phase (status lim o l).spent
  | _, .empty => fun _ => rfl
  | _, .commit h lg => allowed_ok lim o lg (Hub.ok h)

end HubLog
