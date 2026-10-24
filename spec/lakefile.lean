import Lake
open Lake DSL

/-
The spec package: the single source of truth for the loop and the
architecture (handover.md), one library per domain. `Architect` and
`Planner` hold what a director writes with; `dashboard arch gen` vendors
`Architect`, `Architect.*` and `Planner.Types` into a project under these
same module names. `Player` is the model, importing their data alone.
-/
package spec

@[default_target]
lean_lib Architect where
  globs := #[.andSubmodules `Architect]

@[default_target]
lean_lib Planner where
  globs := #[.submodules `Planner]

@[default_target]
lean_lib Player where
  globs := #[.andSubmodules `Player]

@[default_target]
lean_lib Agents where
  globs := #[.submodules `Agents]
