// `dashboard arch check` (architect-v3.md): language-blind architecture
// checks run as an ordinary suite check. Loads the architecture from a
// checkout (`architecture.toml` plus `*.api.kdl` files, or the Lean package
// under `architecture/`: `load`) and verifies the modules and roots budgets,
// each module's and root's byte and AST budgets, that no file belongs to two
// of them, that every file under `source_paths` is owned, and (with a
// `test_cmd`) that a module's tests pass when only its own files, its APIs'
// files and `shared` are visible, so a stray import of another module cannot
// resolve. Modules are tested generators first (the manifest's DAG).

use crate::config::{self, ArchConfig, ModuleConfig, path_under_test_paths};
use crate::idl::Idl;
use crate::lean;
use crate::sandbox::Sandbox;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tree_sitter::{Language, Node, Parser};

/// Directories never counted or copied.
const SKIP_DIRS: [&str; 5] = [".git", "target", "node_modules", ".dashboard", ".lake"];

#[derive(Debug, Default)]
pub struct Report {
    /// Every violation, one line each.
    pub problems: Vec<String>,
    /// `module <name> ast 120/200 bytes 4000/8000`-style lines.
    pub headroom: Vec<String>,
    /// The order module tests ran in: generators before consumers.
    pub order: Vec<String>,
}

impl Report {
    pub fn pass(&self) -> bool {
        self.problems.is_empty()
    }
}

/// The architecture of a checkout: the manifest, every API's IDL, and where
/// each API's bindings go.
#[derive(Debug, Default)]
pub struct Loaded {
    pub cfg: ArchConfig,
    pub idls: BTreeMap<String, Idl>,
    /// Per API, the directory its `gen/` lives under (root-relative).
    pub api_dirs: BTreeMap<String, PathBuf>,
    /// Problems reading an API's IDL (reported by `bindings`).
    pub errors: Vec<String>,
    /// The architecture is the Lean package (`architecture/`), not TOML + KDL.
    pub lean: bool,
}

impl Loaded {
    /// A TOML manifest with its `*.api.kdl` files, one per API under its paths.
    pub fn from_toml(root: &Path, cfg: ArchConfig) -> Loaded {
        let mut out = Loaded { cfg, ..Default::default() };
        for a in &out.cfg.apis {
            let files = match expand(root, &a.paths) {
                Ok(f) => f,
                Err(e) => {
                    out.errors.push(format!("api {}: {e}", a.name));
                    continue;
                }
            };
            let kdl: Vec<&PathBuf> = files.iter().filter(|f| f.to_string_lossy().ends_with(".api.kdl")).collect();
            let [path] = kdl.as_slice() else {
                out.errors.push(format!("api {}: expected exactly one *.api.kdl in its paths, found {}", a.name, kdl.len()));
                continue;
            };
            match std::fs::read_to_string(root.join(path)).map_err(|e| e.to_string()).and_then(|t| Idl::parse(&t)) {
                Ok(idl) if idl.api != a.name => {
                    out.errors.push(format!("api {}: {} declares api {:?}", a.name, path.display(), idl.api))
                }
                Ok(idl) => {
                    out.api_dirs.insert(a.name.clone(), path.parent().unwrap_or(Path::new("")).to_path_buf());
                    out.idls.insert(a.name.clone(), idl);
                }
                Err(e) => out.errors.extend(e.lines().map(|l| format!("api {}: {}: {l}", a.name, path.display()))),
            }
        }
        out
    }

    /// A Lean export: bindings go under an API's first path (a directory,
    /// or its parent).
    pub fn from_export(root: &Path, e: lean::Exported) -> Loaded {
        let mut out = Loaded { cfg: e.arch, idls: e.idls, lean: true, ..Default::default() };
        for a in &out.cfg.apis {
            let Some(first) = a.paths.first() else { continue };
            let p = PathBuf::from(first.trim_end_matches('/'));
            let dir = if root.join(&p).is_dir() { p } else { p.parent().unwrap_or(Path::new("")).to_path_buf() };
            out.api_dirs.insert(a.name.clone(), dir);
        }
        out
    }
}

/// Loads the architecture of the checkout at `root`: the Lean package when
/// `architecture/Architecture.lean` exists, else `architecture.toml`. Both
/// is an error (a project has one architecture), as is neither.
pub fn load(root: &Path) -> Result<Loaded, String> {
    let toml = root.join(config::ARCH_FILE).is_file();
    match (toml, lean::present(root)) {
        (true, true) => Err(format!(
            "both {} and {} exist: a project has one architecture; remove the TOML and the *.api.kdl files once migrated",
            config::ARCH_FILE,
            config::ARCH_LEAN_MANIFEST
        )),
        (false, false) => Err(format!("no {} and no {}", config::ARCH_FILE, config::ARCH_LEAN_MANIFEST)),
        (true, false) => {
            let text = std::fs::read_to_string(root.join(config::ARCH_FILE)).map_err(|e| format!("{}: {e}", config::ARCH_FILE))?;
            let cfg = config::arch_config_from_str(&text).map_err(|e| format!("{}: {e}", config::ARCH_FILE))?;
            Ok(Loaded::from_toml(root, cfg))
        }
        (false, true) => {
            let e = lean::export(&root.join(lean::DIR), &Sandbox::off())?;
            Ok(Loaded::from_export(root, e))
        }
    }
}

