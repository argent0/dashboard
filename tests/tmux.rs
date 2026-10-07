// The real tmux backend's handling of a dead agent (`TmuxSessions`): its
// pane lingers (`remain-on-exit`) so `reap` can read the crash's output and
// exit status, it's dead to `state::tmux_alive` (what `dashboard status`
// asks) and to `Sessions::alive` while it lingers, and a relaunch replaces
// it. Needs a `tmux` binary; skipped without one.

use dashboard::player::{Sessions, TmuxSessions};
use dashboard::state;
use std::process::Command;
use std::time::{Duration, Instant};

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", &format!("={name}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Waits (up to 10 s) for `cond`.
fn eventually(mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_crashed_session_lingers_dead_until_reaped_with_its_output_and_status() {
    if !have_tmux() {
        eprintln!("skipped: no tmux");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let prompt = dir.path().join("prompt.md");
    std::fs::write(&prompt, "the prompt").unwrap();
    let project = format!("dashtest{}", std::process::id());
    let name = state::tmux_session_name(&project, "crash");
    let mut sessions = TmuxSessions::new(&project);

    // The agent prints something and exits 3 straight away (`$0` is the
    // prompt, passed as the command's first argument).
    let cmd = "sh -c 'echo \"crashed on: $0\"; exit 3'";
    sessions
        .launch("crash", dir.path(), cmd, &prompt)
        .expect("launch");
    assert!(
        eventually(|| !state::tmux_alive(&name)),
        "the command exited"
    );
    assert!(session_exists(&name), "its pane lingers");
    assert!(!sessions.alive("crash"), "lingering, it's still dead");

    // A relaunch (here, of a command that stays up) replaces it.
    sessions
        .launch("crash", dir.path(), "sleep 30; true", &prompt)
        .expect("relaunch over a lingering dead pane");
    assert!(eventually(|| sessions.alive("crash")), "relaunched");
    assert_eq!(sessions.reap("crash"), None, "a live session isn't reaped");
    assert!(sessions.alive("crash"));

    sessions.kill("crash");
    sessions
        .launch("crash", dir.path(), cmd, &prompt)
        .expect("launch");
    assert!(eventually(|| !sessions.alive("crash")));
    let remains = sessions.reap("crash").expect("a dead pane to reap");
    assert_eq!(remains.status, Some(3));
    assert_eq!(remains.signal, None);
    let screen = remains.screen.expect("its screen");
    assert!(screen.contains("crashed on: the prompt"), "{screen}");
    assert!(!screen.contains("Pane is dead"), "{screen}");
    assert!(!session_exists(&name), "reaped");
    assert_eq!(sessions.reap("crash"), None, "nothing left to reap");
}
