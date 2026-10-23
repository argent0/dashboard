import Lake
open Lake DSL

/-
The spec package: the single source of truth for the loop and the
architecture (handover.md). `Spec` holds the model; `Dashboard` re-exports
the DSL core under the name projects import, so `Spec/Arch/Export.lean`
keeps the bytes `dashboard arch gen` vendors.
-/
package spec

lean_lib Dashboard

@[default_target]
lean_lib Spec
