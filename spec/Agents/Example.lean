import Agents.Views
import Agents.Bounded

/-!
Four goals, as logs. Each is a legal log by its `Goal` value; every role's
run along it comes from `Goal.worker`, `Goal.foreman`, `Goal.planner` and
`Goal.architect`, with nothing written by hand. The checks below read
where each role ends, and where `main` stands.
-/
namespace HubLog.Example
open Kernel

/-- The run set fails at the post and at the merge `m1`. -/
def failing (rev : Revision) : List String :=
  if rev = "post" ∨ rev = "m1" then ["t"] else []

/-- `sub1` merges as `m1`; any other revision as `m2`. -/
def merge (_main rev : Revision) : Merge :=
  if rev = "sub1" then .onto "m1" else .onto "m2"

/-- The architecture refuses `d1` for ownership. -/
def archOk (rev : Revision) : Except Reason Unit :=
  if rev = "d1" then .error .ownership else .ok ()

/-- The designs sit on `main0`. -/
def parents (rev : Revision) : List Revision :=
  if rev = "d1" ∨ rev = "d2" then ["main0"] else []

/-- `out` leaves the worker's scope. -/
def owns (_ : Role) (rev : Revision) : Option Reason :=
  if rev = "out" then some .scope else none

def o : Oracles := ⟨"main0", parents, failing, merge, archOk, owns⟩
def lim : Limits := {}
def p : Plan := { slug := ⟨"a"⟩ }
def g : Planner.Goal := .fromOperator "ship a"

/-- Each commit pushed by its author. -/
def honest (cs : List Commit) : Log := cs.map fun c => ⟨c.author, c⟩

theorem post_fails : failing "post" ≠ [] := by decide

-- An accepted task, start to land ---------------------------------------------

/-- Plan, post, submit, recommend, accept: it lands. -/
def loopLog : Log := honest [.accept "sub", .recommend [], .submit "sub", .post "post", .plan p]

def loop : Goal lim o loopLog :=
  .commit (.commit (.commit (.commit (.commit .empty (.plan p)) (.post "post"))
    (.submit "sub")) (.recommend [])) .accept

example : workerView lim o loopLog = some .accepted := rfl
example : foremanView lim o loopLog = some .accepted := rfl
example : plannerView lim o g loopLog = .landed p := rfl
/-- The landing's merge is `main`. -/
example : (status lim o loopLog).main = "m2" := rfl

-- A merge that breaks the run set --------------------------------------------

/-- The first landing, `m1`, breaks the run set: a suite strike, and the
worker builds again; its next submission lands as `m2`. -/
def brokenLog : Log :=
  honest [.accept "sub", .recommend [], .submit "sub", .accept "sub1", .recommend [],
    .submit "sub1", .post "post", .plan p]

def brokenLoop : Goal lim o brokenLog :=
  .commit (.commit (.commit (.commit (.commit (.commit (.commit (.commit .empty
    (.plan p)) (.post "post")) (.submit "sub1")) (.recommend [])) .accept)
    (.submit "sub")) (.recommend [])) .accept

/-- After `m1`, the worker builds again, in the same round, and `main`
stays. -/
example : workerView lim o (brokenLog.drop 3) =
    some (.building ⟨p, "post", ["t"], []⟩ 0) := rfl
example : (status lim o (brokenLog.drop 3)).main = "main0" := rfl
example : (status lim o brokenLog).spent.strikes.suite = 1 := rfl
example : workerView lim o brokenLog = some .accepted := rfl
example : plannerView lim o g brokenLog = .landed p := rfl

-- A submission outside the scope -----------------------------------------------

/-- The worker's `out` leaves its scope: a scope strike, and it builds
again. -/
def scopeLog : Log := honest [.submit "out", .post "post", .plan p]

def scope : Goal lim o scopeLog :=
  .commit (.commit (.commit .empty (.plan p)) (.post "post")) (.submit "out")

example : workerView lim o scopeLog = some (.building ⟨p, "post", ["t"], []⟩ 0) := rfl
example : (status lim o scopeLog).spent.strikes.scope = 1 := rfl

-- A design question ------------------------------------------------------------

/-- The planner asks for `api`; the hub refuses `d1` for ownership and
takes `d2`, which becomes `main`. -/
def designLog : Log := honest [.propose "d2", .propose "d1", .ask "api"]

def design : Goal lim o designLog :=
  .commit (.commit (.commit .empty (.ask "api" (by decide))) (.propose "d1" rfl))
    (.propose "d2" rfl)

/-- After the refusal, the architect designs again. -/
example : architectView lim o 0 (designLog.drop 1) = some (.asked (.need "api") 1) := rfl
/-- The architect's revision is on `main`, and the planner holds it. -/
example : architectView lim o 0 designLog = some (.designed (.need "api") "d2") := rfl
example : (status lim o designLog).main = "d2" := rfl
example : plannerView lim o g designLog = .toPlan g [.designed "api" "d2"] := rfl

/-- Every role's run along the design, from the log alone. -/
def designRuns :=
  (Goal.planner lim o g design, Goal.architect lim o 0 design, Goal.worker lim o design)

-- Rejected -------------------------------------------------------------------

/-
Each of these fails to build; the error follows each.

(i) A refused design heard as designed. The planner's view is a function of
the log, and `d1` is refused, so the planner still designs.

    example : plannerView lim o g (honest [.propose "d1", .ask "api"]) =
        .toPlan g [.designed "api" "d1"] := rfl

    error: Type mismatch
      rfl
    has type
      ?m.10 = ?m.10
    but is expected to have type
      plannerView lim o g (honest [Commit.propose "d1", Commit.ask "api"]) =
        Planner.State.toPlan g [Planner.Answer.designed "api" "d1"]

