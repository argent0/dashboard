/-
A roadmap as `tasks/Roadmap.lean` writes it: importing the plan's data
alone, every field by its name. Elaborating is the test.
-/
import Planner.Types

namespace Spec.Examples.Roadmap
open Dashboard

def roadmap : Roadmap :=
  { title := some "demo",
    phases := [{ id := "p1", title := some "First", goal := some "Ship" }],
    milestones := [{ id := "mvp", title := some "MVP", after := ["alpha", "beta"] }],
    docs_paths := ["docs/adr"],
    limits :=
      { max_review_rounds := 2, max_contest_rounds := 2, max_relaunches := 2,
        strikes :=
          { history := 2, ownership := 2, pins := 2, scope := 2, suite := 3, plan := 2,
            post := 2, review := 2, accept := 2, retire := 2, claims := 2, evidence := 1,
            budget := 2 } },
    player :=
      { agent_cmd := "claude", task_agent_cmd := "claude", idle_after := 900,
        lock_files := ["Cargo.lock"], lock_regen := [("Cargo.lock", "cargo generate-lockfile")],
        runs := .parallel },
    sandbox := .jail .offline ["~/.cargo"],
    budget := { check_timeout := 60, suite_warn := 300, suite_fail := 600 } }

end Spec.Examples.Roadmap
