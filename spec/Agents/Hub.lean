import Agents.Planner
import Agents.Architect
import Agents.Foreman
import Agents.Worker

/-!
The hub of one goal (decision 12). Every event of the loop is a commit to
the hub, made by one role; the log holds them newest first. The player is
the hub's sole writer: what it does, the checks and the merges, is the
effect a legal commit has on the hub, read through the oracles (`Oracles`).

The hub's status and its counters are functions of the log: `status l`
folds each commit's effect into the status, and `spent l` counts the
commits that spend (spec §10). A commit is legal when its author may make
it at the status of the log before it (`Allowed`); its effect then reads
the oracles: the post's tests must fail, a submission must pass the run
set, a landing merges through `merge`, and a proposal passes `archOk` or
is refused. Past a limit, the effect is the stop. `Hub Legal l` is a legal
log: each commit is legal at the log before it. Each role's state is a view
of it (`Agents/Views.lean`).
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

/-- The log, newest first. -/
abbrev Log := List Commit

/-- What the hub reads a commit through. `failing rev`: the checks of the
run set that fail at `rev`. `merge rev`: merging `rev` onto `main`.
`archOk rev`: `arch check` and the ownership rules at `rev`. -/
structure Oracles where
  failing : Revision → List String
  merge : Revision → Merge
  archOk : Revision → Except Reason Unit

/-- The hub's limits. Open: `continues`, `conflicts`, `proposals` and
`needs` have no roadmap key yet. -/
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
  /-- Questions to the architect per goal. -/
  needs : Nat := 2

/-- A legal log: each commit is legal at the log before it. -/
inductive Hub (Legal : Log → Commit → Type) : Log → Type
  | empty : Hub Legal []
  | commit {l c} : Hub Legal l → Legal l c → Hub Legal (c :: l)

def Hub.length {Legal : Log → Commit → Type} : {l : Log} → Hub Legal l → Nat
  | _, .empty => 0
  | _, .commit h _ => h.length + 1

/-- A role's view along a legal log: a view `v` of each log, and the role's
part `R` in each commit, give a run of parts from the empty log's view to
the log's. This is the projection; that `part` type-checks is the check. -/
def Hub.project {Legal : Log → Commit → Type} {V : Type} (v : Log → V) (R : V → V → Type)
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
  /-- The planner resolves a conflict on `paths`. -/
  | resolving (w : Work) (paths : List String) (sub : Revision)
  /-- On `main`. An end. -/
  | landed (p : Plan)
  /-- Dropped for `cause`; `worker` holds the worker's end cause. An end. -/
  | dropped (p : Plan) (cause : Cause) (why : String) (worker : Option Cause)

def Phase.awaitsPlanner : Phase → Bool
  | .deciding .. | .doubting .. | .stopping .. | .resolving .. => true
  | _ => false

/-- The hub's status: where the goal stands, and the architect's answers,
oldest first. -/
structure Status where
  phase : Phase := .toPlan
  answers : List Planner.Answer := []

/-- What a goal has spent: each a count of commits in the log. -/
structure Spent where
  /-- Questions asked. -/
  needs : Nat := 0
  /-- Proposals refused since the last question. -/
  refusals : Nat := 0
  /-- Send-backs whose test fails: the round. -/
  rounds : Nat := 0
  contests : Nat := 0
  /-- Posts whose tests pass. -/
  posts : Nat := 0
  /-- Send-backs whose test passes. -/
  reviews : Nat := 0
  /-- Submissions and landings that fail the run set. -/
  suite : Nat := 0
  continues : Nat := 0
  conflicts : Nat := 0

variable (lim : Limits) (o : Oracles)

/-- What landing `rev` spends. -/
def Spent.land (s : Spent) (rev : Revision) : Spent :=
  match o.merge rev with
  | .onto m => if o.failing m = [] then s else { s with suite := s.suite + 1 }
  | .conflict _ => { s with conflicts := s.conflicts + 1 }

/-- What a commit spends. -/
def Spent.add (s : Spent) : Commit → Spent
  | .ask _ => { s with needs := s.needs + 1, refusals := 0 }
  | .propose rev => match o.archOk rev with
    | .ok _ => s
    | .error _ => { s with refusals := s.refusals + 1 }
  | .post rev => if o.failing rev = [] then { s with posts := s.posts + 1 } else s
  | .submit rev => if o.failing rev = [] then s else { s with suite := s.suite + 1 }
  | .contest .. => { s with contests := s.contests + 1 }
  | .sendBack note =>
    if o.failing note = [] then { s with reviews := s.reviews + 1 }
    else { s with rounds := s.rounds + 1 }
  | .continue _ => { s with continues := s.continues + 1 }
  | .accept rev | .resolve rev => s.land o rev
  | _ => s

