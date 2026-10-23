/-
Examples for the vocabulary: every one elaborates in `lake build`.
-/
import Player

namespace Spec.Examples
open Dashboard Spec

-- Slugs: freshness reads like the hub.
example : fresh [.main, .task "alpha", .retired "beta"] "beta" = false := by decide
example : fresh [.main, .task "alpha", .retired "beta"] "gamma" = true := by decide

-- The slug grammar.
#guard validSlug "foo-bar-2" && validSlug "2foo"
#guard !validSlug "" && !validSlug "-foo" && !validSlug "Foo" && !validSlug "foo_bar"
  && !validSlug "a.b" && !validSlug "a/b"

-- Paths: `encode (decode p) = p` on real paths, the decoded value as named.
def realPaths : List (String × Path) :=
  [("tasks/Roadmap.lean", .roadmap),
   ("prompts.kdl", .prompts),
   ("tasks/alpha/Task.lean", .taskDecl "alpha"),
   ("tasks/alpha/prompt.md", .prompt "alpha"),
   ("tasks/alpha/reviews/r2.md", .review "alpha" 2),
   ("tasks/alpha/reviews/notes.md", .taskFile "alpha" ["reviews", "notes.md"]),
   ("tasks/alpha/reviews/r02.md", .taskFile "alpha" ["reviews", "r02.md"]),
   ("tasks/alpha/questions/1.toml", .taskFile "alpha" ["questions", "1.toml"]),
   ("tasks/alpha/contests/c1.md", .taskFile "alpha" ["contests", "c1.md"]),
   ("tasks/Alpha/Task.lean", .other ["tasks", "Alpha", "Task.lean"]),
   ("architecture/Tests.lean", .arch ["Tests.lean"]),
   ("architecture/Tests/x.lean", .tests "x"),
   ("architecture/Tests/x.worker.lean", .workerTests "x"),
   ("architecture/Tests/X.lean", .arch ["Tests", "X.lean"]),
   ("architecture/Apis/core-alpha.lean", .arch ["Apis", "core-alpha.lean"]),
   ("crates/core/src/lib.rs", .other ["crates", "core", "src", "lib.rs"]),
   ("roadmap.toml", .other ["roadmap.toml"])]

#guard realPaths.all fun (s, p) => Path.decode s == p && p.encode == s

-- Refs: every namespace the Rust player writes, both ways.
def realRefs : List (String × Ref) :=
  [("refs/heads/main", .main),
   ("refs/heads/task/alpha", .task "alpha"),
   ("refs/retired/alpha", .retired "alpha"),
   ("refs/explored/alpha", .explored "alpha"),
   ("refs/rejected/alpha/3", .ledger .rejected (.task "alpha") 3),
   ("refs/rejected/_main/0", .ledger .rejected .main 0),
   ("refs/impure/alpha/1", .ledger .impure (.task "alpha") 1),
   ("refs/flakes/_main/2", .ledger .flakes .main 2),
   ("refs/incoming/director/main", .incoming .directorMain),
   ("refs/incoming/director/task/alpha", .incoming (.directorTask "alpha")),
   ("refs/incoming/worker/alpha", .incoming (.worker "alpha")),
   ("refs/incoming/agent/alpha", .incoming (.agent "alpha")),
   ("refs/dashboard/pins/alpha", .dashboard .pins "alpha"),
   ("refs/dashboard/agent-asked/alpha", .dashboard .agentAsked "alpha"),
   ("refs/dashboard/escalation-cleared/alpha/1", .escalationCleared "alpha" 1),
   ("refs/dashboard/test/0000001730000000-alpha", .test "0000001730000000-alpha"),
   ("refs/dashboard/queue/7", .queue "7"),
   ("refs/notes/dashboard", .note .checks),
   ("refs/notes/dashboard-escalations-cleared", .note .escalationsCleared),
   ("refs/notes/dashboard-agent-reports", .note .agentReports)]

def decodes (s : String) (r : Ref) : Bool :=
  match Ref.decode s with
  | .ok r' => r' == r
  | .error _ => false

def fails (s : String) (e : NameError) : Bool :=
  match Ref.decode s with
  | .ok _ => false
  | .error e' => e' == e

#guard realRefs.all fun (s, r) => decodes s r && r.encode == s

-- Every note and dashboard kind round-trips.
#guard NoteKind.all.all fun k => decodes (Ref.note k).encode (.note k)
#guard DashKind.all.all fun k => decodes (Ref.dashboard k "a").encode (.dashboard k "a")

-- A malformed task branch is §10's `plan` rejection; a foreign ref none.
#guard fails "refs/heads/task/Alpha" (.malformed "Alpha")
#guard fails "refs/incoming/director/task/a.b" (.malformed "a.b")
#guard (NameError.malformed "Alpha").reason? == some .plan
#guard fails "refs/x/from" .foreign
#guard fails "refs/rejected/alpha/03" .foreign

