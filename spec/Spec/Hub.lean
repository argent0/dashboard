/-
The vocabulary (spec §2, §4, §8, §10): what the hub holds and who writes to
it. Git is abstract here: a commit is parents plus a tree, a tree a snapshot
of files, the hub a map from ref to commit. The model compares slugs, refs
and paths; their git spelling lives in the decoders at the end.

The architecture and the plan at a commit come from two oracles applied to
its tree (`Oracles`): the model reads a manifest or a plan only through them.
It imports the data of both languages and none of their syntax, so their
keywords stay free as names here.
-/
import Dashboard.ArchTypes
import Dashboard.PlanTypes

namespace Spec
open Dashboard

-- ---------------------------------------------------------------------------
-- Roles (§2) and owners
-- ---------------------------------------------------------------------------

/-- Who made a commit, by the clone the player fetched it from (§4: provenance
by fetch source). The player writes merges, syncs and every ref. -/
inductive Source
  | director
  | taskAgent
  | worker
  | player
  deriving Repr, DecidableEq, Inhabited

/-- What a ledger entry belongs to: a task, or a director commit to `main`
(spelled `_main`), which is shown and never counted (§10). -/
inductive Owner
  | main
  | task (s : Slug)
  deriving Repr, DecidableEq, Inhabited

-- ---------------------------------------------------------------------------
-- Files (§8.2)
-- ---------------------------------------------------------------------------

/-- A file of a tree, named by the role the hub gives it. `rel` is a path as
segments, so rules compare prefixes with `List.isPrefixOf` and `decide`
reaches them. -/
inductive Path
  /-- `tasks/Roadmap.lean`: phases, limits, `docs_paths` (decision 6). -/
  | roadmap
  /-- `prompts.kdl`: every prompt the player sends (§19). -/
  | prompts
  /-- `tasks/<slug>/Task.lean`: the task's declaration. -/
  | taskDecl (s : Slug)
  /-- `tasks/<slug>/prompt.md`: director → worker instructions. -/
  | prompt (s : Slug)
  /-- `tasks/<slug>/reviews/r<round>.md`: a send-back's note. -/
  | review (s : Slug) (round : Nat)
  /-- Any other file under `tasks/<slug>/`: a spike's questions, baselines,
  claims and findings (§14), a contest (§18). -/
  | taskFile (s : Slug) (rel : List String)
  /-- `architecture/Tests/<slug>.lean`: the checks a task's post adds (§6.3). -/
  | tests (s : Slug)
  /-- `architecture/Tests/<slug>.worker.lean`: the checks its worker adds. -/
  | workerTests (s : Slug)
  /-- Any other file under `architecture/`. -/
  | arch (rel : List String)
  /-- Module sources, docs, test files: everything else, by its full path. -/
  | other (rel : List String)
  deriving Repr, DecidableEq, Inhabited

/-- The task a file under `tasks/<slug>/` belongs to. -/
def Path.task? : Path → Option Slug
  | .taskDecl s | .prompt s | .review s _ | .taskFile s _ => some s
  | _ => none

/-- A file of the architecture package: what the manifest oracle reads. -/
def Path.inArch : Path → Bool
  | .arch _ | .tests _ | .workerTests _ => true
  | _ => false

/-- A file of the tasks package that the plan oracle reads beyond
`architecture/`: the roadmap and the task declarations. -/
def Path.inTasks : Path → Bool
  | .roadmap | .taskDecl _ => true
  | _ => false

/-- A file's contents, named by git's blob hash. -/
structure Blob where
  id : String
  deriving Repr, DecidableEq, Inhabited

/-- A snapshot of files, one entry per path. Built by `Tree.ofList` in the
decoder's order, so equal snapshots are equal values. Directories and modes
stay outside. -/
structure Tree where
  files : List (Path × Blob) := []
  deriving Repr, DecidableEq, Inhabited

def Tree.get (t : Tree) (p : Path) : Option Blob :=
  (t.files.find? (·.1 == p)).map (·.2)

/-- The files of `t` that `keep` holds for. -/
def Tree.restrict (t : Tree) (keep : Path → Bool) : Tree :=
  ⟨t.files.filter (keep ·.1)⟩