/-- Landing `rev` for work `w`: on `main`, a broken run set, or a
conflict. -/
def land (s : Spent) (w : Work) (sub rev : Revision) : Phase :=
  match o.merge rev with
  | .onto m =>
    if o.failing m = [] then .landed w.job.plan
    else if s.suite + 1 < lim.suiteStrikes then .serving w
    else .stopping w.assignment w.job.prompt (.strikes .suite) true
  | .conflict paths =>
    if s.conflicts + 1 ≤ lim.conflicts then .resolving w paths sub
    else .stopping w.assignment w.job.prompt .conflicts true

/-- A commit's effect on the phase. -/
def Phase.next (s : Spent) : Phase → Commit → Phase
  | .toPlan, .ask what => .designing what
  | .toPlan, .plan p => .posting p none
  | .toPlan, .abandon why => .abandoned why
  | .designing what, .propose rev => match o.archOk rev with
    | .ok _ => .toPlan
    | .error _ => if s.refusals + 1 < lim.proposals then .designing what else .toPlan
  | .designing _, .decline _ => .toPlan
  | .posting p h, .post rev =>
    if o.failing rev = [] then
      if s.posts + 1 < lim.postStrikes then .posting p h
      else .stopping ⟨p, s.rounds⟩ rev (.strikes .post) h.isSome
    else .serving ⟨⟨p, rev, o.failing rev, notesOf h⟩, s.rounds⟩
  | .posting p h, .unsure why => .doubting ⟨p, s.rounds⟩ why h
  | .serving w, .submit rev =>
    if o.failing rev = [] then .asking w (.review s.rounds rev)
    else if s.suite + 1 < lim.suiteStrikes then .serving w
    else .stopping w.assignment w.job.prompt (.strikes .suite) true
  | .serving w, .contest t why =>
    if s.contests + 1 ≤ lim.contestRounds then .asking w (.contest s.rounds t why)
    else .stopping w.assignment w.job.prompt .contestRounds true
  | .serving w, .block _ => .stopping w.assignment w.job.prompt .blocked true
  | .asking w (.review n sub), .sendBack note =>
    if o.failing note = [] then
      if s.reviews + 1 < lim.reviewStrikes then .asking w (.review n sub)
      else .stopping w.assignment w.job.prompt (.strikes .review) true
    else if s.rounds + 1 ≤ lim.reviewRounds then .serving (w.sentBack note (o.failing note))
    else .stopping w.assignment w.job.prompt .reviewRounds true
  | .asking w (.review _ sub), .recommend t => .deciding w t sub sub
  | .asking w (.contest ..), .reply rev => .serving (w.replied rev)
  | .asking w k, .unsure why => .doubting w.assignment why (some (w, k.held))
  | .deciding w _ _ sub, .trim rev => .deciding w [] rev sub
  | .deciding w _ _ sub, .accept rev => land lim o s w sub rev
  | .doubting _ _ h, .continue p => .posting p h
  | .resolving w _ sub, .resolve rev => land lim o s w sub rev
  | .deciding w .., .drop why | .resolving w .., .drop why =>
    .dropped w.job.plan .retired why (some .retired)
  | .doubting a _ h, .drop why => .dropped a.plan .retired why (h.map fun _ => .retired)
  | .stopping a _ c wk, .drop why => .dropped a.plan c why (if wk then some c else none)
  | ph, _ => ph

/-- The architect's answer a commit makes, if any. -/
def answer? (s : Spent) : Phase → Commit → Option Planner.Answer
  | .designing what, .propose rev => match o.archOk rev with
    | .ok _ => some (.designed what rev)
    | .error r => if s.refusals + 1 < lim.proposals then none else some (.refused what r)
  | .designing what, .decline why => some (.declined what why)
  | _, _ => none

def Status.next (st : Status) (s : Spent) (c : Commit) : Status where
  phase := st.phase.next lim o s c
  answers := match answer? lim o s st.phase c with
    | some a => st.answers ++ [a]
    | none => st.answers

