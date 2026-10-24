import Agents.Kernel

/-!
The foreman's work. One foreman per task. It starts with something to do,
an assignment to post: the prompt and the failing tests. Then it answers
the hub's asks one at a time: reviews of the worker's submissions and
contests of its tests. It ends when the task is accepted or dropped.

It writes within its scope alone (`inBounds`).
-/
namespace Foreman
open Kernel Dashboard

/-- What the foreman is handed: the plan of its task, and the round the
hub is in when it hands it. -/
structure Assignment where
  plan : Plan
  round : Nat := 0

/-- What the hub asks of the foreman after the post. -/
inductive Ask
  /-- Review submission `sub`, made in round `round`. -/
  | review (round : Nat) (sub : Revision)
  /-- The worker contests `test` in round `round`. -/
  | contest (round : Nat) (test why : String)

/-- The foreman's possible answers to each ask. -/
inductive Response : Ask → Type
  /-- The work goes back, with a note and a new failing test. -/
  | sentBack {n s} (note : Revision) : Response (.review n s)
  /-- The work meets the task; `trim` names the task checks to drop from
  the suite (spec §6.5). -/
  | recommend {n s} (trim : List String) : Response (.review n s)
  /-- The reply to the contest, committed. -/
  | replied {n t w} (rev : Revision) : Response (.contest n t w)
  | unsure {k} (why : String) : Response k

inductive State
  /-- Something to do: post for `a`. The start. -/
  | toPost (a : Assignment)
  /-- Posted prompt `prompt` for `a`; waits for the next ask. -/
  | posted (a : Assignment) (prompt : Revision)
  /-- Unsure about `a`, with why; waits for a revised assignment. -/
  | unsure (a : Assignment) (why : String)
  /-- Owes the answer to `k`. -/
  | owing (a : Assignment) (prompt : Revision) (k : Ask)
  /-- The task was accepted. An end. -/
  | accepted
  /-- The task was dropped. An end. -/
  | dropped (c : Cause)

/-- Where response `r` to ask `k` leaves the foreman. -/
def after (a : Assignment) (prompt : Revision) : (k : Ask) → Response k → State
  | _, .unsure why => .unsure a why
  | _, .sentBack _ | _, .recommend _ | _, .replied _ => .posted a prompt

/-- The states that wait on the hub alone. -/
def State.waits : State → Bool
  | .posted .. | .unsure .. => true
  | .toPost _ | .owing .. | .accepted | .dropped _ => false

/-- The foreman's legal transitions. -/
inductive Step : State → State → Type
  | post {a} (prompt : Revision) : Step (.toPost a) (.posted a prompt)
  | postUnsure {a} (why : String) : Step (.toPost a) (.unsure a why)
  /-- The post was refused, or the plan was revised: post again. -/
  | repost {a p} (a' : Assignment) : Step (.posted a p) (.toPost a')
  | revised {a w} (a' : Assignment) : Step (.unsure a w) (.toPost a')
  | asked {a p} (k : Ask) : Step (.posted a p) (.owing a p k)
  | respond {a p k} (r : Response k) : Step (.owing a p k) (after a p k r)
  | accept {a p} : Step (.posted a p) .accepted
  | drop {s} (c : Cause) (h : s.waits = true) : Step s (.dropped c)

def State.isStart : State → Bool
  | .toPost _ => true
  | .posted .. | .unsure .. | .owing .. | .accepted | .dropped _ => false

/-- While it owes an answer, the foreman's only step is to give it. -/
theorem owing_steps (st : Step (.owing a p k) t) : ∃ r : Response k, t = after a p k r := by
  cases st with
  | respond r => exact ⟨r, rfl⟩
  | drop _ h => simp [State.waits] at h

/-- Every answer keeps the assignment and its prompt, unless unsure. -/
theorem answer_keeps_prompt (r : Response k) (h : ∀ w, after a p k r ≠ .unsure a w) :
    after a p k r = .posted a p := by
  cases r with
  | unsure w => exact absurd rfl (h w)
  | _ => rfl

theorem accepted_is_final : Step .accepted t → False := by
  intro st; cases st with
  | drop _ h => simp [State.waits] at h

theorem dropped_is_final : Step (.dropped c) t → False := by
  intro st; cases st with
  | drop _ h => simp [State.waits] at h

-- The scope ------------------------------------------------------------------

/-- The task's checks: `architecture/Tests/<slug>.lean`. -/
def testsFile (s : Slug) : String := s!"architecture/Tests/{s.name}.lean"

/-- The task's plan: `tasks/<slug>/Task.lean`, the planner's. -/
def taskFile (s : Slug) : String := s!"tasks/{s.name}/Task.lean"

/-- An API's IDL file. -/
def apiFile (a : Api) : String := s!"architecture/Apis/{a.name}.lean"

/-- A module's frame file. -/
def frameFile (m : String) : String := s!"architecture/Frames/{m}.lean"

/-- A frame's generated skeleton: `<src>/frame`. -/
def Frame.genDir (f : Frame) : String := s!"{trimSlash f.srcDir}/frame"

/-- `c` names one of `cs`. -/
def names (cs : List Component) (c : Component) : Bool :=
  cs.any (·.name == c.name)

/-- What the foreman of task `s` on `cs` may write, at a commit whose
manifest is `m`, with the roadmap's `lockFiles`: the task's checks file,
the task directory, the run set's check files, the APIs with both ends in
`cs` and their files, the frames of `cs`, the fixtures they use, the
suite's `test_paths` and the lock files. `inBounds` takes `Task.lean` out. -/
def writable (m : Manifest) (lockFiles : List String) (s : Slug) (cs : List Component) :
    List String :=
  let apis := m.apis.filter fun a => names cs a.generator && names cs a.consumer
  let mods := cs.filterMap fun | .ofModule md => some md.name | .ofRoot _ => none
  [testsFile s, s!"tasks/{s.name}"] ++
  (impacted m.checks cs).flatMap (·.files) ++
  apis.flatMap (fun a => apiFile a :: a.paths) ++
  mods.map frameFile ++
  (m.frames.filter (mods.contains ·.moduleName)).map Frame.genDir ++
  (m.fixtures.filter (·.users.any (names cs))).flatMap (·.paths) ++
  m.suite.test_paths ++ lockFiles

/-- Whether a path a foreman commit changes lies in its scope. `added`: the
commit creates the path. `own` are the checks of the task's checks file at
that commit: a new file under their `files` is in scope too. -/
def inBounds (m : Manifest) (lockFiles : List String) (s : Slug) (cs : List Component)
    (own : List Check) (path : String) (added : Bool) : Bool :=
  !(path == taskFile s) &&
    (under path (writable m lockFiles s cs) || (added && under path (own.flatMap (·.files))))

/-- The plan stays the planner's. -/
theorem taskFile_pinned : inBounds m l s cs own (taskFile s) added = false := by
  simp [inBounds]

end Foreman
