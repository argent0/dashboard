// A module enters a project as one director `main` commit (architect-v3.md,
// "Adding a module"): its manifest entry and its APIs' entries in
// `Architecture.lean`, a stub IDL per API, the APIs' generated bindings,
// and the module's skeleton rendered from the project's own template
// (`architecture/scaffold/`), plus whatever `shared` files its build
// touches (a lockfile). `add` writes all of it into the director clone's
// working tree for the director to fill the IDLs and commit; `admit` is
// the player's side: the extra paths such a `main` commit may touch, and
// the check that the new module's files are exactly the scaffold's.

use crate::arch;
use crate::config::{self, ArchConfig, path_under_test_paths};
use crate::git::Git;
use crate::lean;
use std::collections::BTreeMap;
use std::path::Path;

/// The project's template for a new module, under the Lean package.
pub const DIR: &str = "architecture/scaffold";
/// The module's manifest entry: `{name}`, `{crate}` substituted.
pub const MODULE: &str = "module.lean";
/// An API's manifest entry (optional): `{api}`, `{generator}`, `{consumer}`.
pub const API: &str = "api.lean";
/// The module's files, mirrored from the project root; `{name}` and
/// `{crate}` are substituted in paths and contents.
pub const FILES: &str = "files";

/// The API entry when the scaffold has no `api.lean`.
const DEFAULT_API: &str = "api «{api}» : «{generator}» → «{consumer}» where\n  paths = [\"apis/{api}\"]\n";

/// The starter scaffold `dashboard arch gen` writes beside a new package:
/// one Rust crate per module under `crates/`.
pub fn starter() -> Vec<(String, String)> {
    vec![
        (
            format!("{DIR}/{MODULE}"),
            "-- {name}: what it owns.\nmodule «{name}» where\n  lang = rust\n  paths = [\"crates/{name}\"]\n  \
             test_cmd = \"cargo test --offline --quiet -p {name}\"\n  build_cmd = \"cargo build --offline --quiet -p {name}\"\n"
                .to_string(),
        ),
        (format!("{DIR}/{API}"), DEFAULT_API.to_string()),
        (
            format!("{DIR}/{FILES}/crates/{{name}}/Cargo.toml"),
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n".to_string(),
        ),
        (format!("{DIR}/{FILES}/crates/{{name}}/src/lib.rs"), "//! {name}\n".to_string()),
    ]
}

/// `name` with `-` as `_`: a Rust crate's identifier.
pub fn crate_ident(name: &str) -> String {
    name.replace('-', "_")
}