-- Reasons: the thirteen of `config::Reason`, spelled as Rust spells them.
#guard Reason.all.length == 13
#guard Reason.all.all fun r => Reason.decode? r.encode == some r
example : ({} : StrikeLimits).limit .evidence = 1 := by decide

-- Rules read as pattern matches: `path_allowed` (`src/player.rs`).
def directorMainOk (docs : List (List String)) : Path → Bool
  | .roadmap | .prompts | .arch _ | .tests _ | .workerTests _ => true
  | .other rel => docs.any (·.isPrefixOf rel)
  | _ => false

example : directorMainOk [["docs", "adr"]] (.other ["docs", "adr", "0001.md"]) = true := by decide
example : directorMainOk [["docs", "adr"]] (.other ["docs", "adr-other", "0001.md"]) = false := by decide
example : directorMainOk [] (.taskDecl "alpha") = false := by decide

-- `is_trim`: the commit leaves `tasks/<slug>/` alone.
def isTrim (s : Slug) (changed : List Path) : Bool := changed.all (·.task? != some s)

example : isTrim "alpha" [.tests "alpha", .other ["tests", "a.rs"]] = true := by decide
example : isTrim "alpha" [.review "alpha" 1] = false := by decide
example : isTrim "alpha" [.taskFile "alpha" ["reviews", "notes.md"]] = false := by decide

-- Trees: equal snapshots are equal values, whatever the listing order.
def b (s : String) : Blob := ⟨s⟩
#guard Tree.ofList [(.prompts, b "1"), (.roadmap, b "2")]
  == Tree.ofList [(.roadmap, b "2"), (.prompts, b "1")]
#guard (Tree.ofList [(.prompts, b "1"), (.prompts, b "3")]).get .prompts == some (b "3")

-- The oracles' assumptions hold for a real oracle: a lookup from the blob
-- of `architecture/Architecture.lean` to a manifest, and from the blob of
-- `tasks/Roadmap.lean` to a plan.

theorem Tree.get_restrict (t : Tree) (keep : Path → Bool) (p : Path) (h : keep p) :
    (t.restrict keep).get p = t.get p := by
  simp only [Tree.get, Tree.restrict]
  induction t.files with
  | nil => rfl
  | cons f fs ih =>
    by_cases hf : f.1 = p
    · subst hf; simp [List.filter, h]
    · have hb : (f.1 == p) = false := by simpa using hf
      by_cases hk : keep f.1
      · simp only [List.filter, hk, List.find?, hb]; exact ih
      · simp only [List.filter, hk, List.find?, hb]; exact ih

def archFile : Path := .arch ["Architecture.lean"]

def lookupArch (table : List (Blob × Manifest)) (t : Tree) : Except String Manifest :=
  match t.get archFile >>= fun bl => (table.find? (·.1 == bl)).map (·.2) with
  | some m => .ok m
  | none => .error "architecture fails to elaborate"

def lookupPlan (archT : List (Blob × Manifest)) (table : List (Blob × Plan)) (t : Tree) :
    Except String Plan :=
  match lookupArch archT t, t.get .roadmap >>= fun bl => (table.find? (·.1 == bl)).map (·.2) with
  | .ok _, some p => .ok p
  | .ok _, none => .error "tasks fail to elaborate"
  | .error e, _ => .error e

def lookup (archT : List (Blob × Manifest)) (planT : List (Blob × Plan)) : Oracles where
  arch := lookupArch archT
  plan := lookupPlan archT planT
  arch_local t := by
    simp only [lookupArch]
    rw [Tree.get_restrict t _ archFile rfl]
  plan_local t := by
    simp only [lookupPlan, lookupArch]
    rw [Tree.get_restrict t _ archFile rfl, Tree.get_restrict t _ .roadmap rfl]
  plan_needs_arch t e h := by
    simp only [lookupPlan, h]
    exact ⟨e, rfl⟩

-- Probe point 2 of "Tasks package", on the real oracle: a plan commit that
-- adds `tasks/alpha/Task.lean` keeps the manifest.
def m0 : Manifest := { modules := [{ name := "core", lang := .rust }] }
def oracle := lookup [(b "a1", m0)] [(b "r1", {})]
-- Written in canonical order: `decide` stops at `Tree.ofList`'s string sort.
def atMain : Tree := ⟨[(archFile, b "a1"), (.roadmap, b "r1")]⟩
def atPlan : Tree := ⟨[(archFile, b "a1"), (.roadmap, b "r1"), (.taskDecl "alpha", b "t1")]⟩

#guard atPlan == Tree.ofList [(.taskDecl "alpha", b "t1"), (.roadmap, b "r1"), (archFile, b "a1")]
#guard (oracle.arch atMain).toOption == some m0 && (oracle.arch atPlan).toOption == some m0
example : oracle.arch atMain = oracle.arch atPlan :=
  oracle.arch_of_tasks_only (by decide)

end Spec.Examples
