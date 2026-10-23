/-
The architecture language a project's `architecture/` files import: its
data and its syntax. Vendored as `architecture/Architect.lean`; the plan's
data is `Planner.Types`, the exporter `Architect.Export`. The suite's
language is `Architect.Tests`, which `Tests.lean` and `Tests/*.lean`
import beside `Architecture`.
-/
import Architect.Types
import Architect.Syntax
