/-
The structural rules at work (`Architect.Rules`): a block breaking one of
its own rules is an error on the block, and `Manifest.problems` reports the
rules that read several declarations.
-/
import Architect
import Architect.Tests

namespace Spec.Examples.Rules
open Dashboard

module alpha where
  lang = rust
  paths = ["alpha"]

module beta where
  lang = rust
  paths = ["beta/"]

root cli where
  lang = rust
  paths = ["cli"]

/-- error: module Gamma: a name is lowercase letters, digits, - and _
module Gamma: paths is empty: name its files -/
#guard_msgs in
module Gamma where
  lang = rust

/-- error: root up: path "../up" is a relative path whose segments are names (no ., .., leading /) -/
#guard_msgs in
root up where
  lang = rust
  paths = ["../up"]

api «alpha-beta» : alpha → beta where
  paths = ["api/alpha-beta"]
  «impl» = ["alpha/seq.rs"]

/-- error: api beta-alpha: impl "alpha/seq.rs" lies inside generator module beta's paths -/
#guard_msgs in
api «beta-alpha» : beta → alpha where
  paths = ["api/beta-alpha"]
  «impl» = ["alpha/seq.rs"]

/-- error: api self: the generator and the consumer are two components -/
#guard_msgs in
api self : alpha → alpha where
  paths = ["api/self"]

/-- error: fixture dice: stand_in = alpha-beta, so users are exactly alpha and beta (the stand-in compiles only in their views) -/
#guard_msgs in
fixture dice where
  users = [alpha, cli]
  paths = ["fixtures/dice"]
  stand_in = «alpha-beta»

/-- error: check slow: min_pass lies between 1 and runs (2), got 3
check slow: covers is empty: write covers = all or covers = [module, …] -/
#guard_msgs in
check slow where
  cmd = "true"
  runs = 2
  min_pass = 3

-- The cross-file rules, on manifests built as data.

def base : Manifest :=
  { modules := [alpha, beta], roots := [cli], apis := [«alpha-beta»],
    idls := [{ apiName := "alpha-beta" }] }

#guard base.wellformed

/-- A second API for the same pair, and a cycle back. -/
def twice : Manifest :=
  { base with
    apis := base.apis ++ [{ «alpha-beta» with name := "again" }, { name := "back", generator := beta, consumer := alpha, paths := ["api/back"] }],
    idls := base.idls ++ [{ apiName := "again" }, { apiName := "back" }] }

#guard twice.problems ==
  ["api again: a second api for alpha → beta (one api per connected pair)",
   "the module graph has a cycle: alpha → beta → alpha (generator → consumer is a DAG)"]

/-- An API without its IDL, an IDL without its API, a fixture over a module,
and totals over budget. -/
def loose : Manifest :=
  { base with
    settings := { max_modules := 1 },
    idls := [{ apiName := "ghost" }],
    fixtures := [{ name := "seed", users := [beta], paths := ["alpha/seed"] }] }

#guard loose.problems ==
  ["fixture seed: path \"alpha/seed\" overlaps module alpha's \"alpha\": a fixture's files are its own",
   "modules: 2 exceeds the total modules budget 1",
   "api alpha-beta: an `idl alpha-beta` block declares it (Apis/alpha-beta.lean)",
   "Apis/ghost.lean: an idl names an api the manifest declares"]

/-- A check named like a module, and a frame of a TypeScript module writing
outside it. -/
def web : Module := { name := "web", lang := .typescript, paths := ["web"] }

def clash : Manifest :=
  { base with
    modules := base.modules ++ [web],
    checks := [{ name := "alpha", cmd := "true", covers := .all }],
    frames := [{ moduleName := "web", srcDir := "alpha/src" }] }

#guard clash.problems ==
  ["check alpha: a module's implicit check has this name; name it apart",
   "frame web: frames are Rust only so far; web is typescript",
   "frame web: src \"alpha/src\" lies inside module web's paths"]

/-- `linked` and `view_rewritable`: relative, a link apart from owned paths,
a rewritable path under `shared`. -/
def links : Manifest :=
  { base with settings := { shared := ["shared.txt"], linked := ["alpha/sub", "../x"], view_rewritable := ["beta/lib.rs"] } }

#guard links.problems ==
  ["architecture: linked \"../x\" is a relative path whose segments are names (no ., .., leading /)",
   "architecture: view_rewritable \"beta/lib.rs\" lies under a shared path",
   "architecture: linked \"alpha/sub\" overlaps \"alpha\": a link stays apart from module, root and api paths"]

/-- A fixture with an undeclared user, and fixtures over an API, `shared`,
the Lean package, another fixture, `test_paths` and a check's files. -/
def ghost : Module := { name := "ghost", lang := .rust, paths := ["ghost"] }

def crowded : Manifest :=
  { base with
    settings := { shared := ["shared.txt"] },
    «suite» := { test_paths := ["tests/setup"] },
    checks := [{ name := "t", cmd := "true", covers := .only [alpha], files := ["fx/inner/t.rs"] }],
    fixtures := [
      { name := "lost", users := [ghost], paths := ["fx/lost"] },
      { name := "wide", users := [alpha], paths := ["api/alpha-beta/x", "shared.txt", "architecture/x", "fx"] },
      { name := "inner", users := [beta], paths := ["fx/inner", "tests"] }] }

#guard crowded.problems ==
  ["fixture wide: path \"architecture/x\" lies apart from architecture/ and tasks/",
   "fixture lost: ghost is a declared module or root",
   "fixture lost: path \"fx/lost\" overlaps fixture wide's \"fx\": a fixture's files are its own",
   "fixture wide: path \"api/alpha-beta/x\" overlaps api alpha-beta's \"api/alpha-beta\": a fixture's files are its own",
   "fixture wide: path \"shared.txt\" overlaps shared's \"shared.txt\": a fixture's files are its own",
   "fixture wide: path \"fx\" overlaps fixture lost's \"fx/lost\": a fixture's files are its own",
   "fixture wide: path \"fx\" overlaps fixture inner's \"fx/inner\": a fixture's files are its own",
   "fixture wide: path \"fx\" overlaps check t's \"fx/inner/t.rs\": a fixture's files are its own",
   "fixture inner: path \"fx/inner\" overlaps fixture wide's \"fx\": a fixture's files are its own",
   "fixture inner: path \"fx/inner\" overlaps check t's \"fx/inner/t.rs\": a fixture's files are its own",
   "fixture inner: path \"tests\" overlaps test_paths's \"tests/setup\": a fixture's files are its own"]

-- A stand-in's users in either order.
#guard ({ name := "dice", users := [beta, alpha], paths := ["fx/dice"], stand_in := some «alpha-beta» } : Fixture).problems == []

#guard cycle ["a", "b", "c"] [("a", "b"), ("b", "c"), ("c", "b")] == some ["b", "c", "b"]
#guard cycle ["a", "b"] [("a", "b")] == none

end Spec.Examples.Rules
