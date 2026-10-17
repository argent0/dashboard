// `dashboard arch check` (architect-v3.md): language-blind architecture
// checks run as an ordinary suite check. Reads `architecture.toml` from a
// checkout and verifies the modules budget, each module's byte and AST
// budgets, that no file belongs to two modules, and (with a `test_cmd`) that
// a module's tests pass when only its own files, its APIs' files and
// `shared` are visible, so a stray import of another module cannot resolve.

use crate::config::{ArchConfig, ModuleConfig, path_under_test_paths};
use crate::idl::Idl;
use crate::sandbox::Sandbox;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tree_sitter::{Language, Node, Parser};

/// Directories never counted or copied.
const SKIP_DIRS: [&str; 4] = [".git", "target", "node_modules", ".dashboard"];

#[derive(Debug, Default)]
pub struct Report {
    /// Every violation, one line each.
    pub problems: Vec<String>,
    /// `module <name> ast 120/200 bytes 4000/8000`-style lines.
    pub headroom: Vec<String>,
}

impl Report {
    pub fn pass(&self) -> bool {
        self.problems.is_empty()
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

/// Total (ast nodes, bytes) of a module's files. Files of another language
/// count toward bytes only.
pub fn measure(root: &Path, m: &ModuleConfig, files: &[PathBuf]) -> Result<(u64, u64), String> {
    let (mut ast, mut bytes) = (0, 0);
    for f in files {
        let src = std::fs::read(root.join(f)).map_err(|e| format!("{}: {e}", f.display()))?;
        bytes += src.len() as u64;
        if let Some(l) = language(&m.lang, f) {
            ast += ast_nodes(l, &src).map_err(|e| format!("{}: {e}", f.display()))?;
        }
    }
    Ok((ast, bytes))
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

/// Parses every API's IDL, cross-checks refs, and renders the bindings each
/// should have. Errors are one line each.
pub fn bindings(root: &Path, cfg: &ArchConfig) -> (Vec<ApiBindings>, Vec<String>) {
    let mut errs = Vec::new();
    let mut idls: BTreeMap<&str, (Idl, PathBuf)> = BTreeMap::new();
    for a in &cfg.apis {
        let files = match expand(root, &a.paths) {
            Ok(f) => f,
            Err(e) => {
                errs.push(format!("api {}: {e}", a.name));
                continue;
            }
        };
        let kdl: Vec<&PathBuf> =
            files.iter().filter(|f| f.to_string_lossy().ends_with(".api.kdl")).collect();
        let [path] = kdl.as_slice() else {
            errs.push(format!("api {}: expected exactly one *.api.kdl in its paths, found {}", a.name, kdl.len()));
            continue;
        };
        match std::fs::read_to_string(root.join(path))
            .map_err(|e| e.to_string())
            .and_then(|t| Idl::parse(&t))
        {
            Ok(idl) if idl.api != a.name => {
                errs.push(format!("api {}: {} declares api {:?}", a.name, path.display(), idl.api))
            }
            Ok(idl) => {
                idls.insert(&a.name, (idl, (*path).clone()));
            }
            Err(e) => errs.push(format!("api {}: {}: {e}", a.name, path.display())),
        }
    }
    let lang_of = |m: &str| cfg.modules.iter().find(|x| x.name == m).map(|x| x.lang.as_str());
    let is_wire = |a: &crate::config::ApiConfig| lang_of(&a.generator) != lang_of(&a.consumer);
    let mut out = Vec::new();
    for a in &cfg.apis {
        let Some((idl, path)) = idls.get(a.name.as_str()) else { continue };
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
                Some((t, _)) if !t.owns_generic(c) => {
                    errs.push(format!("{who}: ref {target}.{c}: {target} owns no such generic container"))
                }
                _ => {}
            }
        }
        if let Err(e) = instances_present(root, a, idl) {
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
                Some((t, _)) if t.type_decl(ty).is_none() => {
                    errs.push(format!("{who}: ref {target}.{ty}: {target} declares no such type"))
                }
                _ => {}
            }
            if is_wire(a) && !is_wire(ta) {
                errs.push(format!("{who}: crosses languages but refs {target}.{ty}, whose api does not"));
            }
        }
        let mut langs = vec![lang_of(&a.generator).unwrap_or(""), lang_of(&a.consumer).unwrap_or("")];
        langs.dedup();
        let mut files = Vec::new();
        let gen_dir = path.parent().unwrap_or(Path::new("")).join("gen");
        let up = "../".repeat(gen_dir.components().count());
        let impls: Vec<String> = a.impls.iter().map(|i| format!("{up}{i}")).collect();
        for lang in langs {
            if lang != "rust" && idl.has_containers() {
                errs.push(format!("{who}: no {lang} generator for elements, containers or instances yet"));
                continue;
            }
            let (ext, text) = match lang {
                "rust" => ("rs", idl.rust(is_wire(a), &impls)),
                "typescript" => ("ts", idl.typescript(is_wire(a))),
                "kotlin" => ("kt", idl.kotlin(is_wire(a))),
                _ => {
                    errs.push(format!("{who}: no {lang} generator yet"));
                    continue;
                }
            };
            let file = path
                .parent()
                .unwrap_or(Path::new(""))
                .join("gen")
                .join(if lang == "kotlin" {
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
            let Some((idl, _)) = idls.get(cur) else { continue };
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
/// that could be rendered are still synced when others could not).
pub fn sync_bindings(root: &Path, cfg: &ArchConfig, write: bool) -> (Vec<String>, Vec<String>) {
    let (bs, mut errs) = bindings(root, cfg);
    let mut stale = Vec::new();
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

/// Checks the checkout at `root` against `cfg`. `scratch` is an existing
/// directory for jail views.
pub fn check(root: &Path, cfg: &ArchConfig, sandbox: &Sandbox, scratch: &Path) -> Report {
    let mut rep = Report::default();
    let n = cfg.modules.len() as u64;
    if cfg.max_modules > 0 && n > cfg.max_modules {
        rep.problems
            .push(format!("modules: {n} exceeds the total modules budget {}", cfg.max_modules));
    }
    let mut owner: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut files_of: BTreeMap<&str, Vec<PathBuf>> = BTreeMap::new();
    for m in &cfg.modules {
        match expand(root, &m.paths) {
            Ok(files) => {
                for f in &files {
                    if let Some(prev) = owner.insert(f.clone(), m.name.clone()) {
                        rep.problems.push(format!(
                            "{}: belongs to both module {prev} and module {}",
                            f.display(),
                            m.name
                        ));
                    }
                }
                files_of.insert(&m.name, files);
            }
            Err(e) => rep.problems.push(format!("module {}: {e}", m.name)),
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
    let (stale, errs) = sync_bindings(root, cfg, false);
    for f in stale {
        rep.problems.push(format!("{f}: generated binding is missing or differs from its IDL (run `dashboard arch gen`)"));
    }
    rep.problems.extend(errs);
    {
        let (bs, _) = bindings(root, cfg);
        for b in &bs {
            let a = cfg.apis.iter().find(|a| a.name == b.name).unwrap();
            let c = if a.max_constructors > 0 { format!("/{}", a.max_constructors) } else { String::new() };
            let t = if a.max_types > 0 { format!("/{}", a.max_types) } else { String::new() };
            let mut line = format!("api {}: constructors {}{c}, T slots {}{t}", b.name, b.idl.constructors(), b.idl.slots());
            if !b.idl.elements.is_empty() || a.max_elements > 0 || a.max_fields > 0 {
                let e = if a.max_elements > 0 { format!("/{}", a.max_elements) } else { String::new() };
                let f = if a.max_fields > 0 { format!("/{}", a.max_fields) } else { String::new() };
                line.push_str(&format!(", element variants {}{e}, fields {}{f}", b.idl.element_variants(), b.idl.element_fields()));
            }
            rep.headroom.push(line);
        }
    }
    lint(root, cfg, &files_of, sandbox, &mut rep);
    let (bs, _) = bindings(root, cfg);
    for m in &cfg.modules {
        if let Some(cmd) = &m.test_cmd {
            if let Err(e) = run_view(root, cfg, &bs, m, cmd, sandbox, scratch) {
                rep.problems.push(format!("module {}: tests in its jail view: {e}", m.name));
            }
        }
    }
    rep
}

/// The consumer contract (no wildcard over an API's constructors, no wire
/// helper reading a T) in every module's own files, plus any `lint_cmd`.
fn lint(
    root: &Path,
    cfg: &ArchConfig,
    files_of: &BTreeMap<&str, Vec<PathBuf>>,
    sandbox: &Sandbox,
    rep: &mut Report,
) {
    let (bs, _) = bindings(root, cfg);
    for m in &cfg.modules {
        let Some(files) = files_of.get(m.name.as_str()) else { continue };
        let consumed: Vec<&ApiBindings> = bs
            .iter()
            .filter(|b| cfg.apis.iter().any(|a| a.name == b.name && a.consumer == m.name))
            .collect();
        let api_paths = cfg.api_paths();
        for f in files.iter().filter(|f| !api_paths.iter().any(|p| path_under_test_paths(&f.to_string_lossy(), std::slice::from_ref(p)))) {
            let Ok(src) = std::fs::read(root.join(f)) else { continue };
            let mut found = crate::lint::wire_leaks(&String::from_utf8_lossy(&src));
            if let Some(l) = language(&m.lang, f) {
                for b in &consumed {
                    found.extend(crate::lint::wildcard_arms(&m.lang, l.clone(), &src, &b.idl));
                }
            }
            for x in found {
                rep.problems.push(format!("{}:{}: {}", f.display(), x.line, x.message));
            }
        }
        if let Some(cmd) = &m.lint_cmd {
            let out = sandbox.command(&format!("DASHBOARD_MODULE={} {cmd}", m.name), root).output();
            match out {
                Err(e) => rep.problems.push(format!("module {}: lint_cmd: {e}", m.name)),
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
                            "module {}: lint_cmd gave no JSON findings ({e}), exit {:?}: {}",
                            m.name,
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
        paths.extend(a.impls.iter().cloned());
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

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "a/lib.rs", "pub fn f() -> u32 { 1 }");
        write(d.path(), "b/lib.rs", "pub fn g() {}");
        write(d.path(), "api/ab.api.kdl", IDL);
        let cfg = arch_config_from_str(MANIFEST).unwrap();
        sync_bindings(d.path(), &cfg, true);
        write(d.path(), "shared.txt", "");
        d
    }

    fn run(d: &Path, manifest: &str) -> Report {
        let cfg = arch_config_from_str(manifest).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        check(d, &cfg, &Sandbox::off(), scratch.path())
    }

    #[test]
    fn passes_and_reports_headroom() {
        let d = fixture();
        let rep = run(d.path(), MANIFEST);
        assert!(rep.pass(), "{:?}", rep.problems);
        assert!(rep.headroom[0].starts_with("module a: ast "), "{:?}", rep.headroom);
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
        let cfg = arch_config_from_str(&m).unwrap();
        let (bs, errs) = bindings(d.path(), &cfg);
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
