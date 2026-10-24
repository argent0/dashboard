import Architect.Rules
import Planner.Types

/-!
Words every role shares. A task is named by its `Slug`; a commit by its
`Revision`. A check is one entry of the suite: a named command run in a
view of the components it covers.

Every role states its work as a machine: a type of states and an indexed
type `Step : State → State → Type` whose values are its legal transitions.
Each machine starts with something to do and ends in states with no step
out. A `Run` is a path of steps.

Roles meet in the hub (`Agents/Hub.lean`): every event is a commit by
one role, and each role's state is a view of the hub's log, so every legal
log is a run of every role's machine (`Agents/Views.lean`). Shared words
stay few: a role's view of a shared thing is its own type, in its own
words.
-/
namespace Kernel
export Dashboard (Slug Component Check Manifest)

/-- A commit's id in the hub. -/
abbrev Revision := String

/-- A task as planned: the slug, the components it changes, the planner's
brief to its foreman, and the goal of its phase. -/
structure Plan where
  slug : Slug
  components : List Component := []
  brief : String := ""
  goal : String := ""

/-- What a refused commit did wrong. -/
inductive Reason
  /-- It writes outside its author's paths, or breaks the architecture. -/
  | ownership
  /-- A worker commit that changes a pinned file (spec §8.3). -/
  | pins
  /-- A worker commit outside the task's scope (spec §8.3). -/
  | scope
  /-- A post whose task check already passes. -/
  | post
  /-- A send-back whose new test already passes on the worker's code. -/
  | review
  /-- A submission or a merge that fails the run set. -/
  | suite
  deriving Repr, DecidableEq, Inhabited

/-- Why a task stopped. -/
inductive Cause
  | strikes (r : Reason)
  /-- A send-back would pass the review rounds allowed. -/
  | reviewRounds
  /-- A contest would pass the contest rounds allowed. -/
  | contestRounds
  /-- A merge conflict would pass the conflicts allowed. -/
  | conflicts
  | blocked
  /-- Dropped by the planner. -/
  | retired
  deriving Repr, DecidableEq, Inhabited

/-- A run of the machine `Step`: a path of steps from `s` to `u`. -/
inductive Run {S : Type} (Step : S → S → Type) : S → S → Type
  | stay {s} : Run Step s s
  | next {s t u} : Step s t → Run Step t u → Run Step s u

/-- One step, as a run. -/
def Run.one {S : Type} {Step : S → S → Type} {s t : S} (st : Step s t) : Run Step s t :=
  .next st .stay

/-- Two steps, as a run. -/
def Run.two {S : Type} {Step : S → S → Type} {s t u : S} (a : Step s t) (b : Step t u) :
    Run Step s u :=
  .next a (.next b .stay)

/-- A run, one step longer at its end. -/
def Run.snoc {S : Type} {Step : S → S → Type} : {s t u : S} → Run Step s t → Step t u →
    Run Step s u
  | _, _, _, .stay, st => .next st .stay
  | _, _, _, .next a r, st => .next a (r.snoc st)

/-- The steps of a run. -/
def Run.length {S : Type} {Step : S → S → Type} : {s u : S} → Run Step s u → Nat
  | _, _, .stay => 0
  | _, _, .next _ r => r.length + 1

/-- A property every step keeps holds along every run. -/
theorem Run.keeps {S : Type} {Step : S → S → Type} (P : S → Prop)
    (h : ∀ {s t}, Step s t → P s → P t) : ∀ {s u}, Run Step s u → P s → P u
  | _, _, .stay, p => p
  | _, _, .next st r, p => Run.keeps P h r (h st p)

/-- When every step lowers `fuel`, a run takes at most `fuel` of its start
steps. -/
theorem Run.bounded {S : Type} {Step : S → S → Type} (fuel : S → Nat)
    (h : ∀ {s t}, Step s t → fuel t < fuel s) : ∀ {s u}, (r : Run Step s u) → r.length ≤ fuel s
  | _, _, .stay => Nat.zero_le _
  | _, _, .next st r => by
    have := Run.bounded fuel h r
    have := h st
    simp only [Run.length]; omega

/-- A role of the loop: who makes a commit. -/
inductive Role
  | architect | planner | foreman | worker
  deriving Repr, DecidableEq, BEq, Inhabited

/-- One or more steps of `Step` from `s` to `u`: a role that moves. -/
inductive Steps {S : Type} (Step : S → S → Type) : S → S → Type
  | mk {s t u} : Step s t → Run Step t u → Steps Step s u

def Steps.one {S : Type} {Step : S → S → Type} {s t : S} (st : Step s t) : Steps Step s t :=
  .mk st .stay

def Steps.two {S : Type} {Step : S → S → Type} {s t u : S} (a : Step s t) (b : Step t u) :
    Steps Step s u :=
  .mk a (.one b)

/-- A role's part in one commit, between two moments: it starts in a start
state, or it moves. Absent before and after, it takes no part: `Empty`. -/
def Side {S : Type} (Step : S → S → Type) (start : S → Bool) : Option S → Option S → Type
  | none, none => Empty
  | none, some s => PLift (start s = true)
  | some s, some t => Steps Step s t
  | some _, none => Empty

/-- A role that may be absent, at one commit: it takes part (`Side`), or it
waits as it is. -/
abbrev Peer {S : Type} (Step : S → S → Type) (start : S → Bool) (w w' : Option S) : Type :=
  Side Step start w w' ⊕ PLift (w' = w)

/-- A role that always exists, at one commit: it moves, or it waits as it
is. -/
abbrev Present {S : Type} (Step : S → S → Type) (w w' : S) : Type :=
  Steps Step w w' ⊕ PLift (w' = w)

end Kernel
