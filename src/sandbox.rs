// The jail every check runs in (roadmap.toml's `[sandbox]`). With it on, a
// check's `sh -c` runs under bubblewrap: the host filesystem read-only, its
// worktree and the configured `writable` paths read-write, a private tmpfs
// on `/tmp`, and (unless `network = true`) only loopback. A test that writes
// outside its sandbox or reaches the network then just fails, like any other
// failing check. `preflight` proves the jail actually holds on this host
// before the player trusts it (run by `dashboard init` and the player).

use crate::config::SandboxConfig;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The binary the jail runs under.
const BWRAP: &str = "bwrap";

#[derive(Debug, Clone, PartialEq)]
pub struct Sandbox {
    /// `None`: no jail, checks run as plain `sh -c`.
    jail: Option<Jail>,
}

#[derive(Debug, Clone, PartialEq)]
struct Jail {
    network: bool,
    writable: Vec<PathBuf>,
    /// Bound read-only even though `/` already is: `/tmp` gets a tmpfs, so a
    /// hub living under `/tmp` would otherwise vanish, and with it the
    /// worktree's `.git`.
    readable: Vec<PathBuf>,
    /// Bound read-write like `writable`, but the runner's own (`with_writable`).
    scratch: Vec<PathBuf>,
}

impl Sandbox {
    /// No jail at all.
    pub fn off() -> Self {
        Self { jail: None }
    }

