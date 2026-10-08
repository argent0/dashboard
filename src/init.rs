// `dashboard init`: creates a project's hub (bare repo) from a director
// clone and wires up the two-way links between them. Resolves the "how the
// hub is initialised" item spec.md leaves open in its parking lot (§13); the
// exact steps are the plan's binding Decisions.

use crate::config;
use crate::git::Git;
use crate::paths::{self, Home};
use crate::prompts::{self, Prompts};
use crate::sandbox::Sandbox;
use std::path::{Path, PathBuf};

// --------------------------------------------------------------------------
// Result
// --------------------------------------------------------------------------

#[derive(Debug)]
pub struct InitReport {
    pub home: PathBuf,
    pub hub: PathBuf,
    pub project: String,
    pub director: PathBuf,
    /// `Sandbox::describe` of the jail that passed its preflight.
    pub sandbox: String,
}

impl InitReport {
    /// Short human-readable summary printed by `dashboard init`.
    pub fn summary(&self) -> String {
        format!(
            "dashboard initialized\n  project: {}\n  home:    {}\n  hub:     {}\n  sandbox: {}\n\nNext: run `dashboard player` in another terminal.",
            self.project,
            self.home.display(),
            self.hub.display(),
            self.sandbox,
        )
    }
}

fn path_str(p: &Path) -> Result<&str, String> {
    p.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", p.display()))
}

// --------------------------------------------------------------------------
// init
// --------------------------------------------------------------------------

/// Runs `dashboard init` from `director_dir` (anywhere inside the director's
/// clone). `name` overrides the project name (default: the clone toplevel's
/// basename); `home` overrides the dashboard home root (default:
/// `paths::Home::for_project(name)`).
pub fn init(
    director_dir: &Path,
    name: Option<&str>,
    home: Option<PathBuf>,
) -> Result<InitReport, String> {
    // ---- resolve and validate the director clone ----
    let director = Git::new(director_dir);
    let toplevel_raw = director
        .run(&["rev-parse", "--show-toplevel"])
        .map_err(|e| format!("not inside a git clone: {e}"))?;
    let toplevel =
        std::fs::canonicalize(&toplevel_raw).map_err(|e| format!("{toplevel_raw}: {e}"))?;
    let director = Git::new(&toplevel);

    if !director.ref_exists("refs/heads/main") {
        return Err("director clone has no local branch `main` — create one before running `dashboard init`".to_string());
    }

    let roadmap_text = director
        .show_file("refs/heads/main", "roadmap.toml")
        .ok_or_else(|| "roadmap.toml not found at main's tip".to_string())?;
    let roadmap =
        config::roadmap_config_from_str(&roadmap_text).map_err(|e| format!("roadmap.toml: {e}"))?;
    // Before anything is created: a host that can't jail the checks the
    // way roadmap.toml asks gets no hub at all.
    let sandbox =
        Sandbox::from_config(&roadmap.sandbox).map_err(|e| format!("roadmap.toml: {e}"))?;
    // Scratch in the director's git dir, not /tmp: the jail's private
    // /tmp would hide an "outside" dir there, proving nothing.
    let git_dir = director
        .run(&["rev-parse", "--absolute-git-dir"])
        .map_err(|e| format!("cannot find the director's git dir: {e}"))?;
    sandbox.preflight(Path::new(&git_dir))?;

    config::suite_at(&director, "refs/heads/main")?; // tests.toml + tests.d/; also runs Check::validate on every entry

    if let Some(text) = director.show_file("refs/heads/main", prompts::FILE) {
        Prompts::parse(&text)?;
    }

    // ---- project name and home ----
    let project = match name {
        Some(n) => n.to_string(),
        None => toplevel
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .ok_or_else(|| format!("cannot derive a project name from {}", toplevel.display()))?,
    };

    let mut home = match home {
        Some(root) => Home { root },
        None => Home::for_project(&project),
    };
    if home.hub().exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite an existing hub",
            home.hub().display()
        ));
    }

    // ---- create the hub ----
    std::fs::create_dir_all(&home.root).map_err(|e| format!("{}: {e}", home.root.display()))?;
    home.root =
        std::fs::canonicalize(&home.root).map_err(|e| format!("{}: {e}", home.root.display()))?;
    let hub_path = home.hub();

    // Worker clones nest inside the director's own working tree (see
    // paths::workers_root) so they inherit whatever workspace trust the
    // operator already granted it, instead of the player needing to grant
    // trust itself to a directory it creates with nobody present. Make sure
    // the director repo ignores that subfolder before the hub ever fetches
    // main, so the ignore rule ships with main's very first commit in the
    // hub.
    ensure_dashboard_gitignore(&toplevel, &home.root)?;
    ensure_prompts(&toplevel, &home.root)?;

    let creator = Git::new(&home.root);
    creator
        .run(&["init", "--quiet", "--bare", path_str(&hub_path)?])
        .map_err(|e| e.to_string())?;

    let hub = Git::new(&hub_path);
    hub.run(&["symbolic-ref", "HEAD", "refs/heads/main"])
        .map_err(|e| e.to_string())?;
    hub.run(&[
        "fetch",
        "--quiet",
        path_str(&toplevel)?,
        "refs/heads/main:refs/heads/main",
    ])
    .map_err(|e| format!("fetching main into the hub: {e}"))?;

    hub.config_set("dashboard.director", path_str(&toplevel)?)
        .map_err(|e| e.to_string())?;
    hub.config_set("dashboard.project", &project)
        .map_err(|e| e.to_string())?;

    // ---- wire up the director clone ----
    director
        .config_set("dashboard.home", path_str(&home.root)?)
        .map_err(|e| e.to_string())?;

    match director.run(&["remote", "get-url", "hub"]) {
        Ok(existing) => {
            let existing_canon = std::fs::canonicalize(&existing).ok();
            let target_canon = std::fs::canonicalize(&hub_path).ok();
            if existing_canon != target_canon {
                return Err(format!(
                    "director clone's `hub` remote already points at {existing} (expected {})",
                    hub_path.display()
                ));
            }
        }
        Err(_) => {
            director
                .run(&["remote", "add", "hub", path_str(&hub_path)?])
                .map_err(|e| e.to_string())?;
        }
    }
    director
        .run(&["fetch", "--quiet", "hub"])
        .map_err(|e| format!("fetching hub into the director clone: {e}"))?;

    // ---- home scaffolding ----
    std::fs::create_dir_all(paths::workers_root(&toplevel)).map_err(|e| e.to_string())?;
    let events = home.events();
    if !events.exists() {
        std::fs::write(&events, "").map_err(|e| e.to_string())?;
    }

    Ok(InitReport {
        home: home.root,
        hub: hub_path,
        project,
        director: toplevel,
        sandbox: sandbox.describe(),
    })
}