(i') The planner plans before the hub has judged a design.

    example : Goal lim o (honest [.plan p, .ask "api"]) :=
      .commit (.commit .empty (.ask "api" (by decide))) (.plan p)

    error: Application type mismatch: The argument
      Allowed.plan p
    has type
      Allowed ?m.16 ?m.17 Phase.toPlan ?m.18 ?m.19 { role := Role.planner, commit := Commit.plan p }
    but is expected to have type
      Legal lim o (List.map (fun c => { role := c.author, commit := c }) [Commit.ask "api"])
        ((fun c => { role := c.author, commit := c }) (Commit.plan p))

(ii) The worker accepted while the merge breaks.

    example : workerView lim o (brokenLog.drop 3) = some .accepted := rfl

    error: Type mismatch
      rfl
    has type
      ?m.8 = ?m.8
    but is expected to have type
      workerView lim o (List.drop 3 brokenLog) = some Worker.State.accepted

(iii) Worker: a push that answers the worker, passed off as a wait.

    example : Peer Worker.Step Worker.State.isStart
        (workerView lim o (loopLog.drop 1)) (workerView lim o loopLog) := .inr ⟨rfl⟩

    error: Application type mismatch: The argument
      rfl
    has type
      ?m.9 = ?m.9
    but is expected to have type
      workerView lim o loopLog = workerView lim o (List.drop 1 loopLog)

(iii) Foreman: it accepts while the submission waits for its review.

    example : Peer Foreman.Step Foreman.State.isStart
        (foremanView lim o (loopLog.drop 3)) (foremanView lim o (loopLog.drop 2)) :=
      .inl (Steps.one .accept)

    error: Application type mismatch: The argument
      Foreman.Step.accept
    has type
      Foreman.Step (Foreman.State.posted ?m.14 ?m.15) Foreman.State.accepted
    but is expected to have type
      Foreman.Step (Foreman.State.posted …) (Foreman.State.owing … (Foreman.Ask.review … "sub"))

(iii) Planner: it lands while the merge breaks the run set.

    example : Present Planner.Step
        (plannerView lim o g (brokenLog.drop 4)) (plannerView lim o g (brokenLog.drop 3)) :=
      .inl (Steps.two (.decide .accept) .land)

    error: Application type mismatch: The argument
      Planner.Step.land
    has type
      Planner.Step (Planner.State.landing ?m.19 ?m.20) (Planner.State.landed ?m.20)
    but is expected to have type
      Planner.Step (Planner.after g … (Planner.Advice.recommend []) Planner.Decision.accept)
        (plannerView lim o g (List.drop 3 brokenLog))

(iii) Architect: it counts a refused proposal as designed.

    example : Peer (Architect.Step lim.proposals) Architect.State.isStart
        (architectView lim o 0 (honest [.ask "api"]))
        (architectView lim o 0 (honest [.propose "d1", .ask "api"])) :=
      .inl (Steps.two (.propose "d1") .accepted)

    error: Application type mismatch: The argument
      Architect.Step.accepted
    has type
      Architect.Step ?m.21 (Architect.State.proposing ?m.22 ?m.23 ?m.24) (Architect.State.designed ?m.22 ?m.24)
    but is expected to have type
      Architect.Step lim.proposals
        (Architect.State.proposing (Architect.Question.need "api") … "d1")
        (Architect.State.asked (Architect.Question.need "api") …)

(iv) The worker pushes the accept: the planner's alone.

    example : Goal lim o (⟨.worker, .accept "sub"⟩ :: loopLog.drop 1) :=
      .commit (.commit (.commit (.commit (.commit .empty (.plan p)) (.post "post"))
        (.submit "sub")) (.recommend [])) .accept

    error: Application type mismatch: The argument
      Allowed.accept
    has type
      Allowed ?m.44 ?m.45 (Phase.deciding ?m.46 ?m.47 ?m.48 ?m.49) ?m.50 ?m.51
        { role := Role.planner, commit := Commit.accept ?m.48 }
    but is expected to have type
      Legal lim o (List.drop 1 loopLog) { role := Role.worker, commit := Commit.accept "sub" }

(v) A design off `main`: `d3` sits on no commit.

    example : Goal lim o (honest [.propose "d3", .ask "api"]) :=
      .commit (.commit .empty (.ask "api" (by decide))) (.propose "d3" rfl)

    error: Application type mismatch: The argument
      rfl
    has type
      ?m.22 = ?m.22
    but is expected to have type
      o.parents "d3" = [(status lim o (List.map (fun c => …) [Commit.ask "api"])).main]

(vi) A doubt continued as another task.

    example : Goal lim o (honest [.continue { slug := ⟨"b"⟩ }, .unsure "?", .plan p]) :=
      .commit (.commit (.commit .empty (.plan p)) (.postUnsure "?")) (.continue _ rfl (by decide))

    error: Application type mismatch: The argument
      rfl
    has type
      ?m.37 = ?m.37
    but is expected to have type
      { slug := { name := "b" } }.slug = { plan := p, round := … }.plan.slug

(vii) A trim off the tip: `x` sits on no commit, the tip is `sub`.

    example : Goal lim o
        (honest [.trim "x", .recommend ["t"], .submit "sub", .post "post", .plan p]) :=
      .commit (.commit (.commit (.commit (.commit .empty (.plan p)) (.post "post"))
        (.submit "sub")) (.recommend ["t"])) (.trim "x" (by decide) rfl rfl rfl)

    error: Application type mismatch: The argument
      rfl
    has type
      ?m.61 = ?m.61
    but is expected to have type
      o.parents "x" = ["sub"]
-/

end HubLog.Example
