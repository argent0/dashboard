// XDG-based on-disk layout for a project's dashboard home (hub, subagent
// clones, player scratch worktree, event log, director cursor).

use crate::git::Git;
use std::path::{Path, PathBuf};

/// `$XDG_DATA_HOME/dashboard`, falling back to `$HOME/.local/share/dashboard`
/// when `XDG_DATA_HOME` is unset or empty.
pub fn data_root() -> PathBuf {
    match std::env::var("XDG_DATA_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v).join("dashboard"),
        _ => {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local").join("share").join("dashboard")
        }
    }
}

#[derive(Debug)]
pub struct Home {
    pub root: PathBuf,
}

impl Home {
    pub fn for_project(name: &str) -> Self {
        Self { root: data_root().join(name) }
    }

    pub fn hub(&self) -> PathBuf {
        self.root.join("hub.git")
    }

    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    pub fn events(&self) -> PathBuf {
        self.root.join("events.jsonl")
    }

    pub fn director_cursor(&self) -> PathBuf {
        self.root.join("director.cursor")
    }

    /// The player's own per-tick status snapshot (spec.md's step 7: `last`,
    /// persisted so a restart doesn't re-emit events for statuses the
    /// director has already been told about).
    pub fn player_state(&self) -> PathBuf {
        self.root.join("player-state.json")
    }

    /// Written every tick with the current unix time, so `dashboard wait`
    /// can tell the director when the player itself looks dead.
    pub fn heartbeat(&self) -> PathBuf {
        self.root.join("player.heartbeat")
    }
}

/// Where a project's subagent clones live: nested inside the *director's*
/// own working tree, never under the dashboard home. `dashboard init`
/// gitignores this subfolder.
///
/// Each one is a `git worktree` of `subagent_workspace` (never a fresh
/// `git clone`), because an agent CLI's workspace trust is scoped to a git
/// *repository*, not a filesystem path: a fresh clone is always a fresh
/// repository, and nobody is present to click through its first-run
/// trust dialog. A worktree shares its repository's identity with
/// `subagent_workspace`, so it inherits the one trust decision the operator
/// already made for that single, permanent repo instead of needing a fresh
/// one per task — safe because slugs are never reused (spec §8.1), so
/// worktrees never collide on a branch name. Parallel tasks (spec §4) do
/// share `refs/remotes/origin/*`, so two subagents fetching at the same
/// instant can see a transient ref-lock error; a retry clears it.
pub fn subagents_root(director: &Path) -> PathBuf {
    director.join(".dashboard").join("subagents")
}

pub fn subagent_clone(director: &Path, slug: &str) -> PathBuf {
    subagents_root(director).join(slug)
}

/// The one persistent clone of the hub every subagent's worktree hangs off
/// (see `subagents_root`). Created lazily on first launch, trusted once by
/// the operator, and never deleted.
pub fn subagent_workspace(director: &Path) -> PathBuf {
    director.join(".dashboard").join("workspace")
}

/// Resolves the dashboard home: the explicit `--home` path if given, else
/// `git config dashboard.home` read from `cwd`'s repo.
pub fn resolve_home(explicit: Option<PathBuf>, cwd: &Path) -> Result<Home, String> {
    if let Some(root) = explicit {
        return Ok(Home { root });
    }
    let git = Git::new(cwd);
    match git.config_get("dashboard.home") {
        Some(v) if !v.is_empty() => Ok(Home { root: PathBuf::from(v) }),
        _ => Err("no dashboard home configured for this repo — run `dashboard init` or pass --home".to_string()),
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // XDG_DATA_HOME/HOME are process-global; every test that mutates them
    // takes this lock first so they don't race against each other under
    // cargo test's default multi-threaded runner.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn data_root_prefers_xdg_data_home_then_falls_back_to_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved_xdg = std::env::var("XDG_DATA_HOME").ok();
        let saved_home = std::env::var("HOME").ok();

        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/xdg/data");
        }
        assert_eq!(data_root(), PathBuf::from("/xdg/data/dashboard"));

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
            std::env::set_var("HOME", "/home/someone");
        }
        assert_eq!(data_root(), PathBuf::from("/home/someone/.local/share/dashboard"));

        unsafe {
            std::env::set_var("XDG_DATA_HOME", "");
        }
        assert_eq!(data_root(), PathBuf::from("/home/someone/.local/share/dashboard"));

        unsafe {
            match saved_xdg {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn home_accessors() {
        let home = Home { root: PathBuf::from("/data/dashboard/myproj") };
        assert_eq!(home.hub(), PathBuf::from("/data/dashboard/myproj/hub.git"));
        assert_eq!(home.work(), PathBuf::from("/data/dashboard/myproj/work"));
        assert_eq!(home.events(), PathBuf::from("/data/dashboard/myproj/events.jsonl"));
        assert_eq!(home.director_cursor(), PathBuf::from("/data/dashboard/myproj/director.cursor"));
        assert_eq!(home.player_state(), PathBuf::from("/data/dashboard/myproj/player-state.json"));
        assert_eq!(home.heartbeat(), PathBuf::from("/data/dashboard/myproj/player.heartbeat"));
    }

    #[test]
    fn subagent_paths_nest_under_the_directors_own_tree() {
        let director = PathBuf::from("/home/op/myproj");
        assert_eq!(subagents_root(&director), PathBuf::from("/home/op/myproj/.dashboard/subagents"));
        assert_eq!(subagent_clone(&director, "foo"), PathBuf::from("/home/op/myproj/.dashboard/subagents/foo"));
        assert_eq!(subagent_workspace(&director), PathBuf::from("/home/op/myproj/.dashboard/workspace"));
    }

    #[test]
    fn for_project_joins_data_root() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved = std::env::var("XDG_DATA_HOME").ok();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/xdg/data");
        }
        let home = Home::for_project("myproj");
        assert_eq!(home.root, PathBuf::from("/xdg/data/dashboard/myproj"));
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    #[test]
    fn resolve_home_uses_explicit_path() {
        let home = resolve_home(Some(PathBuf::from("/explicit/home")), Path::new(".")).unwrap();
        assert_eq!(home.root, PathBuf::from("/explicit/home"));
    }

    #[test]
    fn resolve_home_errors_without_config_or_explicit_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let git = Git::new(dir.path());
        git.run(&["init", "-q", "-b", "main"]).unwrap();
        let err = resolve_home(None, dir.path()).unwrap_err();
        assert!(err.contains("dashboard init"), "unexpected error: {err}");
    }

    #[test]
    fn resolve_home_reads_git_config() {
        let dir = tempfile::TempDir::new().unwrap();
        let git = Git::new(dir.path());
        git.run(&["init", "-q", "-b", "main"]).unwrap();
        git.config_set("dashboard.home", "/configured/home").unwrap();
        let home = resolve_home(None, dir.path()).unwrap();
        assert_eq!(home.root, PathBuf::from("/configured/home"));
    }
}
