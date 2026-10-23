import Lake
open Lake DSL

/-
The spec package: the single source of truth for the loop and the
architecture (handover.md). `Dashboard` holds the files `dashboard arch gen`
vendors into a project, under the module names the project sees; `Spec`
holds the model, which imports their data alone (`Dashboard.ArchTypes`,
`Dashboard.PlanTypes`).
-/
package spec

@[default_target]
lean_lib Dashboard where
  globs := #[.andSubmodules `Dashboard]

@[default_target]
lean_lib Spec