    /// The jail `cfg` describes. `~/` in `writable` is `$HOME`.
    pub fn from_config(cfg: &SandboxConfig) -> Result<Self, String> {
        cfg.validate()?;
        if !cfg.enabled {
            return Ok(Self::off());
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let writable = cfg
            .writable
            .iter()
            .map(|w| match w.strip_prefix("~/") {
                Some(rest) => home
                    .as_ref()
                    .map(|h| h.join(rest))
                    .ok_or_else(|| format!("[sandbox] writable {w:?}: $HOME is not set")),
                None => Ok(PathBuf::from(w)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            jail: Some(Jail {
                network: cfg.network,
                writable,
                readable: Vec::new(),
                scratch: Vec::new(),
            }),
        })
    }

    /// Also binds `path` read-only inside the jail (see `Jail::readable`).
    pub fn with_readable(mut self, path: &Path) -> Self {
        if let Some(j) = self.jail.as_mut() {
            j.readable.push(canonical(path));
        }
        self
    }

    /// Also binds `path` read-write inside the jail: the runner's own
    /// directories (its worktree, its views), so not part of the policy's
    /// fingerprint, which `state` recomputes from `[sandbox]` alone.
    pub fn with_writable(mut self, path: &Path) -> Self {
        if let Some(j) = self.jail.as_mut() {
            j.scratch.push(canonical(path));
        }
        self
    }

    pub fn enabled(&self) -> bool {
        self.jail.is_some()
    }

    /// Identifies the policy, so a check result cached under one policy is
    /// never reused under another (`CheckResult::isolation`). The version
    /// prefix also covers how impurity is detected (v1: worktree leaks; v2:
    /// also `walls_hit`), so results from an older detector re-run.
    pub fn fingerprint(&self) -> String {
        match &self.jail {
            None => "v2 jail=off".to_string(),
            Some(j) => {
                let rw: Vec<String> = j.writable.iter().map(|p| p.display().to_string()).collect();
                format!(
                    "v2 jail=bwrap net={} rw={}",
                    if j.network { "on" } else { "off" },
                    rw.join(":")
                )
            }
        }
    }

    /// The lines of a failing run's output showing it hit the jail: a write
    /// the read-only host filesystem refused or, with the network off, a
    /// connection or name lookup that had nowhere to go. A heuristic (the
    /// messages are libc's and curl's), but a failure like that never means
    /// "fails for the task's reason". Empty when the jail is off.
    pub fn walls_hit(&self, output: &str) -> Vec<String> {
        let Some(j) = &self.jail else {
            return Vec::new();
        };
        let mut walls = vec!["Read-only file system"];
        if !j.network {
            walls.extend([
                "Network is unreachable",
                "Temporary failure in name resolution",
                "Could not resolve host",
            ]);
        }
        let mut hits: Vec<String> = Vec::new();
        for line in output.lines() {
            let line = line.trim();
            if walls.iter().any(|w| line.contains(w)) && !hits.iter().any(|h| h == line) {
                hits.push(line.to_string());
            }
            if hits.len() == 3 {
                break;
            }
        }
        hits
    }

    /// One line for the operator, e.g. `bwrap, network off, writable: ~/.cargo`.
    pub fn describe(&self) -> String {
        match &self.jail {
            None => "off — checks run unjailed".to_string(),
            Some(j) => {
                let mut s = format!("bwrap, network {}", if j.network { "on" } else { "off" });
                if !j.writable.is_empty() {
                    let rw: Vec<String> =
                        j.writable.iter().map(|p| p.display().to_string()).collect();
                    s.push_str(&format!(", writable: {}", rw.join(", ")));
                }
                s
            }
        }
    }

    /// `sh -c <cmd>` in `cwd`, jailed if the sandbox is on.
    pub fn command(&self, cmd: &str, cwd: &Path) -> Command {
        let Some(j) = &self.jail else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(cmd).current_dir(cwd);
            return c;
        };
        let cwd = canonical(cwd);
        let mut c = Command::new(BWRAP);
        c.args(["--ro-bind", "/", "/"])
            .args(["--dev", "/dev"])
            .args(["--proc", "/proc"])
            .args(["--tmpfs", "/tmp"]);
        for r in &j.readable {
            c.arg("--ro-bind-try").arg(r).arg(r);
        }
        // A writable path that doesn't exist on this host (say, a cache the
        // toolchain hasn't created yet) is skipped rather than fatal.
        for w in j.writable.iter().chain(&j.scratch) {
            c.arg("--bind-try").arg(w).arg(w);
        }
        c.arg("--bind").arg(&cwd).arg(&cwd);
        if !j.network {
            c.arg("--unshare-net");
        }
        // A pid namespace plus --die-with-parent: killing bwrap (the
        // timeout's process-group kill) takes everything inside with it.
        c.args(["--unshare-pid", "--unshare-ipc", "--die-with-parent"])
            .args(["--setenv", "TMPDIR", "/tmp"])
            .arg("--chdir")
            .arg(&cwd)
            .args(["--", "sh", "-c", cmd])
            .current_dir(&cwd);
        c
    }

    /// Proves the jail holds on this host: bwrap runs, the working
    /// directory is writable, a directory outside it isn't, `/tmp` is
    /// private, and the network (when off) is loopback only. Scratch files
    /// go under `scratch`, which must exist; they're removed afterwards. A
    /// sandbox that's off passes trivially.
    pub fn preflight(&self, scratch: &Path) -> Result<(), String> {
        let Some(j) = &self.jail else {
            return Ok(());
        };
        // Unique even across threads preflighting at once (tests do).
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nonce = format!(
            "dashboard-preflight-{}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let root = scratch.join(&nonce);
        let inside = root.join("inside");
        let outside = root.join("outside");
        let result = (|| {
            std::fs::create_dir_all(&inside).map_err(|e| format!("{}: {e}", inside.display()))?;
            std::fs::create_dir_all(&outside).map_err(|e| format!("{}: {e}", outside.display()))?;
            // `scratch` may sit under a `writable` path (legitimately
            // writable from inside), which would make the "outside" probe
            // meaningless: leave those out of this one run.
            let outside_canon = canonical(&outside);
            let probe = Sandbox {
                jail: Some(Jail {
                    writable: j
                        .writable
                        .iter()
                        .filter(|w| !outside_canon.starts_with(canonical(w)))
                        .cloned()
                        .collect(),
                    ..j.clone()
                }),
            };
            probe.probe(j.network, &inside, &outside, &nonce)
        })();
        let _ = std::fs::remove_dir_all(&root);
        result.map_err(|e| format!("sandbox preflight failed: {e}"))
    }

    fn probe(
        &self,
        network: bool,
        inside: &Path,
        outside: &Path,
        nonce: &str,
    ) -> Result<(), String> {
        const HINT: &str = "install bubblewrap (Arch: pacman -S bubblewrap; Debian/Ubuntu: \
             apt install bubblewrap, and on Ubuntu also allow unprivileged user namespaces), or \
             turn the jail off with `[sandbox] enabled = false` in roadmap.toml";
        // Not `nonce` itself: with `scratch` under /tmp, bwrap creates
        // `/tmp/<nonce>/inside` in the jail's tmpfs as a mount point.
        let tmp_probe = format!("{nonce}.tmp-probe");
        let host_tmp = Path::new("/tmp").join(&tmp_probe);
        let script = format!(
            "echo ok > ./inside-ok || exit 10\n\
             echo leak > {out} 2>/dev/null\n\
             echo leak > /tmp/{tmp_probe} || exit 11\n\
             {net}\n\
             exit 0\n",
            out = sh_quote(&outside.join("leak").display().to_string()),
            // Interfaces of this process's own network namespace, minus
            // the two header lines and loopback.
            net = if network {
                String::new()
            } else {
                "if tail -n +3 /proc/net/dev | grep -v '^ *lo:' | grep -q .; then \
                 echo 'network interfaces besides lo:'; cat /proc/net/dev; exit 12; fi"
                    .to_string()
            },
        );
        let out = self
            .command(&script, inside)
            .output()
            .map_err(|e| format!("cannot run {BWRAP}: {e}\n{HINT}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let text = text.trim();
        match out.status.code() {
            Some(0) => {}
            Some(10) => {
                return Err(format!(
                    "the jail can't write its own working directory: {text}"
                ));
            }
            Some(11) => return Err(format!("the jail can't write its /tmp: {text}")),
            Some(12) => {
                return Err(format!(
                    "network = false, but the jail still has a network: {text}"
                ));
            }
            code => return Err(format!("{BWRAP} exited {code:?}: {text}\n{HINT}")),
        }
        if !inside.join("inside-ok").exists() {
            return Err("a write to the working directory didn't reach the host".to_string());
        }
        if outside.join("leak").exists() {
            return Err(format!(
                "a check could write {} outside its worktree",
                outside.display()
            ));
        }
        if host_tmp.exists() {
            let _ = std::fs::remove_file(&host_tmp);
            return Err("the jail's /tmp is the host's /tmp, not a private one".to_string());
        }
        Ok(())
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, network: bool, writable: &[&str]) -> SandboxConfig {
        SandboxConfig {
            enabled,
            network,
            writable: writable.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn fingerprint_tells_policies_apart() {
        let off = Sandbox::from_config(&cfg(false, false, &[])).unwrap();
        let jailed = Sandbox::from_config(&cfg(true, false, &[])).unwrap();
        let net = Sandbox::from_config(&cfg(true, true, &[])).unwrap();
        let rw = Sandbox::from_config(&cfg(true, false, &["/var/cache/x"])).unwrap();
        let all = [&off, &jailed, &net, &rw].map(|s| s.fingerprint());
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(off, Sandbox::off());
    }

    #[test]
    fn writable_must_be_absolute_or_home_relative() {
        assert!(Sandbox::from_config(&cfg(true, false, &["relative/dir"])).is_err());
        assert!(Sandbox::from_config(&cfg(true, false, &["~/.cargo", "/opt"])).is_ok());
    }

    #[test]
    fn walls_hit_matches_the_jails_refusals_only() {
        let out = "ok\nsh: 1: cannot create /home/x: Read-only file system\n\
                   curl: (6) Could not resolve host: example.com\nboom\n";
        let jailed = Sandbox::from_config(&cfg(true, false, &[])).unwrap();
        assert_eq!(jailed.walls_hit(out).len(), 2);
        let with_net = Sandbox::from_config(&cfg(true, true, &[])).unwrap();
        assert_eq!(
            with_net.walls_hit(out),
            vec!["sh: 1: cannot create /home/x: Read-only file system".to_string()]
        );
        assert!(Sandbox::off().walls_hit(out).is_empty());
        assert!(jailed.walls_hit("assertion failed\n").is_empty());
    }

    #[test]
    fn off_runs_plain_sh() {
        let c = Sandbox::off().command("true", Path::new("/"));
        assert_eq!(c.get_program(), "sh");
    }
}