/// Ensures the director repo's `.gitignore` (on `refs/heads/main`) excludes
/// `/.dashboard/` (where worker clones live, `paths::workers_root`),
/// committing the change if needed.
///
/// When `main` is what's actually checked out here (the ordinary case —
/// `dashboard init` runs from the director clone with `main` current), edits
/// it in place, exactly like any other one-off setup commit the operator
/// might make themselves. Otherwise `main` isn't checked out anywhere in
/// this clone, so it's only safe to fast-forward its ref directly: building
/// the commit in a throwaway `git worktree` first (never touching whatever
/// *is* checked out or its index) and then advancing `refs/heads/main` to
/// it. Moving that ref while `main` is the checked-out branch elsewhere
/// would leave that worktree's index stale against its own HEAD — exactly
/// the bug this split avoids.
fn ensure_dashboard_gitignore(toplevel: &Path, scratch_parent: &Path) -> Result<(), String> {
    let director = Git::new(toplevel);
    let existing = director
        .show_file("refs/heads/main", ".gitignore")
        .unwrap_or_default();
    if existing.lines().any(|l| l.trim() == "/.dashboard/") {
        return Ok(());
    }

    let mut content = existing.clone();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str("/.dashboard/\n");
    commit_on_main(
        toplevel,
        scratch_parent,
        ".gitignore",
        &content,
        "dashboard: ignore worker clones (.dashboard/)",
    )
}

/// Commits the starter prompts.kdl (spec §19) on `main` if it has none; an
/// existing one was already validated.
fn ensure_prompts(toplevel: &Path, scratch_parent: &Path) -> Result<(), String> {
    if Git::new(toplevel)
        .show_file("refs/heads/main", prompts::FILE)
        .is_some()
    {
        return Ok(());
    }
    commit_on_main(
        toplevel,
        scratch_parent,
        prompts::FILE,
        prompts::STARTER,
        "dashboard: add the player's prompts (prompts.kdl)",
    )
}

/// Commits `content` as `file` on `main`: in place when `main` is checked
/// out here, else in a scratch worktree, then fast-forwarding the ref (see
/// `ensure_dashboard_gitignore`).
fn commit_on_main(
    toplevel: &Path,
    scratch_parent: &Path,
    file: &str,
    content: &str,
    message: &str,
) -> Result<(), String> {
    let director = Git::new(toplevel);
    if director
        .run(&["symbolic-ref", "-q", "HEAD"])
        .ok()
        .as_deref()
        == Some("refs/heads/main")
    {
        std::fs::write(toplevel.join(file), content).map_err(|e| e.to_string())?;
        director.run(&["add", file]).map_err(|e| e.to_string())?;
        director
            .run(&["commit", "--quiet", "-m", message])
            .map_err(|e| e.to_string())?;
        return Ok(());
    }

    let worktree_dir = scratch_parent.join("init-commit");
    let _ = std::fs::remove_dir_all(&worktree_dir);
    let tmp_branch = "__dashboard_init_commit";
    let _ = director.run(&["branch", "-D", tmp_branch]);
    director
        .run(&[
            "worktree",
            "add",
            "--quiet",
            "-B",
            tmp_branch,
            path_str(&worktree_dir)?,
            "refs/heads/main",
        ])
        .map_err(|e| format!("creating scratch worktree for {file}: {e}"))?;

    let commit_result = (|| -> Result<String, String> {
        std::fs::write(worktree_dir.join(file), content).map_err(|e| e.to_string())?;

        let scratch = Git::new(&worktree_dir);
        scratch.run(&["add", file]).map_err(|e| e.to_string())?;
        scratch
            .run(&["commit", "--quiet", "-m", message])
            .map_err(|e| e.to_string())?;
        scratch
            .rev_parse("HEAD")
            .ok_or_else(|| "commit succeeded but HEAD is unreadable".to_string())
    })();

    let _ = director.run(&["worktree", "remove", "--force", path_str(&worktree_dir)?]);
    let _ = director.run(&["branch", "-D", tmp_branch]);

    let new_tip = commit_result?;
    director
        .update_ref("refs/heads/main", &new_tip)
        .map_err(|e| e.to_string())
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
        assert!(
            err.contains("not inside a git clone"),
            "unexpected error: {err}"
        );
    }
}