/-- The counters of a log. -/
def spent : Log → Spent
  | [] => {}
  | c :: l => (spent l).add o c

/-- The status of a log. -/
def status : Log → Status
  | [] => {}
  | c :: l => (status l).next lim o (spent o l) c

/-- Who may commit what, where the goal stands. -/
inductive Allowed : Phase → Spent → Commit → Type
  | ask {s} (what : String) (h : s.needs + 1 ≤ lim.needs) : Allowed .toPlan s (.ask what)
  | plan {s} (p : Plan) : Allowed .toPlan s (.plan p)
  | abandon {s} (why : String) : Allowed .toPlan s (.abandon why)
  | propose {what s} (rev : Revision) : Allowed (.designing what) s (.propose rev)
  | decline {what s} (why : String) : Allowed (.designing what) s (.decline why)
  | post {p h s} (rev : Revision) : Allowed (.posting p h) s (.post rev)
  | postUnsure {p h s} (why : String) : Allowed (.posting p h) s (.unsure why)
  | submit {w s} (rev : Revision) : Allowed (.serving w) s (.submit rev)
  | contest {w s} (t why : String) : Allowed (.serving w) s (.contest t why)
  | block {w s} (why : String) : Allowed (.serving w) s (.block why)
  | sendBack {w n sub s} (note : Revision) : Allowed (.asking w (.review n sub)) s (.sendBack note)
  | recommend {w n sub s} (t : List String) : Allowed (.asking w (.review n sub)) s (.recommend t)
  | reply {w n t why s} (rev : Revision) : Allowed (.asking w (.contest n t why)) s (.reply rev)
  | askUnsure {w k s} (why : String) : Allowed (.asking w k) s (.unsure why)
  /-- The planner's trim passes the run set (spec §6.5). -/
  | trim {w t tip sub s} (rev : Revision) (ht : t ≠ []) (hp : o.failing rev = []) :
      Allowed (.deciding w t tip sub) s (.trim rev)
  /-- The planner lands the tip the hub holds. -/
  | accept {w t tip sub s} : Allowed (.deciding w t tip sub) s (.accept tip)
  | continue {a why h s} (p : Plan) (hl : s.continues + 1 ≤ lim.continues) :
      Allowed (.doubting a why h) s (.continue p)
  | resolve {w paths sub s} (rev : Revision) : Allowed (.resolving w paths sub) s (.resolve rev)
  | drop {ph s} (why : String) (h : ph.awaitsPlanner = true) : Allowed ph s (.drop why)

/-- A commit is legal when its author may make it at the log before it. -/
abbrev Legal (l : Log) (c : Commit) : Type := Allowed lim o (status lim o l).phase (spent o l) c

/-- The legal logs of one goal. -/
abbrev Goal := Hub (Legal lim o)

-- What every legal log keeps -------------------------------------------------

/-- The phases with no worker. -/
def Phase.workerless : Phase → Bool
  | .toPlan | .designing _ | .abandoned _ => true
  | .posting _ h | .doubting _ _ h => h.isNone
  | .stopping _ _ _ wk => !wk
  | .dropped _ _ _ wc => wc.isNone
  | _ => false

/-- No worker, no round. -/
def Ok (ph : Phase) (s : Spent) : Prop := ph.workerless = true → s.rounds = 0

theorem allowed_ok (h : Allowed lim o ph s c) (ok : Ok ph s) :
    Ok (ph.next lim o s c) (s.add o c) := by
  intro hw
  cases h with
  | drop why hd =>
    cases ph with
    | doubting a w held => cases held <;> simp_all [Phase.next, Spent.add, Phase.workerless, Ok]
    | stopping a pr c wk => cases wk <;> simp_all [Phase.next, Spent.add, Phase.workerless, Ok]
    | _ => simp_all [Phase.next, Phase.workerless, Phase.awaitsPlanner, Ok]
  | _ =>
    simp only [Phase.next, Spent.add, land, Spent.land] at hw ⊢ <;>
      (repeat' split at hw) <;> (repeat' split) <;> simp_all [Phase.workerless, Ok]

theorem Hub.ok : {l : Log} → Goal lim o l → Ok (status lim o l).phase (spent o l)
  | _, .empty => fun _ => rfl
  | _, .commit h lg => allowed_ok lim o lg (Hub.ok h)

end HubLog