/// Every file under `paths` (files or directories, relative to `root`),
/// as root-relative paths, sorted. A missing path is an error.
pub fn expand(root: &Path, paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for p in paths {
        let full = root.join(p);
        if !full.exists() {
            return Err(format!("path {p:?} does not exist"));
        }
        walk(root, &full, &mut out)?;
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn walk(root: &Path, at: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if at.is_dir() {
        let rd = std::fs::read_dir(at).map_err(|e| format!("{}: {e}", at.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            if entry.path().is_dir() && SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            walk(root, &entry.path(), out)?;
        }
    } else if let Ok(rel) = at.strip_prefix(root) {
        out.push(rel.to_path_buf());
    }
    Ok(())
}

pub(crate) fn language(lang: &str, file: &Path) -> Option<Language> {
    let ext = file.extension()?.to_str()?;
    match (lang, ext) {
        ("rust", "rs") => Some(tree_sitter_rust::LANGUAGE.into()),
        ("typescript", "ts") => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        ("typescript", "tsx") => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        ("kotlin", "kt" | "kts") => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        _ => None,
    }
}

fn count(node: Node) -> u64 {
    let mut cursor = node.walk();
    1 + node.children(&mut cursor).map(count).sum::<u64>()
}

/// Number of tree-sitter nodes in `src`, or an error if it doesn't parse.
pub fn ast_nodes(language: Language, src: &[u8]) -> Result<u64, String> {
    let mut parser = Parser::new();
    parser.set_language(&language).map_err(|e| e.to_string())?;
    let tree = parser.parse(src, None).ok_or("parse failed")?;
    if tree.root_node().has_error() {
        return Err("syntax error".to_string());
    }
    Ok(count(tree.root_node()))
}

/// Total (ast nodes, bytes) of a module's or root's files. Files of another
/// language count toward bytes only.
pub fn measure_files(root: &Path, lang: &str, files: &[PathBuf]) -> Result<(u64, u64), String> {
    let (mut ast, mut bytes) = (0, 0);
    for f in files {
        let src = std::fs::read(root.join(f)).map_err(|e| format!("{}: {e}", f.display()))?;
        bytes += src.len() as u64;
        if let Some(l) = language(lang, f) {
            ast += ast_nodes(l, &src).map_err(|e| format!("{}: {e}", f.display()))?;
        }
    }
    Ok((ast, bytes))
}

/// Total (ast nodes, bytes) of a module's files.
pub fn measure(root: &Path, m: &ModuleConfig, files: &[PathBuf]) -> Result<(u64, u64), String> {
    measure_files(root, &m.lang, files)
}

fn over(what: &str, who: &str, used: u64, max: u64, rep: &mut Report) -> String {
    if max > 0 && used > max {
        rep.problems.push(format!("{who}: {what} {used} exceeds budget {max}"));
    }
    if max > 0 {
        format!("{what} {used}/{max}")
    } else {
        format!("{what} {used}")
    }
}

/// An API with its parsed IDL and the bindings it should have.
pub struct ApiBindings {
    pub name: String,
    pub idl: Idl,
    /// `(root-relative path, expected content)`.
    pub files: Vec<(PathBuf, String)>,
}

/// Cross-checks every API's IDL against the others and renders the bindings
/// each should have. Errors are one line each.
pub fn bindings(root: &Path, loaded: &Loaded) -> (Vec<ApiBindings>, Vec<String>) {
    let cfg = &loaded.cfg;
    let mut errs = loaded.errors.clone();
    let idls = &loaded.idls;
    let lang_of = |m: &str| cfg.node(m).map(|(l, _)| l);
    let is_wire = |a: &crate::config::ApiConfig| lang_of(&a.generator) != lang_of(&a.consumer);
    let mut out = Vec::new();
    for a in &cfg.apis {
        let Some(idl) = idls.get(a.name.as_str()) else { continue };
        let who = format!("api {}", a.name);
        if a.max_constructors > 0 && idl.constructors() > a.max_constructors {
            errs.push(format!("{who}: constructors {} exceeds budget {}", idl.constructors(), a.max_constructors));
        }
        if a.max_types > 0 && idl.slots() > a.max_types {
            errs.push(format!("{who}: T slots {} exceeds budget {}", idl.slots(), a.max_types));
        }
        if a.max_elements > 0 && idl.element_variants() > a.max_elements {
            errs.push(format!("{who}: element variants {} exceeds budget {}", idl.element_variants(), a.max_elements));
        }
        if a.max_fields > 0 && idl.element_fields() > a.max_fields {
            errs.push(format!("{who}: element fields {} exceeds budget {}", idl.element_fields(), a.max_fields));
        }
        if a.max_fns > 0 && idl.fn_count() > a.max_fns {
            errs.push(format!("{who}: fns {} exceeds budget {}", idl.fn_count(), a.max_fns));
        }
        if a.max_inputs > 0 && idl.inputs() > a.max_inputs {
            errs.push(format!("{who}: fn inputs {} exceeds budget {}", idl.inputs(), a.max_inputs));
        }
        if a.max_rules > 0 && idl.rules() > a.max_rules {
            errs.push(format!("{who}: refinement rules {} exceeds budget {}", idl.rules(), a.max_rules));
        }
        if a.max_tokens > 0 && idl.token_count() > a.max_tokens {
            errs.push(format!("{who}: tokens {} exceeds budget {}", idl.token_count(), a.max_tokens));
        }
        if loaded.lean {
            // No tunnels (architect-v3.md): every string and bytes is bounded,
            // and no bound exceeds the API's cap.
            errs.extend(idl.unbounded().into_iter().map(|e| format!("{who}: {e}")));
            let (s, b) = idl.longest();
            let cap = string_cap(a);
            if s > cap {
                errs.push(format!("{who}: a string max_len {s} exceeds the api's max_string_len {cap} (a raise: commit if it is no tunnel)"));
            }
            if a.max_bytes_len > 0 && b > a.max_bytes_len {
                errs.push(format!("{who}: a bytes max_len {b} exceeds the api's max_bytes_len {}", a.max_bytes_len));
            }
        }
        // The callee folds an input container, so the generator must consume
        // the class API for its Seq (architect-v3.md, §1). A container only
        // returned is folded by the consumer, which refs it already. A named
        // sum taken as input carries its constructors' containers in.
        let inputs: Vec<&String> = idl
            .fns
            .iter()
            .flat_map(|f| &f.inputs)
            .flat_map(|i| {
                let carried = idl.sums.iter().filter(|s| s.name == *i).flat_map(|s| s.ctors.iter().flat_map(|c| &c.args));
                std::iter::once(i).chain(carried)
            })
            .collect();
        for c in &idl.containers {
            let crate::idl::ContainerKind::Of(_, target, seq) = &c.kind else { continue };
            if inputs.contains(&&c.name)
                && let Some(ta) = cfg.apis.iter().find(|x| x.name == *target)
                && ta.consumer != a.generator
            {
                errs.push(format!(
                    "{who}: a fn takes a container of {target}.{seq}, so generator {} must consume {target} (it is for {})",
                    a.generator, ta.consumer
                ));
            }
        }
        for (target, c) in idl.container_refs() {
            let Some(ta) = cfg.apis.iter().find(|x| x.name == target) else {
                errs.push(format!("{who}: ref to unknown api {target:?}"));
                continue;
            };
            if ![&a.generator, &a.consumer].iter().any(|m| **m == ta.generator || **m == ta.consumer) {
                errs.push(format!("{who}: ref {target}.{c}: {target} shares no module with {}", a.name));
            }
            if target == a.name {
                errs.push(format!("{who}: refs its own container {c}"));
            }
            match idls.get(target) {
                Some(t) if !t.owns_generic(c) => {
                    errs.push(format!("{who}: ref {target}.{c}: {target} owns no such generic container"))
                }
                _ => {}
            }
        }
        // A class API for a TypeScript consumer: the TypeScript binding is
        // the instance (with its laws), and the consumer never calls Rust,
        // so the Rust side and its `impl` check are skipped. The same impl
        // files are checked through the Rust consumers' class APIs.
        let ts_class_api = ts_class_api(cfg, a, idl);
        if !ts_class_api
            && let Err(e) = instances_present(root, a, idl)
        {
            errs.push(format!("{who}: {e}"));
        }
        for (target, ty) in idl.refs() {
            let Some(ta) = cfg.apis.iter().find(|x| x.name == target) else {
                errs.push(format!("{who}: ref to unknown api {target:?}"));
                continue;
            };
            let linked = [&a.generator, &a.consumer]
                .iter()
                .any(|m| **m == ta.generator || **m == ta.consumer);
            if !linked {
                errs.push(format!("{who}: ref {target}.{ty}: {target} shares no module with {}", a.name));
            }
            if target == a.name {
                errs.push(format!("{who}: refs its own type {ty}"));
            }
            match idls.get(target) {
                Some(t) if !t.declares(ty) => {
                    errs.push(format!("{who}: ref {target}.{ty}: {target} declares no such type"))
                }
                Some(t) => {
                    // A re-exported token keeps its kind: a fn taking it
                    // still admits the stale/spent case.
                    if let Some(k) = t.token(ty)
                        && let Some(c) = k.kind.failure()
                    {
                        let local = idl.types.iter().find(|x| matches!(&x.kind, crate::idl::Kind::Ref(a, y) if a == target && y == ty)).map(|x| x.name.as_str()).unwrap_or(ty);
                        for f in idl.fns_missing(local, c) {
                            errs.push(format!(
                                "{who}: fn {}: takes {local} ({target}.{ty}, {}), so {} must have a `{c}` constructor",
                                f.name,
                                k.kind.word(),
                                f.returns
                            ));
                        }
                    }
                }
                None => {}
            }
            if is_wire(a) && !is_wire(ta) {
                errs.push(format!("{who}: crosses languages but refs {target}.{ty}, whose api does not"));
            }
        }
        let mut langs = vec![lang_of(&a.generator).unwrap_or(""), lang_of(&a.consumer).unwrap_or("")];
        if ts_class_api {
            langs.remove(0);
        }
        langs.dedup();
        let mut files = Vec::new();
        let Some(api_dir) = loaded.api_dirs.get(&a.name) else { continue };
        let gen_dir = api_dir.join("gen");
        let up = "../".repeat(gen_dir.components().count());
        let impls: Vec<String> = a.impls.iter().map(|i| format!("{up}{i}")).collect();
        // The container classes a TypeScript consumer may use: the owner's,
        // plus every instance declared by an API the same module consumes.
        let mut ts_classes: BTreeMap<String, Vec<crate::idl::Class>> = idl
            .container_refs()
            .iter()
            .map(|(owner, c)| {
                let mut cs: Vec<crate::idl::Class> =
                    idls.get(*owner).map(|o| o.classes().into_iter().filter(|(_, k)| k == c).map(|(k, _)| k).collect()).unwrap_or_default();
                for x in cfg.apis.iter().filter(|x| x.consumer == a.consumer) {
                    if let Some(xi) = idls.get(x.name.as_str()) {
                        cs.extend(xi.instances.iter().filter(|i| i.api == *owner && i.container == *c).map(|i| i.class));
                    }
                }
                cs.sort();
                cs.dedup();
                (format!("{owner}.{c}"), cs)
            })
            .collect();
        // Its own generics: the classes it declares for them.
        for c in &idl.containers {
            if let crate::idl::ContainerKind::Generic(cs) = &c.kind {
                let mut cs = cs.clone();
                cs.sort();
                cs.dedup();
                ts_classes.insert(format!("{}.{}", a.name, c.name), cs);
            }
        }
        for lang in langs {
            let owns_classes = !idl.classes().is_empty();
            if lang == "typescript" && owns_classes && !ts_class_api {
                errs.push(format!("{who}: a TypeScript API declares no container classes: the generated container is the instance"));
                continue;
            }
            if lang == "kotlin" && idl.has_containers() {
                errs.push(format!("{who}: no {lang} generator for elements, containers or instances yet"));
                continue;
            }
            let (ext, text) = match lang {
                "rust" => ("rs", idl.rust(is_wire(a), &impls)),
                "typescript" => ("ts", idl.typescript(is_wire(a), &ts_classes)),
                "kotlin" => ("kt", idl.kotlin(is_wire(a))),
                _ => {
                    errs.push(format!("{who}: no {lang} generator yet"));
                    continue;
                }
            };
            if lang == "typescript"
                && let Some(laws) = idl.typescript_laws(&ts_classes)
            {
                files.push((gen_dir.join(format!("{}.laws.test.ts", a.name.replace('-', "_"))), laws));
            }
            let file = gen_dir.join(if lang == "kotlin" {
                format!("{}.kt", crate::idl::pascal(&a.name))
            } else {
                format!("{}.{ext}", a.name.replace('-', "_"))
            });
            if !a.paths.iter().any(|p| path_under_test_paths(&file.to_string_lossy(), std::slice::from_ref(p))) {
                errs.push(format!("{who}: {} is not under the api's paths, so it would not be pinned", file.display()));
            }
            files.push((file, text));
        }
        out.push(ApiBindings { name: a.name.clone(), idl: idl.clone(), files });
    }
    // A ref cycle between APIs would make the generated modules recursive.
    for a in &cfg.apis {
        let mut seen = vec![a.name.as_str()];
        let mut stack = vec![a.name.as_str()];
        while let Some(cur) = stack.pop() {
            let Some(idl) = idls.get(cur) else { continue };
            for (t, _) in idl.refs().into_iter().chain(idl.container_refs()) {
                if t == a.name {
                    errs.push(format!("api {}: ref cycle through {cur}", a.name));
                } else if !seen.contains(&t) {
                    seen.push(t);
                    stack.push(t);
                }
            }
        }
    }
    errs.sort();
    errs.dedup();
    (out, errs)
}

/// A class API whose consumer is TypeScript and generator is not: only
/// containers and instances, the consumer's binding is the instance (with
/// its laws), and nothing on that side mounts the API's `impl` files.
fn ts_class_api(cfg: &ArchConfig, a: &crate::config::ApiConfig, idl: &Idl) -> bool {
    let lang_of = |m: &str| cfg.node(m).map(|(l, _)| l);
    !idl.classes().is_empty()
        && idl.only_containers()
        && lang_of(&a.consumer) == Some("typescript")
        && lang_of(&a.generator) != Some("typescript")
}

/// The cap on string `max_len`s in an API: its `max_string_len`, or 256.
pub fn string_cap(a: &crate::config::ApiConfig) -> u64 {
    if a.max_string_len > 0 { a.max_string_len } else { 256 }
}

/// Every class the API declares has an instance in its `impl` files: for
/// Rust, an `impl ... Class<..> for Container<..>` item. A heuristic like
/// the lints; the generated law tests are what judge the instance.
fn instances_present(root: &Path, a: &crate::config::ApiConfig, idl: &Idl) -> Result<(), String> {
    let classes = idl.classes();
    if classes.is_empty() {
        return Ok(());
    }
    if a.impls.is_empty() {
        return Err("declares container classes but has no `impl` files".to_string());
    }
    let mut text = String::new();
    for i in &a.impls {
        text.push_str(&std::fs::read_to_string(root.join(i)).map_err(|e| format!("impl {i}: {e}"))?);
    }
    for (class, c) in classes {
        let re = regex::Regex::new(&format!(
            r"\bimpl\s*(<[^{{]*>)?\s*(\w+::)*{}\s*(<[^{{]*>)?\s+for\s+(\w+::)*{c}\b",
            class.name()
        ))
        .unwrap();
        if !re.is_match(&text) {
            return Err(format!("class {} declared for {c}, but no `impl {} for {c}` in its impl files", class.name(), class.name()));
        }
    }
    Ok(())
}

/// Every API `module` sees in its view: those it generates or consumes,
/// plus every API they ref, transitively (their generated files name them).
fn view_apis<'a>(cfg: &'a ArchConfig, bs: &[ApiBindings], module: &str) -> Vec<&'a crate::config::ApiConfig> {
    let mut names: Vec<&str> = cfg
        .apis
        .iter()
        .filter(|a| a.generator == module || a.consumer == module)
        .map(|a| a.name.as_str())
        .collect();
    let mut i = 0;
    while i < names.len() {
        if let Some(b) = bs.iter().find(|b| b.name == names[i]) {
            for (t, _) in b.idl.refs().into_iter().chain(b.idl.container_refs()) {
                if let Some(ta) = cfg.apis.iter().find(|x| x.name == t)
                    && !names.contains(&ta.name.as_str())
                {
                    names.push(&ta.name);
                }
            }
        }
        i += 1;
    }
    cfg.apis.iter().filter(|a| names.contains(&a.name.as_str())).collect()
}

/// Writes the expected bindings (`write`), or lists those that differ, and
/// returns the paths it wrote or found stale beside any errors (bindings
/// that could be rendered are still synced when others could not). For a
/// Lean architecture the vendored library files are synced the same way.
pub fn sync_bindings(root: &Path, loaded: &Loaded, write: bool) -> (Vec<String>, Vec<String>) {
    let (bs, mut errs) = bindings(root, loaded);
    let mut stale = Vec::new();
    if loaded.lean {
        if write {
            match lean::write_vendored(root) {
                Ok(w) => stale.extend(w),
                Err(e) => errs.push(e),
            }
        } else {
            stale.extend(lean::drift(root));
        }
    }
    for b in bs {
        for (path, content) in b.files {
            let full = root.join(&path);
            if std::fs::read_to_string(&full).ok().as_deref() == Some(content.as_str()) {
                continue;
            }
            if write {
                let done = std::fs::create_dir_all(full.parent().unwrap())
                    .and_then(|_| std::fs::write(&full, content));
                if let Err(e) = done {
                    errs.push(format!("{}: {e}", path.display()));
                }
            }
            stale.push(path.display().to_string());
        }
    }
    (stale, errs)
}

/// Checks the checkout at `root` against `loaded`. `scratch` is an existing
/// directory for jail views.
pub fn check(root: &Path, loaded: &Loaded, sandbox: &Sandbox, scratch: &Path) -> Report {
    let cfg = &loaded.cfg;
    let mut rep = Report::default();
    let n = cfg.modules.len() as u64;
    if cfg.max_modules > 0 && n > cfg.max_modules {
        rep.problems
            .push(format!("modules: {n} exceeds the total modules budget {}", cfg.max_modules));
    }
    let r = cfg.roots.len() as u64;
    if cfg.max_roots > 0 && r > cfg.max_roots {
        rep.problems.push(format!("roots: {r} exceeds the total roots budget {}", cfg.max_roots));
    }
    let mut owner: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut files_of: BTreeMap<&str, Vec<PathBuf>> = BTreeMap::new();
    let nodes: Vec<(&str, &str, &[String])> = cfg
        .modules
        .iter()
        .map(|m| ("module", m.name.as_str(), m.paths.as_slice()))
        .chain(cfg.roots.iter().map(|r| ("root", r.name.as_str(), r.paths.as_slice())))
        .collect();
    for (kind, name, paths) in &nodes {
        match expand(root, paths) {
            Ok(files) => {
                for f in &files {
                    if let Some(prev) = owner.insert(f.clone(), format!("{kind} {name}")) {
                        rep.problems.push(format!("{}: belongs to both {prev} and {kind} {name}", f.display()));
                    }
                }
                files_of.insert(name, files);
            }
            Err(e) => rep.problems.push(format!("{kind} {name}: {e}")),
        }
    }
    for m in &cfg.modules {
        let Some(files) = files_of.get(m.name.as_str()) else { continue };
        let who = format!("module {}", m.name);
        match measure(root, m, files) {
            Ok((ast, bytes)) => {
                let a = over("ast", &who, ast, m.max_ast, &mut rep);
                let b = over("bytes", &who, bytes, m.max_bytes, &mut rep);
                rep.headroom.push(format!("{who}: {a}, {b}"));
            }
            Err(e) => rep.problems.push(format!("{who}: {e}")),
        }
    }
    for rt in &cfg.roots {
        let Some(files) = files_of.get(rt.name.as_str()) else { continue };
        let who = format!("root {}", rt.name);
        match measure_files(root, &rt.lang, files) {
            Ok((ast, bytes)) => {
                let a = over("ast", &who, ast, rt.max_ast, &mut rep);
                let b = over("bytes", &who, bytes, rt.max_bytes, &mut rep);
                rep.headroom.push(format!("{who}: {a}, {b}"));
            }
            Err(e) => rep.problems.push(format!("{who}: {e}")),
        }
    }
    // Ownership completeness: everything under `source_paths` has an owner.
    if !cfg.source_paths.is_empty() {
        let mut owned_paths: Vec<String> = cfg.api_paths();
        owned_paths.extend(cfg.shared.iter().cloned());
        match expand(root, &cfg.source_paths) {
            Ok(files) => {
                for f in files {
                    let s = f.to_string_lossy();
                    if !owner.contains_key(&f) && !path_under_test_paths(&s, &owned_paths) {
                        rep.problems.push(format!("{s}: owned by nobody (no module, API, root or shared entry)"));
                    }
                }
            }
            Err(e) => rep.problems.push(format!("source_paths: {e}")),
        }
    }
    let (stale, errs) = sync_bindings(root, loaded, false);
    for f in stale {
        if f.starts_with("architecture/") {
            rep.problems.push(format!("{f}: vendored dashboard library is missing or differs from this dashboard's (run `dashboard arch gen`)"));
        } else {
            rep.problems.push(format!("{f}: generated binding is missing or differs from its IDL (run `dashboard arch gen`)"));
        }
    }
    rep.problems.extend(errs);
    let (bs, _) = bindings(root, loaded);
    for b in &bs {
        let a = cfg.apis.iter().find(|a| a.name == b.name).unwrap();
        let c = if a.max_constructors > 0 { format!("/{}", a.max_constructors) } else { String::new() };
        let t = if a.max_types > 0 { format!("/{}", a.max_types) } else { String::new() };
        let mut line = format!("api {}: constructors {}{c}, T slots {}{t}", b.name, b.idl.constructors(), b.idl.slots());
        if !b.idl.fns.is_empty() || a.max_fns > 0 || a.max_inputs > 0 {
            let f = if a.max_fns > 0 { format!("/{}", a.max_fns) } else { String::new() };
            let i = if a.max_inputs > 0 { format!("/{}", a.max_inputs) } else { String::new() };
            line.push_str(&format!(", fns {}{f}, inputs {}{i}", b.idl.fn_count(), b.idl.inputs()));
        }
        if !b.idl.elements.is_empty() || a.max_elements > 0 || a.max_fields > 0 {
            let e = if a.max_elements > 0 { format!("/{}", a.max_elements) } else { String::new() };
            let f = if a.max_fields > 0 { format!("/{}", a.max_fields) } else { String::new() };
            line.push_str(&format!(", element variants {}{e}, fields {}{f}", b.idl.element_variants(), b.idl.element_fields()));
        }
        if loaded.lean {
            let (s, by) = b.idl.longest();
            line.push_str(&format!(", string max_len {s}/{}", string_cap(a)));
            if a.max_bytes_len > 0 || by > 0 {
                let cap = if a.max_bytes_len > 0 { format!("/{}", a.max_bytes_len) } else { String::new() };
                line.push_str(&format!(", bytes max_len {by}{cap}"));
            }
            for (what, used, max) in [("rules", b.idl.rules(), a.max_rules), ("tokens", b.idl.token_count(), a.max_tokens)] {
                if used > 0 || max > 0 {
                    let cap = if max > 0 { format!("/{max}") } else { String::new() };
                    line.push_str(&format!(", {what} {used}{cap}"));
                }
            }
        }
        rep.headroom.push(line);
    }
    lint(root, loaded, &files_of, sandbox, &mut rep);
    for name in cfg.topological() {
        let Some(m) = cfg.modules.iter().find(|m| m.name == name) else { continue };
        if let Some(cmd) = &m.test_cmd {
            rep.order.push(m.name.clone());
            if let Err(e) = run_view(root, cfg, &bs, m, cmd, sandbox, scratch) {
                rep.problems.push(format!("module {}: tests in its jail view: {e}", m.name));
            }
        }
    }
    rep
}

/// The consumer contract (no wildcard over an API's constructors, no wire
/// helper reading a T) in every module's and root's own files, plus any
/// `lint_cmd`.
fn lint(
    root: &Path,
    loaded: &Loaded,
    files_of: &BTreeMap<&str, Vec<PathBuf>>,
    sandbox: &Sandbox,
    rep: &mut Report,
) {
    let cfg = &loaded.cfg;
    let (bs, _) = bindings(root, loaded);
    let api_paths = cfg.api_paths();
    let nodes: Vec<(&str, &str, Option<&String>)> = cfg
        .modules
        .iter()
        .map(|m| (m.name.as_str(), m.lang.as_str(), m.lint_cmd.as_ref()))
        .chain(cfg.roots.iter().map(|r| (r.name.as_str(), r.lang.as_str(), None)))
        .collect();
    for (name, lang, lint_cmd) in nodes {
        let Some(files) = files_of.get(name) else { continue };
        let consumed: Vec<&ApiBindings> = bs
            .iter()
            .filter(|b| cfg.apis.iter().any(|a| a.name == b.name && a.consumer == name))
            .collect();
        for f in files.iter().filter(|f| !api_paths.iter().any(|p| path_under_test_paths(&f.to_string_lossy(), std::slice::from_ref(p)))) {
            let Ok(src) = std::fs::read(root.join(f)) else { continue };
            let text = String::from_utf8_lossy(&src);
            let mut found = crate::lint::wire_leaks(&text);
            for b in &consumed {
                found.extend(crate::lint::mint_calls(&text, &b.idl.token_names()));
            }
            if let Some(l) = language(lang, f) {
                found.extend(crate::lint::serialized_boundary(lang, l.clone(), &src));
                for b in &consumed {
                    found.extend(crate::lint::wildcard_arms(lang, l.clone(), &src, &b.idl));
                }
            }
            for x in found {
                rep.problems.push(format!("{}:{}: {}", f.display(), x.line, x.message));
            }
        }
        if let Some(cmd) = lint_cmd {
            let out = sandbox.command(&format!("DASHBOARD_MODULE={name} {cmd}"), root).output();
            match out {
                Err(e) => rep.problems.push(format!("module {name}: lint_cmd: {e}")),
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    match serde_json::from_str::<Vec<serde_json::Value>>(stdout.trim()) {
                        Ok(list) => {
                            for f in list {
                                let g = |k: &str| f.get(k).map(|v| v.to_string().trim_matches('"').to_string()).unwrap_or_default();
                                rep.problems.push(format!("{}:{}: {}", g("file"), g("line"), g("message")));
                            }
                        }
                        Err(_) if o.status.success() && stdout.trim().is_empty() => {}
                        Err(e) => rep.problems.push(format!(
                            "module {name}: lint_cmd gave no JSON findings ({e}), exit {:?}: {}",
                            o.status.code(),
                            String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")
                        )),
                    }
                }
            }
        }
    }
}

