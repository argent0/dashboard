import Agents.Hub

/-!
Every goal's log ends. Every loop of a goal spends a counter, and every
counter is a count of commits in the log that the limits cap; each legal
push lowers `fuel`, so a legal log has at most `fuel .toPlan {}` pushes:
every question, its proposals, and the task with all its rounds.
-/
namespace HubLog

variable (lim : Limits) (o : Oracles)

/-- The strikes left, summed over the reasons. -/
def Strikes.left (k : Strikes) : Nat :=
  (lim.strikes.ownership - k.ownership) + (lim.strikes.pins - k.pins) +
  (lim.strikes.scope - k.scope) + (lim.strikes.post - k.post) +
  (lim.strikes.review - k.review) + (lim.strikes.suite - k.suite)

/-- A strike never raises what is left. -/
theorem Strikes.left_add_le (k : Strikes) (r : Kernel.Reason) :
    (k.add r).left lim ≤ k.left lim := by
  cases r <;> simp only [Strikes.add, Strikes.left] <;> omega

/-- A strike within its limit lowers what is left. -/
theorem Strikes.left_add_lt (k : Strikes) (r : Kernel.Reason) (h : k.get r < lim.strike r) :
    (k.add r).left lim < k.left lim := by
  cases r <;> simp only [Strikes.add, Strikes.left, Strikes.get, Limits.strike] at * <;> omega

/-- What is left to spend on the task, summed. -/
def Spent.left (s : Spent) : Nat :=
  (lim.reviewRounds - s.rounds) + (lim.contestRounds - s.contests) + s.strikes.left lim +
  (lim.continues - s.continues) + (lim.conflicts - s.conflicts)

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

/-- What bounds a goal's pushes: what is left to spend, by loop, the
questions left, by their proposals, and the place in the loop. -/
def fuel (ph : Phase) (s : Spent) : Nat :=
  10 * s.left lim + (lim.needs - s.needs) * (lim.proposals + 16) + ph.stage lim s

/-- A strike lowers the fuel: where the phase stays (or goes back less
than one loop), the strike pays for it; where the task stops, the stage
drops. -/
theorem fuel_strike {s : Spent} {r : Kernel.Reason} {stay stop ph : Phase}
    (hstay : stay.stage lim { s with strikes := s.strikes.add r } < ph.stage lim s + 10)
    (hstop : stop.stage lim { s with strikes := s.strikes.add r } < ph.stage lim s) :
    fuel lim (strike lim s r stay stop) { s with strikes := s.strikes.add r } < fuel lim ph s := by
  have hle := s.strikes.left_add_le lim r
  simp only [fuel, Spent.left, strike]
  split
  · have := s.strikes.left_add_lt lim r (by omega)
    omega
  · omega

macro "stage" : tactic => `(tactic| (simp only [Phase.stage, Work.stop]; (try split) <;> omega))

theorem allowed_fuel (h : Allowed lim o ph s m p) :
    fuel lim (ph.next lim o s m p.commit) (s.add o m p.commit) < fuel lim ph s := by
  cases h with
  | @ask s m what hl =>
    have e : lim.needs - s.needs = (lim.needs - (s.needs + 1)) + 1 := by omega
    simp only [fuel, Phase.next, Spent.add, Spent.spend, Commit.refusal, Phase.stage,
      Spent.left, e, Nat.add_mul, Nat.one_mul]
    omega
  | @post p held s m rev =>
    simp only [Phase.next, Spent.add, Spent.spend]
    rcases Commit.refusal o (.post rev) with _ | r <;> dsimp only
    · simp [fuel, Phase.stage]
    · exact fuel_strike lim (by stage) (by stage)
  | @submit w s m rev =>
    simp only [Phase.next, Spent.add, Spent.spend]
    rcases Commit.refusal o (.submit rev) with _ | r <;> dsimp only
    · simp [fuel, Phase.stage]
    · exact fuel_strike lim (by stage) (by stage)
  | @sendBack w n sub s m note =>
    simp only [Phase.next, Spent.add, Spent.spend]
    rcases Commit.refusal o (.sendBack note) with _ | r <;> dsimp only
    · simp only [fuel, Spent.left, Work.stop]
      split <;> simp [Phase.stage] <;> omega
    · exact fuel_strike lim (by stage) (by stage)
  | @reply w n t why s m rev =>
    simp only [Phase.next, Spent.add, Spent.spend]
    rcases Commit.refusal o (.reply rev) with _ | r <;> dsimp only
    · simp [fuel, Phase.stage]
    · exact fuel_strike lim (by stage) (by stage)
  | @accept w t tip sub s m | @resolve w _ sub _ _ s m tip _ _ =>
    simp only [Phase.next, Spent.add, Spent.spend, Commit.refusal, land, Spent.land]
    rcases o.merge m tip with m' | paths <;> dsimp only
    · split
      · simp only [fuel, Phase.stage]; (try split) <;> omega
      · exact fuel_strike lim (by stage) (by stage)
    · simp only [fuel, Spent.left, Work.stop]
      split <;> simp only [Phase.stage] <;> (repeat' split) <;> omega
  | @drop ph s m why hd =>
    cases ph <;> simp_all [Phase.awaitsPlanner, fuel, Phase.next, Spent.add, Spent.spend,
      Commit.refusal, Phase.stage] <;> (try split) <;> omega
  | @askUnsure w k s m why =>
    cases k <;> simp [fuel, Phase.next, Spent.add, Spent.spend, Commit.refusal, Phase.stage]
  | _ =>
    simp only [fuel, Phase.next, Spent.add, Spent.spend, Commit.refusal, Work.stop] <;>
      (repeat' split) <;> simp_all [Phase.stage, Spent.left] <;> (repeat' split) <;> omega

/-- A legal log's length and the fuel left never pass the start's fuel. -/
theorem Hub.spends : {l : Log} → Goal lim o l →
    l.length + fuel lim (status lim o l).phase (status lim o l).spent ≤ fuel lim .toPlan {}
  | _, .empty => Nat.le_of_eq (Nat.zero_add _)
  | _, .commit h lg => by
    have := Hub.spends h
    have := allowed_fuel lim o lg
    simp only [List.length_cons, status, Status.next] at *
    omega

/-- Every legal log of a goal, every question and the task in it, is at
most `fuel .toPlan {}` pushes long. -/
theorem Hub.bounded {l : Log} (h : Goal lim o l) : l.length ≤ fuel lim .toPlan {} := by
  have := Hub.spends lim o h; omega

/-- The bound, spelled out. -/
example : fuel lim .toPlan {} =
    10 * (lim.reviewRounds + lim.contestRounds +
      (lim.strikes.ownership + lim.strikes.pins + lim.strikes.scope + lim.strikes.post +
        lim.strikes.review + lim.strikes.suite) +
      lim.continues + lim.conflicts) +
    lim.needs * (lim.proposals + 16) + 12 := by
  simp [fuel, Spent.left, Strikes.left, Phase.stage]

end HubLog
