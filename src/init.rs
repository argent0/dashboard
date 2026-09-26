// `dashboard init`: creates a project's hub (bare repo) from a director
// clone and wires up the two-way links between them. Resolves the "how the
// hub is initialised" item spec.md leaves open in its parking lot (§13); the
// exact steps are the plan's binding Decisions.

use crate::config;
use crate::git::Git;
use crate::paths::Home;
use std::path::{Path, PathBuf};

// --------------------------------------------------------------------------
// Result
// --------------------------------------------------------------------------

#[derive(Debug)]
pub struct InitReport {
    pub home: PathBuf,
    pub hub: PathBuf,
    pub project: String,
}

impl InitReport {
    /// Short human-readable summary printed by `dashboard init`.
    pub fn summary(&self) -> String {
        format!(
            "dashboard initialized\n  project: {}\n  home:    {}\n  hub:     {}\n\nNext: run `dashboard player` in another terminal.",
            self.project,
            self.home.display(),
            self.hub.display(),
        )
    }
}

fn path_str(p: &Path) -> Result<&str, String> {
    p.to_str().ok_or_else(|| format!("{} is not valid UTF-8", p.display()))
}

// --------------------------------------------------------------------------
// init
// --------------------------------------------------------------------------

/// Runs `dashboard init` from `director_dir` (anywhere inside the director's
/// clone). `name` overrides the project name (default: the clone toplevel's
/// basename); `home` overrides the dashboard home root (default:
/// `paths::Home::for_project(name)`).
pub fn init(director_dir: &Path, name: Option<&str>, home: Option<PathBuf>) -> Result<InitReport, String> {
    // ---- resolve and validate the director clone ----
    let director = Git::new(director_dir);
    let toplevel_raw = director.run(&["rev-parse", "--show-toplevel"]).map_err(|e| format!("not inside a git clone: {e}"))?;
    let toplevel = std::fs::canonicalize(&toplevel_raw).map_err(|e| format!("{toplevel_raw}: {e}"))?;
    let director = Git::new(&toplevel);

    if !director.ref_exists("refs/heads/main") {
        return Err("director clone has no local branch `main` — create one before running `dashboard init`".to_string());
    }

    let roadmap_text = director.show_file("refs/heads/main", "roadmap.toml").ok_or_else(|| "roadmap.toml not found at main's tip".to_string())?;
    config::roadmap_config_from_str(&roadmap_text).map_err(|e| format!("roadmap.toml: {e}"))?;

    let tests_text = director.show_file("refs/heads/main", "tests.toml").ok_or_else(|| "tests.toml not found at main's tip".to_string())?;
    config::tests_config_from_str(&tests_text).map_err(|e| format!("tests.toml: {e}"))?; // also runs Check::validate on every entry

    // ---- project name and home ----
    let project = match name {
        Some(n) => n.to_string(),
        None => toplevel.file_name().map(|s| s.to_string_lossy().into_owned()).ok_or_else(|| format!("cannot derive a project name from {}", toplevel.display()))?,
    };

    let mut home = match home {
        Some(root) => Home { root },
        None => Home::for_project(&project),
    };
    if home.hub().exists() {
        return Err(format!("{} already exists — refusing to overwrite an existing hub", home.hub().display()));
    }

    // ---- create the hub ----
    std::fs::create_dir_all(&home.root).map_err(|e| format!("{}: {e}", home.root.display()))?;
    home.root = std::fs::canonicalize(&home.root).map_err(|e| format!("{}: {e}", home.root.display()))?;
    let hub_path = home.hub();

    let creator = Git::new(&home.root);
    creator.run(&["init", "--quiet", "--bare", path_str(&hub_path)?]).map_err(|e| e.to_string())?;

    let hub = Git::new(&hub_path);
    hub.run(&["symbolic-ref", "HEAD", "refs/heads/main"]).map_err(|e| e.to_string())?;
    hub.run(&["fetch", "--quiet", path_str(&toplevel)?, "refs/heads/main:refs/heads/main"]).map_err(|e| format!("fetching main into the hub: {e}"))?;

    hub.config_set("dashboard.director", path_str(&toplevel)?).map_err(|e| e.to_string())?;
    hub.config_set("dashboard.project", &project).map_err(|e| e.to_string())?;

    // ---- wire up the director clone ----
    director.config_set("dashboard.home", path_str(&home.root)?).map_err(|e| e.to_string())?;

    match director.run(&["remote", "get-url", "hub"]) {
        Ok(existing) => {
            let existing_canon = std::fs::canonicalize(&existing).ok();
            let target_canon = std::fs::canonicalize(&hub_path).ok();
            if existing_canon != target_canon {
                return Err(format!("director clone's `hub` remote already points at {existing} (expected {})", hub_path.display()));
            }
        }
        Err(_) => {
            director.run(&["remote", "add", "hub", path_str(&hub_path)?]).map_err(|e| e.to_string())?;
        }
    }
    director.run(&["fetch", "--quiet", "hub"]).map_err(|e| format!("fetching hub into the director clone: {e}"))?;

    // ---- home scaffolding ----
    std::fs::create_dir_all(home.subagents_dir()).map_err(|e| e.to_string())?;
    let events = home.events();
    if !events.exists() {
        std::fs::write(&events, "").map_err(|e| e.to_string())?;
    }

    Ok(InitReport { home: home.root, hub: hub_path, project })
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------
// Full end-to-end coverage (real git repos, the built binary) lives in
// tests/init.rs; this unit test only covers the plain error path that needs
// no fixture at all.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_outside_a_git_clone() {
        let dir = tempfile::TempDir::new().unwrap();
        let err = init(dir.path(), None, None).unwrap_err();
        assert!(err.contains("not inside a git clone"), "unexpected error: {err}");
    }
}