/// Copies only `m`'s files, its APIs' files (with the APIs they ref and
/// their `impl` instances) and `shared` into a fresh directory under
/// `scratch`, links each existing `linked` cache back into `root`, and runs
/// `cmd` there (jailed if the sandbox is on).
fn run_view(
    root: &Path,
    cfg: &ArchConfig,
    bs: &[ApiBindings],
    m: &ModuleConfig,
    cmd: &str,
    sandbox: &Sandbox,
    scratch: &Path,
) -> Result<(), String> {
    let view = scratch.join(format!("view-{}-{}", m.name, std::process::id()));
    let _ = std::fs::remove_dir_all(&view);
    let mut paths = m.paths.clone();
    paths.extend(cfg.shared.iter().cloned());
    for a in view_apis(cfg, bs, &m.name) {
        paths.extend(a.paths.iter().cloned());
        if !bs.iter().any(|b| b.name == a.name && ts_class_api(cfg, a, &b.idl)) {
            paths.extend(a.impls.iter().cloned());
        }
    }
    for f in expand(root, &paths)? {
        let to = view.join(&f);
        std::fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(root.join(&f), &to).map_err(|e| format!("{}: {e}", f.display()))?;
    }
    // A missing cache is skipped: the suite check's own `cmd` builds it first.
    for l in &cfg.linked {
        let Ok(target) = root.join(l).canonicalize() else { continue };
        let to = view.join(l.trim_end_matches('/'));
        std::fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&target, &to).map_err(|e| format!("linked {l}: {e}"))?;
    }
    let out = sandbox
        .command(cmd, &view)
        .env("DASHBOARD_MODULE", &m.name)
        .env("DASHBOARD_ROOT", root)
        .output()
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&view);
    let out = out?;
    if out.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let tail: Vec<&str> = text.lines().rev().take(15).collect();
    Err(format!("failed:\n{}", tail.into_iter().rev().collect::<Vec<_>>().join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::arch_config_from_str;

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    const MANIFEST: &str = r#"
        max_modules = 2
        shared = ["shared.txt"]
        [[module]]
        name = "a"
        lang = "rust"
        paths = ["a"]
        max_ast = 1000
        max_bytes = 1000
        test_cmd = "test ! -e b/lib.rs"
        [[module]]
        name = "b"
        lang = "rust"
        paths = ["b"]
        test_cmd = "test -e api/ab.api.kdl"
        [[api]]
        name = "ab"
        generator = "a"
        consumer = "b"
        paths = ["api"]
    "#;

    const IDL: &str = "api \"ab\"\ntype \"Id\" repr=\"string\"\nconstructor \"X\" \"Id\"\nconstructor \"Y\"\n";

    fn loaded(root: &Path, manifest: &str) -> Loaded {
        Loaded::from_toml(root, arch_config_from_str(manifest).unwrap())
    }

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "a/lib.rs", "pub fn f() -> u32 { 1 }");
        write(d.path(), "b/lib.rs", "pub fn g() {}");
        write(d.path(), "api/ab.api.kdl", IDL);
        sync_bindings(d.path(), &loaded(d.path(), MANIFEST), true);
        write(d.path(), "shared.txt", "");
        d
    }

    fn run(d: &Path, manifest: &str) -> Report {
        let scratch = tempfile::tempdir().unwrap();
        check(d, &loaded(d, manifest), &Sandbox::off(), scratch.path())
    }

    #[test]
    fn passes_and_reports_headroom() {
        let d = fixture();
        let rep = run(d.path(), MANIFEST);
        assert!(rep.pass(), "{:?}", rep.problems);
        assert!(rep.headroom[0].starts_with("module a: ast "), "{:?}", rep.headroom);
        // Generators before consumers.
        assert_eq!(rep.order, ["a", "b"]);
    }

    #[test]
    fn load_reads_toml_and_refuses_both_sources() {
        let d = fixture();
        assert!(load(d.path()).unwrap_err().contains("no architecture.toml"));
        write(d.path(), "architecture.toml", MANIFEST);
        let l = load(d.path()).unwrap();
        assert!(!l.lean && l.idls.contains_key("ab") && l.errors.is_empty(), "{:?}", l.errors);
        write(d.path(), "architecture/Architecture.lean", "");
        assert!(load(d.path()).unwrap_err().contains("one architecture"));
    }

    #[test]
    fn budgets_and_overlap_fail() {
        let d = fixture();
        let m = MANIFEST.replace("max_modules = 2", "max_modules = 1").replace("max_bytes = 1000", "max_bytes = 5");
        let rep = run(d.path(), &m);
        let all = rep.problems.join("\n");
        assert!(all.contains("total modules budget 1"), "{all}");
        assert!(all.contains("bytes 23 exceeds budget 5"), "{all}");
    }

    #[test]
    fn roots_and_ownership_completeness() {
        let d = fixture();
        write(d.path(), "bin/main.rs", "fn main() {}");
        write(d.path(), "stray/x.rs", "fn x() {}");
        let m = format!(
            "source_paths = [\"a\", \"b\", \"bin\", \"stray\", \"api\"]\nmax_roots = 1\n{MANIFEST}\n\
             [[root]]\nname = \"cli\"\nlang = \"rust\"\npaths = [\"bin\"]\nmax_bytes = 5\n"
        );
        let rep = run(d.path(), &m);
        let all = rep.problems.join("\n");
        assert!(all.contains("stray/x.rs: owned by nobody"), "{all}");
        assert!(all.contains("root cli: bytes 12 exceeds budget 5"), "{all}");
        assert!(!all.contains("bin/main.rs: owned"), "{all}");
        assert!(rep.headroom.iter().any(|h| h.starts_with("root cli: ast")), "{:?}", rep.headroom);
        // A root consumes an API like a module, and keeps the DAG honest.
        let m2 = m.replace("consumer = \"b\"", "consumer = \"cli\"");
        let rep = run(d.path(), &m2);
        assert!(!rep.problems.iter().any(|p| p.contains("unknown")), "{:?}", rep.problems);
        let cyc = format!("{MANIFEST}\n[[api]]\nname = \"ba\"\ngenerator = \"b\"\nconsumer = \"a\"\npaths = [\"api\"]\n");
        assert!(arch_config_from_str(&cyc).unwrap_err().contains("cycle: a -> b -> a"));
    }

    #[test]
    fn view_hides_other_modules() {
        let d = fixture();
        let m = MANIFEST.replace("test ! -e b/lib.rs", "test -e b/lib.rs");
        let rep = run(d.path(), &m);
        assert!(rep.problems.iter().any(|p| p.contains("module a: tests in its jail view")));
    }

    #[test]
    fn api_budgets_drift_and_languages() {
        let d = fixture();
        let m = MANIFEST.replace("paths = [\"api\"]", "paths = [\"api\"]\n        max_constructors = 1\n        max_types = 1");
        let all = run(d.path(), &m).problems.join("\n");
        assert!(all.contains("constructors 2 exceeds budget 1"), "{all}");
        assert!(!all.contains("T slots"), "{all}");
        // Hand-editing a binding is drift.
        write(d.path(), "api/gen/ab.rs", "pub enum Ab {}");
        let all = run(d.path(), MANIFEST).problems.join("\n");
        assert!(all.contains("api/gen/ab.rs: generated binding"), "{all}");
        // A pair crossing languages gets a binding per language, with wire code.
        let m = MANIFEST.replace("name = \"b\"\n        lang = \"rust\"", "name = \"b\"\n        lang = \"kotlin\"");
        let (bs, errs) = bindings(d.path(), &loaded(d.path(), &m));
        assert!(errs.is_empty(), "{errs:?}");
        let names: Vec<String> = bs[0].files.iter().map(|(p, _)| p.display().to_string()).collect();
        assert_eq!(names, ["api/gen/ab.rs", "api/gen/Ab.kt"]);
        assert!(bs[0].files.iter().all(|(_, t)| t.contains("fromJson") || t.contains("from_json")));
    }

    #[test]
    fn consumer_wildcards_and_lint_cmd_fail() {
        let d = fixture();
        write(d.path(), "b/lib.rs", "fn g(x: Ab) { match x { Ab::X(_) => 1, _ => 2 }; }");
        let m = MANIFEST.replace(
            "test_cmd = \"test -e api/ab.api.kdl\"",
            "lint_cmd = \"echo '[{\\\"file\\\":\\\"b/lib.rs\\\",\\\"line\\\":3,\\\"message\\\":\\\"custom\\\"}]'\"",
        );
        let all = run(d.path(), &m).problems.join("\n");
        assert!(all.contains("b/lib.rs:1: wildcard arm in a match over Ab"), "{all}");
        assert!(all.contains("b/lib.rs:3: custom"), "{all}");
        // The generator side may match however it likes.
        write(d.path(), "b/lib.rs", "pub fn g() {}");
        write(d.path(), "a/lib.rs", "fn f(x: Ab) { match x { Ab::X(_) => 1, _ => 2 }; }");
        let all = run(d.path(), MANIFEST).problems.join("\n");
        assert!(!all.contains("wildcard"), "{all}");
    }

    #[test]
    fn views_link_caches_and_get_module_env() {
        let d = fixture();
        write(d.path(), "cache/x.txt", "");
        let m = MANIFEST
            .replace("shared = [\"shared.txt\"]", "shared = [\"shared.txt\"]\n        linked = [\"cache\", \"absent\"]")
            .replace("test ! -e b/lib.rs", "test -L cache && test -e cache/x.txt && test ! -e absent && test \\\"$DASHBOARD_MODULE\\\" = a");
        let rep = run(d.path(), &m);
        assert!(rep.pass(), "{:?}", rep.problems);
        // Links are never copied into a budget.
        assert!(rep.headroom[0].contains("bytes 23/1000"), "{:?}", rep.headroom);
    }

    #[test]
    fn rejects_bad_manifests() {
        for (linked, err) in [("a", "overlaps"), ("a/sub", "overlaps"), (".", "relative"), ("../x", "relative"), ("/abs", "relative")] {
            let m = MANIFEST.replace("shared = [\"shared.txt\"]", &format!("linked = [{linked:?}]"));
            assert!(arch_config_from_str(&m).unwrap_err().contains(err), "{linked}");
        }
        let m = MANIFEST.replace("shared = [\"shared.txt\"]", "linked = [\"\"]");
        assert!(arch_config_from_str(&m).unwrap_err().contains("relative"));
        let dup = format!("{MANIFEST}\n[[api]]\nname=\"x\"\ngenerator=\"a\"\nconsumer=\"b\"\npaths=[\"api\"]");
        assert!(arch_config_from_str(&dup).unwrap_err().contains("one API per connected pair"));
    }
}
