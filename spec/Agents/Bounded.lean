import Agents.Hub

/-!
Every goal's log ends. Every loop of a goal spends a counter, and every
counter is a count of commits in the log that the limits cap; each legal
commit lowers `fuel`, so a legal log has at most `fuel .toPlan {}`
commits: every question, its proposals, and the task with all its rounds.
-/
namespace HubLog

variable (lim : Limits) (o : Oracles)

/-- What is left to spend on the task, summed. -/
def Spent.left (s : Spent) : Nat :=
  (lim.reviewRounds - s.rounds) + (lim.contestRounds - s.contests) +
  (lim.postStrikes - s.posts) + (lim.reviewStrikes - s.reviews) +
  (lim.suiteStrikes - s.suite) + (lim.continues - s.continues) +
  (lim.conflicts - s.conflicts)

/-- A phase's place in one loop: each step forward lowers it, and each step
back spends. A question in design counts the proposals it has left. -/
def Phase.stage (s : Spent) : Phase → Nat
  | .toPlan => 12
  | .designing _ => 13 + (lim.proposals - s.refusals)
  | .posting .. => 9
  | .serving _ => 7
  | .asking _ (.contest ..) => 8
  | .asking _ (.review ..) => 6
  | .deciding _ t _ _ => if t = [] then 4 else 5
  | .doubting .. | .resolving .. => 3
  | .stopping .. => 1
  | .abandoned _ | .landed _ | .dropped .. => 0

/-- What bounds a goal's commits: what is left to spend, by loop, the
questions left, by their proposals, and the place in the loop. -/
def fuel (ph : Phase) (s : Spent) : Nat :=
  10 * s.left lim + (lim.needs - s.needs) * (lim.proposals + 16) + ph.stage lim s

theorem allowed_fuel (h : Allowed lim o ph s c) :
    fuel lim (ph.next lim o s c) (s.add o c) < fuel lim ph s := by
  cases h with
  | @ask s what hl =>
    have e : lim.needs - s.needs = (lim.needs - (s.needs + 1)) + 1 := by omega
    simp only [fuel, Phase.next, Spent.add, Phase.stage, Spent.left, e, Nat.add_mul, Nat.one_mul]
    omega
  | @drop ph s why hd =>
    cases ph <;> simp_all [Phase.awaitsPlanner, fuel, Phase.next, Spent.add, Phase.stage] <;>
      (try split) <;> omega
  | @askUnsure w k s why =>
    cases k <;> simp [fuel, Phase.next, Spent.add, Phase.stage]
  | _ =>
    simp only [fuel, Phase.next, Spent.add, land, Spent.land] <;>
      (repeat' split) <;> simp_all [Phase.stage, Spent.left] <;> (repeat' split) <;> omega

/-- A legal log's length and the fuel left never pass the start's fuel. -/
theorem Hub.spends : {l : Log} → Goal lim o l →
    l.length + fuel lim (status lim o l).phase (spent o l) ≤ fuel lim .toPlan {}
  | _, .empty => Nat.le_of_eq (Nat.zero_add _)
  | _, .commit h lg => by
    have := Hub.spends h
    have := allowed_fuel lim o lg
    simp only [List.length_cons, status, spent, Status.next] at *
    omega

/-- Every legal log of a goal, every question and the task in it, is at
most `fuel .toPlan {}` commits long. -/
theorem Hub.bounded {l : Log} (h : Goal lim o l) : l.length ≤ fuel lim .toPlan {} := by
  have := Hub.spends lim o h; omega

/-- The bound, spelled out. -/
example : fuel lim .toPlan {} =
    10 * (lim.reviewRounds + lim.contestRounds + lim.postStrikes + lim.reviewStrikes +
      lim.suiteStrikes + lim.continues + lim.conflicts) +
    lim.needs * (lim.proposals + 16) + 12 := by
  simp [fuel, Spent.left, Phase.stage]

end HubLog