theorem Tree.restrict_restrict (t : Tree) (p q : Path → Bool) :
    (t.restrict p).restrict q = t.restrict (fun x => p x && q x) := by
  simp [Tree.restrict, List.filter_filter, Bool.and_comm]

/-- Restricting to `q` after `p` is restricting to `q` alone, when `q`
implies `p`. -/
theorem Tree.restrict_sub (t : Tree) {p q : Path → Bool} (h : ∀ x, q x → p x) :
    (t.restrict p).restrict q = t.restrict q := by
  rw [Tree.restrict_restrict]
  congr 2
  funext x
  cases hq : q x <;> simp [h x, hq]

/-- A commit: who made it, its parents, its tree and its message (markers,
§5, are read from the subject; the player's merge carries `Task: <slug>`). -/
structure Commit where
  id : String
  author : Source
  parents : List String := []
  tree : Tree := {}
  message : String := ""
  deriving Repr, DecidableEq, Inhabited

-- ---------------------------------------------------------------------------
-- Refs (§4, §8.1)
-- ---------------------------------------------------------------------------

/-- A numbered ledger per owner: `refs/<kind>/<owner>/<n>`. -/
inductive LedgerKind
  /-- A rejected commit, kept as evidence (§5). -/
  | rejected
  /-- A commit whose check wrote outside its view (§20). -/
  | impure
  /-- A check that passed only on retry (§16). -/
  | flakes
  deriving Repr, DecidableEq, Inhabited

/-- The player's per-task records: `refs/dashboard/<kind>/<slug>`. -/
inductive DashKind
  /-- The commit the pins were taken from (§8.3). -/
  | pins
  /-- A finish held while the hub moved (§20). -/
  | held
  | escalated
  | released
  /-- The `main` commit a task branch last saw (§11). -/
  | base
  /-- The task agent's last commit, and the step it was asked (§17). -/
  | agent
  | agentAsked
  deriving Repr, DecidableEq, Inhabited

/-- A notes ref, one per record kind: `refs/notes/dashboard[-<kind>]`. -/
inductive NoteKind
  /-- `refs/notes/dashboard`: check results per commit (§9). -/
  | checks
  /-- Check results per view tree (§9, §20). -/
  | views
  | speedups
  | runsets
  | relaunches
  | rejections
  | impurities
  | imports
  | flakes
  | explorations
  | escalations
  | escalationsCleared
  | conflicts
  | budgetRaises
  | arch
  | agentReports
  deriving Repr, DecidableEq, Inhabited

/-- A fetched clone's branch before import: `refs/incoming/…` (§4). -/
inductive Incoming
  /-- `refs/incoming/director/main`. -/
  | directorMain
  /-- `refs/incoming/director/task/<slug>`. -/
  | directorTask (s : Slug)
  /-- `refs/incoming/worker/<slug>`. -/
  | worker (s : Slug)
  /-- `refs/incoming/agent/<slug>`: the task agent's. -/
  | agent (s : Slug)
  deriving Repr, DecidableEq, Inhabited

/-- Who made the commits fetched into an incoming ref. -/
def Incoming.source : Incoming → Source
  | .directorMain | .directorTask _ => .director
  | .worker _ => .worker
  | .agent _ => .taskAgent

/-- Every ref the player writes or reads in the hub. -/
inductive Ref
  /-- `refs/heads/main`. -/
  | main
  /-- `refs/heads/task/<slug>`: one per task, created at plan (§8.1). -/
  | task (s : Slug)
  /-- `refs/retired/<slug>`: a retired task's branch; the slug stays used. -/
  | retired (s : Slug)
  /-- `refs/explored/<slug>`: a concluded spike's branch (§14). -/
  | explored (s : Slug)
  | ledger (k : LedgerKind) (o : Owner) (n : Nat)
  | incoming (i : Incoming)
  | dashboard (k : DashKind) (s : Slug)
  /-- `refs/dashboard/escalation-cleared/<slug>/<n>`. -/
  | escalationCleared (s : Slug) (n : Nat)
  /-- `refs/dashboard/test/<id>`, `refs/dashboard/queue/<id>`: a run's
  snapshot, deleted after the run. -/
  | test (id : String)
  | queue (id : String)
  /-- A notes ref; `Hub.notes` holds what it annotates. -/
  | note (k : NoteKind)
  deriving Repr, DecidableEq, Inhabited

/-- The hub: every commit it holds, the refs that point at them, and the
notes, as (kind, annotated commit, note). Only the player changes it. -/
structure Hub where
  commits : List Commit := []
  refs : List (Ref × String) := []
  notes : List (NoteKind × String × Blob) := []
  deriving Repr, DecidableEq, Inhabited

def Hub.commit? (h : Hub) (id : String) : Option Commit :=
  h.commits.find? (·.id == id)

def Hub.ref? (h : Hub) (r : Ref) : Option String :=
  (h.refs.find? (·.1 == r)).map (·.2)

def Hub.tip? (h : Hub) (r : Ref) : Option Commit :=
  h.ref? r >>= h.commit?

/-- A slug is fresh when no task branch, retired or explored ref uses it
(§8.1, §9's plan row). -/
def fresh (refs : List Ref) (s : Slug) : Bool :=
  refs.all fun
    | .task t | .retired t | .explored t => t != s
    | _ => true

-- ---------------------------------------------------------------------------
-- Rejections and limits (§10)
-- ---------------------------------------------------------------------------

/-- A rejection reason; each has its own strike counter per task. -/
inductive Reason
  /-- Rebase, amend, force push, merge commit (§5). -/
  | history
  /-- A commit that fits no transition, or touches paths it may not. -/
  | ownership
  /-- A finish that changed a pinned file (§8.3). -/
  | pins
  /-- A finish outside the task's scope (§8.3). -/
  | scope
  /-- A finish whose run set fails (§6.6). -/
  | suite
  /-- An invalid plan or replan: bad slug, bad dependency, cycle. -/
  | plan
  /-- A post whose task check passes, or whose dependencies are unmet. -/
  | post
  /-- A send-back whose run set still passes. -/
  | review
  /-- An accept or trim while the task is other than `tested`. -/
  | accept
  /-- A retire of a task in flight and unescalated. -/
  | retire
  /-- A spike's finish whose findings or claims fail to validate (§14). -/
  | claims
  /-- A flaky-test-fix post whose evidence shows no flake (§15). -/
  | evidence
  /-- A suite past its `[budget]` (architect-v3 budgets). -/
  | budget
  deriving Repr, DecidableEq, Inhabited

def Reason.all : List Reason :=
  [.history, .ownership, .pins, .scope, .suite, .plan, .post, .review, .accept,
   .retire, .claims, .evidence, .budget]

def _root_.Dashboard.StrikeLimits.limit (l : StrikeLimits) : Reason → Nat
  | .history => l.history
  | .ownership => l.ownership
  | .pins => l.pins
  | .scope => l.scope
  | .suite => l.suite
  | .plan => l.plan
  | .post => l.post
  | .review => l.review
  | .accept => l.accept
  | .retire => l.retire
  | .claims => l.claims
  | .evidence => l.evidence
  | .budget => l.budget

-- ---------------------------------------------------------------------------
-- Pins and scope (§8.3)
-- ---------------------------------------------------------------------------

/-- The pinned files and their blobs at launch: a tree, the same map
`refs/dashboard/pins/<slug>` yields. A worker commit keeps each present
and equal. Stage 3 computes it from the manifest. -/
abbrev Pins := Tree

/-- What a worker's commits may change. Stage 3 computes it from the task's
`modules` in the manifest at `main`'s tip. -/
abbrev Scope := Path → Bool

-- ---------------------------------------------------------------------------
-- The two packages, as data, and their oracles (decisions 5, 7, 8)
-- ---------------------------------------------------------------------------

/-- A project's architecture: what `architecture/` declares and the exporter
writes (`config::ArchConfig` plus every `idl::Idl`). -/
structure Manifest where
  settings : Settings := {}
  modules : List Module := []
  roots : List Root := []
  apis : List Api := []
  fixtures : List Fixture := []
  checks : List Check := []
  suite : Suite := {}
  idls : List Idl := []
  frames : List Frame := []
  deriving Repr, DecidableEq, Inhabited

/-- The tasks package of a tree: the roadmap and every task it declares. -/
structure Plan where
  roadmap : Roadmap := {}
  tasks : List Task := []
  deriving Repr, DecidableEq, Inhabited

/-- The oracles for elaborating a tree's packages and exporting them, with
their stated assumptions. A failing `arch` is an `ownership` rejection, a
failing `plan` a `plan` one. -/
structure Oracles where
  arch : Tree → Except String Manifest
  plan : Tree → Except String Plan
  /-- `arch` reads `architecture/` alone (Rust keys its cache on
  `HEAD:architecture`). -/
  arch_local : ∀ t, arch t = arch (t.restrict Path.inArch)
  /-- `plan` reads `architecture/` and the tasks package. -/
  plan_local : ∀ t, plan t = plan (t.restrict fun p => p.inArch || p.inTasks)
  /-- The tasks package requires the architecture package. -/
  plan_needs_arch : ∀ t e, arch t = .error e → ∃ e', plan t = .error e'

namespace Oracles
variable (o : Oracles)

/-- Equal trees give equal manifests: coherence by construction. -/
theorem arch_coherent {t t' : Tree} (e : t = t') : o.arch t = o.arch t' := by
  rw [e]

theorem arch_eq {t t' : Tree} (e : t.restrict Path.inArch = t'.restrict Path.inArch) :
    o.arch t = o.arch t' := by
  rw [o.arch_local t, o.arch_local t', e]

/-- A commit that changes files of the tasks package alone keeps the
manifest: plan and replan leave the architecture, its export and its run
sets as they were. -/
theorem arch_of_tasks_only {t t' : Tree}
    (e : t.restrict (fun p => !p.inTasks) = t'.restrict (fun p => !p.inTasks)) :
    o.arch t = o.arch t' := by
  have sub : ∀ p : Path, p.inArch → !p.inTasks := by
    intro p; cases p <;> simp [Path.inArch, Path.inTasks]
  apply o.arch_eq
  rw [← t.restrict_sub sub, ← t'.restrict_sub sub, e]

/-- A commit that changes neither package (a prompt, a review, module code)
keeps both the manifest and the plan. -/
theorem plan_of_rest {t t' : Tree}
    (e : t.restrict (fun p => p.inArch || p.inTasks) = t'.restrict (fun p => p.inArch || p.inTasks)) :
    o.arch t = o.arch t' ∧ o.plan t = o.plan t' := by
  have sub : ∀ p : Path, p.inArch → (p.inArch || p.inTasks) := by
    intro p h; simp [h]
  refine ⟨o.arch_eq ?_, by rw [o.plan_local t, o.plan_local t', e]⟩
  rw [← t.restrict_sub sub, ← t'.restrict_sub sub, e]

end Oracles

-- ---------------------------------------------------------------------------
-- The decoder: git names to the model's values
-- ---------------------------------------------------------------------------

/-- The slug grammar (§8.1): a lowercase letter or digit, then lowercase
letters, digits and `-`. Its alphabet holds no `.` and no `/`, so every
decode below is unambiguous. -/
def validSlug (s : String) : Bool :=
  match s.toList with
  | c :: cs => (c.isLower || c.isDigit) && cs.all fun c => c.isLower || c.isDigit || c == '-'
  | [] => false

/-- A numeral without a leading zero, so `toString` gives back the text. -/
def canonNat? (s : String) : Option Nat :=
  s.toNat?.filter (toString · == s)

def slug? (s : String) : Option Slug :=
  if validSlug s then some ⟨s⟩ else none

namespace Path

def encodeSegs : Path → List String
  | .roadmap => ["tasks", "Roadmap.lean"]
  | .prompts => ["prompts.kdl"]
  | .taskDecl s => ["tasks", s.name, "Task.lean"]
  | .prompt s => ["tasks", s.name, "prompt.md"]
  | .review s n => ["tasks", s.name, "reviews", s!"r{n}.md"]
  | .taskFile s rel => "tasks" :: s.name :: rel
  | .tests s => ["architecture", "Tests", s.name ++ ".lean"]
  | .workerTests s => ["architecture", "Tests", s.name ++ ".worker.lean"]
  | .arch rel => "architecture" :: rel
  | .other rel => rel

def encode (p : Path) : String := "/".intercalate p.encodeSegs

/-- A review note's round, from `r<N>.md`. -/
def reviewRound? (f : String) : Option Nat :=
  if f.startsWith "r" && f.endsWith ".md" then canonNat? ((f.drop 1).dropEnd 3).toString
  else none

/-- `x.lean` or `x.worker.lean` under `architecture/Tests/`. -/
def testsFile? (f : String) : Option Path :=
  if f.endsWith ".worker.lean" then (slug? (f.dropEnd 12).toString).map .workerTests
  else if f.endsWith ".lean" then (slug? (f.dropEnd 5).toString).map .tests
  else none

/-- Total: a path outside every role is `.other`, a malformed name under
`tasks/` or `architecture/Tests/` falls to `.other` or `.arch`. -/
def decodeSegs : List String → Path
  | ["tasks", "Roadmap.lean"] => .roadmap
  | ["prompts.kdl"] => .prompts
  | "tasks" :: s :: rel@(_ :: _) =>
    match slug? s, rel with
    | some s, ["Task.lean"] => .taskDecl s
    | some s, ["prompt.md"] => .prompt s
    | some s, ["reviews", f] => match reviewRound? f with
      | some n => .review s n
      | none => .taskFile s rel
    | some s, _ => .taskFile s rel
    | none, _ => .other ("tasks" :: s :: rel)
  | ["architecture", "Tests", f] => (testsFile? f).getD (.arch ["Tests", f])
  | "architecture" :: rel@(_ :: _) => .arch rel
  | rel => .other rel

def decode (s : String) : Path := decodeSegs (s.splitOn "/")

end Path

/-- Builds the canonical tree of a list of files: sorted by path, the last
entry for a path kept. -/
def Tree.ofList (fs : List (Path × Blob)) : Tree :=
  let sorted := fs.mergeSort fun a b => decide (a.1.encode ≤ b.1.encode)
  ⟨sorted.foldr (fun f acc => if acc.any (·.1 == f.1) then acc else f :: acc) []⟩

/-- Why a ref name stays outside the model. -/
inductive NameError
  /-- A task branch whose slug breaks the grammar: §10's `plan` rejection. -/
  | malformed (slug : String)
  /-- A ref the player neither writes nor reads. -/
  | foreign
  deriving Repr, DecidableEq, Inhabited

def NameError.reason? : NameError → Option Reason
  | .malformed _ => some .plan
  | .foreign => none

namespace Owner

def encode : Owner → String
  | .main => "_main"
  | .task s => s.name

def decode? (s : String) : Option Owner :=
  if s == "_main" then some .main else (slug? s).map .task

end Owner

def LedgerKind.encode : LedgerKind → String
  | .rejected => "rejected" | .impure => "impure" | .flakes => "flakes"

def DashKind.encode : DashKind → String
  | .pins => "pins" | .held => "held" | .escalated => "escalated"
  | .released => "released" | .base => "base" | .agent => "agent"
  | .agentAsked => "agent-asked"

def NoteKind.encode : NoteKind → String
  | .checks => "dashboard" | .views => "dashboard-views"
  | .speedups => "dashboard-speedups" | .runsets => "dashboard-runsets"
  | .relaunches => "dashboard-relaunches" | .rejections => "dashboard-rejections"
  | .impurities => "dashboard-impurities" | .imports => "dashboard-imports"
  | .flakes => "dashboard-flakes" | .explorations => "dashboard-explorations"
  | .escalations => "dashboard-escalations"
  | .escalationsCleared => "dashboard-escalations-cleared"
  | .conflicts => "dashboard-conflicts" | .budgetRaises => "dashboard-budget-raises"
  | .arch => "dashboard-arch" | .agentReports => "dashboard-agent-reports"

def LedgerKind.all : List LedgerKind := [.rejected, .impure, .flakes]
def DashKind.all : List DashKind :=
  [.pins, .held, .escalated, .released, .base, .agent, .agentAsked]
def NoteKind.all : List NoteKind :=
  [.checks, .views, .speedups, .runsets, .relaunches, .rejections, .impurities,
   .imports, .flakes, .explorations, .escalations, .escalationsCleared, .conflicts,
   .budgetRaises, .arch, .agentReports]

namespace Ref

def encodeSegs : Ref → List String
  | .main => ["refs", "heads", "main"]
  | .task s => ["refs", "heads", "task", s.name]
  | .retired s => ["refs", "retired", s.name]
  | .explored s => ["refs", "explored", s.name]
  | .ledger k o n => ["refs", k.encode, o.encode, toString n]
  | .incoming .directorMain => ["refs", "incoming", "director", "main"]
  | .incoming (.directorTask s) => ["refs", "incoming", "director", "task", s.name]
  | .incoming (.worker s) => ["refs", "incoming", "worker", s.name]
  | .incoming (.agent s) => ["refs", "incoming", "agent", s.name]
  | .dashboard k s => ["refs", "dashboard", k.encode, s.name]
  | .escalationCleared s n => ["refs", "dashboard", "escalation-cleared", s.name, toString n]
  | .test id => ["refs", "dashboard", "test", id]
  | .queue id => ["refs", "dashboard", "queue", id]
  | .note k => ["refs", "notes", k.encode]

def encode (r : Ref) : String := "/".intercalate r.encodeSegs

/-- A slug where the hub holds only well-formed ones. -/
def slugE (s : String) : Except NameError Slug :=
  match slug? s with
  | some s => .ok s
  | none => .error .foreign

def decodeSegs : List String → Except NameError Ref
  | ["refs", "heads", "main"] => .ok .main
  | ["refs", "heads", "task", s] => match slug? s with
    | some s => .ok (.task s)
    | none => .error (.malformed s)
  | ["refs", "retired", s] => .retired <$> slugE s
  | ["refs", "explored", s] => .explored <$> slugE s
  | ["refs", "incoming", "director", "main"] => .ok (.incoming .directorMain)
  | ["refs", "incoming", "director", "task", s] => match slug? s with
    | some s => .ok (.incoming (.directorTask s))
    | none => .error (.malformed s)
  | ["refs", "incoming", "worker", s] => (.incoming ∘ .worker) <$> slugE s
  | ["refs", "incoming", "agent", s] => (.incoming ∘ .agent) <$> slugE s
  | ["refs", "dashboard", "escalation-cleared", s, n] => match canonNat? n with
    | some n => (.escalationCleared · n) <$> slugE s
    | none => .error .foreign
  | ["refs", "dashboard", "test", id] => if id.isEmpty then .error .foreign else .ok (.test id)
  | ["refs", "dashboard", "queue", id] => if id.isEmpty then .error .foreign else .ok (.queue id)
  | ["refs", "dashboard", k, s] => match DashKind.all.find? (·.encode == k) with
    | some k => .dashboard k <$> slugE s
    | none => .error .foreign
  | ["refs", "notes", k] => match NoteKind.all.find? (·.encode == k) with
    | some k => .ok (.note k)
    | none => .error .foreign
  | ["refs", k, o, n] => match LedgerKind.all.find? (·.encode == k), Owner.decode? o, canonNat? n with
    | some k, some o, some n => .ok (.ledger k o n)
    | _, _, _ => .error .foreign
  | _ => .error .foreign

def decode (s : String) : Except NameError Ref := decodeSegs (s.splitOn "/")

end Ref

def Reason.encode : Reason → String
  | .history => "history" | .ownership => "ownership" | .pins => "pins"
  | .scope => "scope" | .suite => "suite" | .plan => "plan" | .post => "post"
  | .review => "review" | .accept => "accept" | .retire => "retire"
  | .claims => "claims" | .evidence => "evidence" | .budget => "budget"

def Reason.decode? (s : String) : Option Reason := Reason.all.find? (·.encode == s)

end Spec