fn subst(text: &str, vars: &[(&str, &str)]) -> String {
    let mut out = text.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// A project's scaffold, read from a working tree or a commit.
#[derive(Debug, Clone, Default)]
pub struct Template {
    module: String,
    api: Option<String>,
    /// `(path under files/, content)`.
    files: Vec<(String, String)>,
}

fn missing() -> String {
    format!(
        "no {DIR}/{MODULE}: a new module is rendered from the project's template. Add {DIR}/ to the architecture: \
         {MODULE} (the manifest entry, with {{name}} and {{crate}}), optional {API} (with {{api}}, {{generator}}, \
         {{consumer}}) and {FILES}/ (the module's files from the project root, {{name}} and {{crate}} in paths and \
         contents); `dashboard arch gen` on a new project writes a starter to copy"
    )
}

impl Template {
    /// From `root`'s working tree.
    pub fn read(root: &Path) -> Result<Template, String> {
        let dir = root.join(DIR);
        let module = std::fs::read_to_string(dir.join(MODULE)).map_err(|_| missing())?;
        let api = std::fs::read_to_string(dir.join(API)).ok();
        let mut files = Vec::new();
        if dir.join(FILES).is_dir() {
            for f in arch::expand(&dir, &[FILES.to_string()])? {
                let rel = f.strip_prefix(FILES).unwrap_or(&f).to_string_lossy().into_owned();
                let text = std::fs::read_to_string(dir.join(&f)).map_err(|e| format!("{DIR}/{FILES}/{rel}: {e}"))?;
                files.push((rel, text));
            }
        }
        Template::new(module, api, files)
    }

    /// From the tree of `rev`; `None` when it has no scaffold.
    pub fn at(git: &Git, rev: &str) -> Result<Option<Template>, String> {
        let Some(module) = git.show_bytes(rev, &format!("{DIR}/{MODULE}")) else { return Ok(None) };
        let text = |b: Vec<u8>, p: &str| String::from_utf8(b).map_err(|_| format!("{p}: not UTF-8"));
        let module = text(module, MODULE)?;
        let api = match git.show_bytes(rev, &format!("{DIR}/{API}")) {
            Some(b) => Some(text(b, API)?),
            None => None,
        };
        let prefix = format!("{DIR}/{FILES}/");
        let mut files = Vec::new();
        for p in git.ls_tree_files(rev, &format!("{DIR}/{FILES}")) {
            let Some(rel) = p.strip_prefix(&prefix) else { continue };
            let bytes = git.show_bytes(rev, &p).ok_or_else(|| format!("{p}: unreadable"))?;
            files.push((rel.to_string(), text(bytes, &p)?));
        }
        Template::new(module, api, files).map(Some)
    }

    fn new(module: String, api: Option<String>, mut files: Vec<(String, String)>) -> Result<Template, String> {
        if files.is_empty() {
            return Err(format!("{DIR}/{FILES}/ holds no files: a new module needs at least one under its paths"));
        }
        files.sort();
        Ok(Template { module, api, files })
    }

    fn vars(name: &str) -> [(&'static str, String); 2] {
        [("name", name.to_string()), ("crate", crate_ident(name))]
    }

    /// Module `name`'s manifest entry, ending in one newline.
    pub fn module_entry(&self, name: &str) -> String {
        let v = Self::vars(name);
        let vars: Vec<(&str, &str)> = v.iter().map(|(k, v)| (*k, v.as_str())).collect();
        format!("{}\n", subst(&self.module, &vars).trim_end())
    }

    /// Module `name`'s files: `(root-relative path, content)`, sorted.
    pub fn module_files(&self, name: &str) -> Vec<(String, String)> {
        let v = Self::vars(name);
        let vars: Vec<(&str, &str)> = v.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let mut out: Vec<(String, String)> = self.files.iter().map(|(p, t)| (subst(p, &vars), subst(t, &vars))).collect();
        out.sort();
        out
    }

    /// API `a`'s manifest entry, ending in one newline.
    pub fn api_entry(&self, a: &ApiSpec) -> String {
        api_entry(self.api.as_deref(), a)
    }
}

fn api_entry(template: Option<&str>, a: &ApiSpec) -> String {
    let vars = [("api", a.name.as_str()), ("generator", a.generator.as_str()), ("consumer", a.consumer.as_str())];
    format!("{}\n", subst(template.unwrap_or(DEFAULT_API), &vars).trim_end())
}

/// One API to add: `[<name>=]<generator>:<consumer>`, named
/// `<generator>-<consumer>` by default.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiSpec {
    pub name: String,
    pub generator: String,
    pub consumer: String,
}

fn valid_name(n: &str) -> bool {
    n.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl std::str::FromStr for ApiSpec {
    type Err = String;
    fn from_str(s: &str) -> Result<ApiSpec, String> {
        let (name, pair) = match s.split_once('=') {
            Some((n, p)) => (Some(n.trim()), p),
            None => (None, s),
        };
        let (g, c) = pair
            .split_once(':')
            .ok_or_else(|| format!("{s:?}: an API is [<name>=]<generator>:<consumer>"))?;
        let (g, c) = (g.trim().to_string(), c.trim().to_string());
        let name = name.map(str::to_string).unwrap_or_else(|| format!("{g}-{c}"));
        for n in [&name, &g, &c] {
            if !valid_name(n) {
                return Err(format!("{s:?}: {n:?} is no name ([a-z][a-z0-9_-]*)"));
            }
        }
        Ok(ApiSpec { name, generator: g, consumer: c })
    }
}

/// A stub IDL for `a`: one placeholder constructor (an IDL declares at
/// least one item), for the director to replace.
pub fn idl_stub(a: &ApiSpec) -> String {
    format!(
        "-- {} → {}: what crosses it (replace the placeholder, then `dashboard arch gen`).\nimport Architecture\nopen Dashboard\n\nidl {} where\n  constructor Placeholder\n",
        a.generator,
        a.consumer,
        lean::ident(&a.name)
    )
}

/// `manifest` with `module` placed after the last `module` block (else
/// before the first `root` or `api` block and its comments, else at the
/// end) and `apis` after the last `api` block (else at the end).
pub fn insert_entries(manifest: &str, module: Option<&str>, apis: &[String]) -> String {
    let mut lines: Vec<String> = manifest.lines().map(str::to_string).collect();
    let block_end = |lines: &[String], start: usize| {
        let mut end = start + 1;
        while end < lines.len() && lines[end].starts_with([' ', '\t']) {
            end += 1;
        }
        end
    };
    let place = |lines: &mut Vec<String>, at: usize, entry: &str| {
        let mut chunk: Vec<String> = Vec::new();
        if at > 0 && !lines[at - 1].trim().is_empty() {
            chunk.push(String::new());
        }
        chunk.extend(entry.trim_end().lines().map(str::to_string));
        if at < lines.len() && !lines[at].trim().is_empty() {
            chunk.push(String::new());
        }
        lines.splice(at..at, chunk);
    };
    if let Some(entry) = module {
        let at = match lines.iter().rposition(|l| l.starts_with("module ")) {
            Some(i) => block_end(&lines, i),
            None => match lines.iter().position(|l| l.starts_with("root ") || l.starts_with("api ")) {
                Some(mut i) => {
                    while i > 0 && lines[i - 1].starts_with("--") {
                        i -= 1;
                    }
                    i
                }
                None => lines.len(),
            },
        };
        place(&mut lines, at, entry);
    }
    for entry in apis {
        let at = match lines.iter().rposition(|l| l.starts_with("api ")) {
            Some(i) => block_end(&lines, i),
            None => lines.len(),
        };
        place(&mut lines, at, entry);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// What `dashboard arch add-module` / `add-api` asks for.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub module: Option<String>,
    pub apis: Vec<ApiSpec>,
}

/// The new module's build, run in the root so lockfiles follow.
#[derive(Debug, Clone, PartialEq)]
pub enum Build {
    /// Only APIs were added.
    NoModule,
    /// The module has no `build_cmd`.
    NoCmd,
    Ran { cmd: String, ok: bool, tail: String },
}

/// What `add` did, in the working tree, uncommitted.
#[derive(Debug, Clone)]
pub struct Added {
    /// `module combat`, `api combat-tick`: the manifest entries added.
    pub entries: Vec<String>,
    /// The scaffold's files and the stub IDLs, root-relative.
    pub wrote: Vec<String>,
    /// Bindings and vendored files `arch gen` wrote.
    pub generated: Vec<String>,
    pub build: Build,
    /// Things for the director to look at, one line each.
    pub notes: Vec<String>,
}

/// Adds `req.module` (rendered from the scaffold) and `req.apis` (stub
/// IDLs) to the checkout at `root`, regenerates the bindings and runs the
/// module's `build_cmd` there, leaving everything uncommitted.
pub fn add(root: &Path, req: &Request) -> Result<Added, String> {
    if req.module.is_none() && req.apis.is_empty() {
        return Err("nothing to add: name a module or an --api".to_string());
    }
    let before = arch::load(root)?;
    let cfg = &before.cfg;
    let known = |n: &str| cfg.node(n).is_some() || req.module.as_deref() == Some(n);
    let mut entries = Vec::new();
    let mut notes = Vec::new();
    let template = match &req.module {
        Some(name) => {
            if !valid_name(name) {
                return Err(format!("{name:?} is no module name ([a-z][a-z0-9_-]*)"));
            }
            if cfg.node(name).is_some() {
                return Err(format!("{name} is already in {}", config::ARCH_LEAN_MANIFEST));
            }
            let n = cfg.modules.len() as u64;
            if cfg.max_modules > 0 && n >= cfg.max_modules {
                return Err(format!(
                    "max_modules is {m} and the manifest has {n} modules: raise it first, as its own commit, after \
                     discussing it with the operator: set `max_modules = {}` in {} and run `dashboard raise --reason \"...\"`; \
                     then add the module",
                    n + 1,
                    config::ARCH_LEAN_MANIFEST,
                    m = cfg.max_modules
                ));
            }
            Some(Template::read(root)?)
        }
        None => None,
    };
    let mut seen = Vec::new();
    for a in &req.apis {
        for end in [&a.generator, &a.consumer] {
            if !known(end) {
                return Err(format!("api {}: {end} is neither a module nor a root of the manifest", a.name));
            }
        }
        if a.generator == a.consumer {
            return Err(format!("api {}: generator and consumer are the same", a.name));
        }
        if cfg.apis.iter().any(|x| x.name == a.name) || seen.contains(&&a.name) {
            return Err(format!("api {} is already declared", a.name));
        }
        if let Some(x) = cfg.apis.iter().find(|x| x.generator == a.generator && x.consumer == a.consumer) {
            return Err(format!("{} → {} already has api {}: one API per pair", a.generator, a.consumer, x.name));
        }
        if root.join(lean::api_file(&a.name)).exists() {
            return Err(format!("{} exists", lean::api_file(&a.name)));
        }
        seen.push(&a.name);
    }
    let files = match (&req.module, &template) {
        (Some(name), Some(t)) => t.module_files(name),
        _ => Vec::new(),
    };
    if let Some((p, _)) = files.iter().find(|(p, _)| root.join(p).exists()) {
        return Err(format!("{p} exists: a new module's files come from the scaffold alone"));
    }

    // Write: the manifest, the stub IDLs, the module's files.
    let manifest_path = root.join(config::ARCH_LEAN_MANIFEST);
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", config::ARCH_LEAN_MANIFEST))?;
    let module_entry = match (&req.module, &template) {
        (Some(name), Some(t)) => {
            entries.push(format!("module {name}"));
            Some(t.module_entry(name))
        }
        _ => None,
    };
    let api_entries: Vec<String> = req
        .apis
        .iter()
        .map(|a| {
            entries.push(format!("api {}", a.name));
            match &template {
                Some(t) => t.api_entry(a),
                None => api_entry(std::fs::read_to_string(root.join(DIR).join(API)).ok().as_deref(), a),
            }
        })
        .collect();
    let edited = insert_entries(&manifest, module_entry.as_deref(), &api_entries);
    std::fs::write(&manifest_path, edited).map_err(|e| format!("{}: {e}", config::ARCH_LEAN_MANIFEST))?;
    let mut wrote = Vec::new();
    let mut write = |rel: &str, text: &str| -> Result<(), String> {
        let full = root.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).map_err(|e| format!("{rel}: {e}"))?;
        std::fs::write(&full, text).map_err(|e| format!("{rel}: {e}"))?;
        wrote.push(rel.to_string());
        Ok(())
    };
    for a in &req.apis {
        write(&lean::api_file(&a.name), &idl_stub(a))?;
    }
    for (p, text) in &files {
        write(p, text)?;
    }

    // The edited architecture: the scaffold must land under the module's
    // paths, then the bindings follow.
    let after = arch::load(root).map_err(|e| format!("the edited architecture (left in the working tree): {e}"))?;
    let mut build = Build::NoModule;
    if let Some(name) = &req.module {
        let m = after
            .cfg
            .modules
            .iter()
            .find(|m| &m.name == name)
            .ok_or_else(|| format!("{DIR}/{MODULE} declares no module {name}"))?;
        if let Some((p, _)) = files.iter().find(|(p, _)| !path_under_test_paths(p, &m.paths)) {
            return Err(format!("{p}: the scaffold writes it outside module {name}'s paths {:?}", m.paths));
        }
        if let Some(p) = m.paths.iter().find(|p| !root.join(p).exists()) {
            return Err(format!("module {name}: path {p:?} does not exist after the scaffold: give it a file in {DIR}/{FILES}/"));
        }
        build = match &m.build_cmd {
            None => Build::NoCmd,
            Some(cmd) => {
                let out = std::process::Command::new("sh")
                    .args(["-c", cmd])
                    .current_dir(root)
                    .env("DASHBOARD_MODULE", name)
                    .env("DASHBOARD_ROOT", root)
                    .output()
                    .map_err(|e| format!("{cmd}: {e}"))?;
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                let tail: Vec<&str> = text.lines().rev().take(15).collect();
                Build::Ran { cmd: cmd.clone(), ok: out.status.success(), tail: tail.into_iter().rev().collect::<Vec<_>>().join("\n") }
            }
        };
    }
    let (generated, errs) = arch::sync_bindings(root, &after, true);
    if !errs.is_empty() {
        return Err(format!("arch gen:\n{}", errs.join("\n")));
    }
    if req.module.is_some() && after.cfg.max_modules > 0 && after.cfg.modules.len() as u64 == after.cfg.max_modules {
        notes.push(format!(
            "max_modules is now at its cap ({}/{}): the next module needs a `dashboard raise` first",
            after.cfg.modules.len(),
            after.cfg.max_modules
        ));
    }
    for a in &req.apis {
        let siblings: Vec<&config::ApiConfig> = cfg.apis.iter().filter(|x| x.generator == a.generator).collect();
        if let Some(first) = siblings.first()
            && !first.impls.is_empty()
            && siblings.iter().all(|x| x.impls == first.impls)
        {
            notes.push(format!(
                "api {}: {}'s other APIs carry impl = {:?}; add it with the IDL's container if this one shares them",
                a.name, a.generator, first.impls
            ));
        }
    }
    Ok(Added { entries, wrote, generated, build, notes })
}

/// `dashboard arch add-*` works on `main` at the hub's tip, like a raise.
pub fn on_main_at_hub(hub: &Git, director: &Git) -> Result<(), String> {
    director
        .run(&["fetch", "--quiet", "hub"])
        .map_err(|e| format!("fetching the hub: {e}"))?;
    let main = hub.rev_parse("refs/heads/main").ok_or("the hub has no main")?;
    let head = director.run(&["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    if head != "refs/heads/main" || director.rev_parse("HEAD").as_deref() != Some(main.as_str()) {
        return Err(
            "a module enters on main at the hub's tip: `git checkout main && git merge --ff-only hub/main`, then add it".to_string(),
        );
    }
    Ok(())
}

/// The player's skeleton rule: `paths` are what a director `main` commit
/// `candidate` touches beyond `roadmap.toml`, `prompts.kdl`, the
/// architecture and `docs_paths`. It admits files under any API's `gen/`
/// (`arch check` pins them to the IDL) and, when the commit adds a module
/// (in `new`, absent from `old`), that module's paths and `shared` files,
/// as long as each new module's files are exactly its scaffold.
pub fn admit(git: &Git, old: Option<&ArchConfig>, new: Option<&ArchConfig>, candidate: &str, paths: &[String]) -> Result<(), String> {
    let refuse = |path: &str, why: &str| {
        Err(format!(
            "ownership: main commit touches {path:?}; a main commit changes only roadmap.toml, prompts.kdl, the architecture \
             (architecture/), docs_paths entries, APIs' gen/ bindings, and a new module's scaffold with shared files{why}"
        ))
    };
    let mut gen_dirs = Vec::new();
    for cfg in old.into_iter().chain(new) {
        for a in &cfg.apis {
            let Some(first) = a.paths.first() else { continue };
            let dir = arch::api_dir(first, |p| git.path_kind(candidate, &p.to_string_lossy()));
            gen_dirs.push(dir.join("gen").to_string_lossy().into_owned());
        }
    }
    let new_modules: Vec<&config::ModuleConfig> = match new {
        Some(n) => n
            .modules
            .iter()
            .filter(|m| !old.is_some_and(|o| o.modules.iter().any(|x| x.name == m.name)))
            .collect(),
        None => Vec::new(),
    };
    let shared = new.map(|n| n.shared.clone()).unwrap_or_default();
    let mut touched: BTreeMap<&str, &config::ModuleConfig> = BTreeMap::new();
    for path in paths {
        if path_under_test_paths(path, &gen_dirs) {
            continue;
        }
        if let Some(m) = new_modules.iter().find(|m| path_under_test_paths(path, &m.paths)) {
            touched.insert(m.name.as_str(), m);
            continue;
        }
        if !new_modules.is_empty() && path_under_test_paths(path, &shared) {
            continue;
        }
        return refuse(path, "");
    }
    if touched.is_empty() {
        return Ok(());
    }
    let template = match Template::at(git, candidate) {
        Ok(Some(t)) => t,
        Ok(None) => return Err(format!("ownership: a new module's files come from the scaffold: {}", missing())),
        Err(e) => return Err(format!("ownership: {DIR}: {e}")),
    };
    for (name, m) in touched {
        let want: BTreeMap<String, String> = template.module_files(name).into_iter().collect();
        let mut have = Vec::new();
        for p in &m.paths {
            have.extend(git.ls_tree_files(candidate, p));
        }
        have.sort();
        have.dedup();
        let differs = |what: &str| {
            Err(format!(
                "ownership: new module {name}: {what}; a new module lands exactly as its scaffold ({DIR}/) renders it \
                 (`dashboard arch add-module`), and its code comes from a task"
            ))
        };
        for p in &have {
            let Some(text) = want.get(p) else { return differs(&format!("{p} is no scaffold file")) };
            if git.show_bytes(candidate, p).as_deref() != Some(text.as_bytes()) {
                return differs(&format!("{p} differs from the scaffold"));
            }
        }
        if let Some(p) = want.keys().find(|p| !have.contains(p)) {
            return differs(&format!("{p} from the scaffold is missing"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_specs_default_their_name_to_the_pair() {
        let a: ApiSpec = "world:combat".parse().unwrap();
        assert_eq!(a, ApiSpec { name: "world-combat".into(), generator: "world".into(), consumer: "combat".into() });
        let b: ApiSpec = "ticks=combat:tick".parse().unwrap();
        assert_eq!(b.name, "ticks");
        assert!("combat".parse::<ApiSpec>().is_err());
        assert!("A:b".parse::<ApiSpec>().is_err());
    }

    #[test]
    fn entries_follow_their_kind() {
        let m = "import Dashboard\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n-- A.\nmodule «a» where\n  lang = rust\n  paths = [\"a\"]\n\n-- The root.\nroot app where\n  lang = rust\n\napi «a-app» : «a» → app where\n  paths = [\"apis/a-app\"]\n";
        let out = insert_entries(m, Some("-- B.\nmodule «b» where\n  lang = rust\n"), &["api «b-a» : «b» → «a» where\n  paths = [\"apis/b-a\"]\n".to_string()]);
        assert_eq!(
            out,
            "import Dashboard\nopen Dashboard\n\narchitecture where\n  max_modules = 3\n\n-- A.\nmodule «a» where\n  lang = rust\n  paths = [\"a\"]\n\n\
             -- B.\nmodule «b» where\n  lang = rust\n\n-- The root.\nroot app where\n  lang = rust\n\n\
             api «a-app» : «a» → app where\n  paths = [\"apis/a-app\"]\n\napi «b-a» : «b» → «a» where\n  paths = [\"apis/b-a\"]\n"
        );
        // No module yet: before the first root, with its comment.
        let s = "architecture where\n  max_modules = 1\n\n-- The root.\nroot app where\n  lang = rust\n";
        let out = insert_entries(s, Some("module «b» where\n  lang = rust\n"), &[]);
        assert_eq!(out, "architecture where\n  max_modules = 1\n\nmodule «b» where\n  lang = rust\n\n-- The root.\nroot app where\n  lang = rust\n");
    }

    #[test]
    fn the_template_substitutes_names_in_paths_and_contents() {
        let t = Template::new(
            "module «{name}» where\n  paths = [\"crates/{name}\"]\n\n".into(),
            None,
            vec![("crates/{name}/src/lib.rs".into(), "//! {crate}\nfn f() {}\n".into())],
        )
        .unwrap();
        assert_eq!(t.module_entry("ui-x"), "module «ui-x» where\n  paths = [\"crates/ui-x\"]\n");
        assert_eq!(t.module_files("ui-x"), [("crates/ui-x/src/lib.rs".to_string(), "//! ui_x\nfn f() {}\n".to_string())]);
        let a = ApiSpec { name: "a-b".into(), generator: "a".into(), consumer: "b".into() };
        assert_eq!(t.api_entry(&a), "api «a-b» : «a» → «b» where\n  paths = [\"apis/a-b\"]\n");
    }
}
