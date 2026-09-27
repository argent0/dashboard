// The player for a git-hub-backed roadmap loop (spec.md): imports and
// validates director/subagent commits, runs every check itself, launches
// subagents in tmux, and merges accepted work — plus `dashboard status`
// (the default when no subcommand is given), which renders the roadmap
// purely from what's derived from the hub (src/state.rs, src/render.rs).
//
// This binary is a thin CLI over the `dashboard` library crate (src/lib.rs);
// integration tests in tests/ exercise the library directly.

use clap::{Parser, Subcommand};
use dashboard::git::Git;
use dashboard::paths::Home;
use dashboard::{events, init, paths, player, render, state, ui};
use std::io::IsTerminal;
use std::path::PathBuf;

// --------------------------------------------------------------------------
// CLI
// --------------------------------------------------------------------------

/// The player for a git-hub-backed roadmap loop: imports and validates
/// director/subagent commits, runs every check itself, launches subagents in
/// tmux, and merges accepted work. With no subcommand, renders the roadmap
/// derived from the hub (see spec.md).
#[derive(Parser)]
#[command(name = "dashboard")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// dashboard home root (<root>/hub.git, etc. — the full home, not a data
    /// root). Later commands resolve this via `dashboard.home` in the
    /// current repo's git config when not given.
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    /// Disable ANSI colors.
    #[arg(long, global = true)]
    no_color: bool,

    /// Override detected terminal width.
    #[arg(long, global = true)]
    width: Option<u32>,
}

#[derive(Subcommand)]
enum Command {
    /// Create this project's hub from the director clone containing (or
    /// above) the current directory.
    Init {
        /// Project name (default: the director clone toplevel's basename).
        #[arg(long)]
        name: Option<String>,
    },
    /// Render the roadmap derived from the hub (spec.md §12). The default
    /// when no subcommand is given.
    Status,
    /// Run the player loop: import and validate director/subagent commits,
    /// run checks, and merge accepted/unblocked work (spec.md §5, §9, §11).
    Player {
        /// Run a single tick, print its report, and exit.
        #[arg(long)]
        once: bool,
        /// Seconds to sleep between ticks when looping.
        #[arg(long, default_value_t = 2)]
        interval: u64,
    },
    /// Re-render the status in place every few seconds, without flicker,
    /// until interrupted (Ctrl-C).
    Watch {
        /// Seconds between refreshes.
        #[arg(long, short = 'n', default_value_t = 2)]
        interval: u64,
    },
    /// Manually restart a task's subagent tmux session after it died (spec.md
    /// §9's launch row leaves automatic relaunch deferred).
    Relaunch {
        /// The task's slug.
        slug: String,
    },
    /// The director's listener: blocks until the player appends a new event,
    /// prints every event past the cursor, advances it, and exits (step 7 —
    /// the director runs this as a background command instead of polling).
    Wait {
        /// Give up (exit 2) after this many seconds with no new events.
        #[arg(long)]
        timeout: Option<u64>,
        /// Print the whole event history and exit, without blocking or
        /// moving the cursor.
        #[arg(long)]
        all: bool,
    },
}

fn main() {
    let args = Args::parse();

    match args.command {
        Some(Command::Init { name }) => run_init(name, args.home),
        Some(Command::Status) | None => run_status(args.home, args.no_color, args.width),
        Some(Command::Watch { interval }) => run_watch(args.home, args.no_color, args.width, interval),
        Some(Command::Player { once, interval }) => run_player(args.home, once, interval),
        Some(Command::Relaunch { slug }) => run_relaunch(args.home, &slug),
        Some(Command::Wait { timeout, all }) => run_wait(args.home, timeout, all),
    }
}

/// Reads `dashboard.project` straight from the hub's git config, exiting
/// with a message pointing at `dashboard init` when it's missing (a hub
/// that wasn't created by `init` at all, or was hand-built by something
/// else).
fn hub_project(hub: &Git) -> String {
    hub.config_get("dashboard.project").unwrap_or_else(|| {
        eprintln!("error: hub is missing dashboard.project — run `dashboard init`");
        std::process::exit(1);
    })
}

/// Builds the real tmux-backed `Sessions` for a resolved home — needed a step
/// earlier than `player::Player::open` (which reads the same config key
/// internally), since `TmuxSessions` has to exist before the generic
/// `Player<S>` can be opened.
fn build_sessions(home: &Home) -> player::TmuxSessions {
    let hub = Git::new(home.hub());
    player::TmuxSessions::new(hub_project(&hub))
}

// --------------------------------------------------------------------------
// dashboard init
// --------------------------------------------------------------------------

fn run_init(name: Option<String>, home: Option<PathBuf>) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            std::process::exit(1);
        }
    };
    match init::init(&cwd, name.as_deref(), home) {
        Ok(report) => println!("{}", report.summary()),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

// --------------------------------------------------------------------------
// dashboard status (default subcommand)
// --------------------------------------------------------------------------

fn run_status(home: Option<PathBuf>, no_color: bool, width: Option<u32>) {
    let color = !no_color && std::io::stdout().is_terminal();
    match render_status(home, color, width) {
        Ok(text) => println!("{text}"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

/// Resolves the home, derives the hub state, and renders it — everything
/// `status` prints, shared with `watch`'s per-frame redraw.
fn render_status(home: Option<PathBuf>, color: bool, width: Option<u32>) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot read current directory: {e}"))?;
    let home = paths::resolve_home(home, &cwd).map_err(|e| e.to_string())?;
    let hub = Git::new(home.hub());
    let project = hub
        .config_get("dashboard.project")
        .ok_or("hub is missing dashboard.project — run `dashboard init`")?;
    let hub_state = state::derive(&hub, &|slug| state::tmux_alive(&state::tmux_session_name(&project, slug)))
        .map_err(|e| format!("{e} — run `dashboard init`"))?;
    let events = events::read_all(&home);
    Ok(render::render(&hub_state, &events, color, ui::detect_width(width)))
}

// --------------------------------------------------------------------------
// dashboard watch
// --------------------------------------------------------------------------

/// Set from a signal handler: SIGINT/SIGTERM ask the loop to stop, SIGWINCH
/// asks for an immediate redraw at the new size.
static WATCH_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static WATCH_RESIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
fn install_watch_signals() {
    use std::sync::atomic::Ordering;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    const SIGWINCH: i32 = 28;
    unsafe extern "C" {
        fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    }
    extern "C" fn on_signal(signum: i32) {
        if signum == SIGWINCH {
            WATCH_RESIZED.store(true, Ordering::SeqCst);
        } else {
            WATCH_STOP.store(true, Ordering::SeqCst);
        }
    }
    unsafe {
        signal(SIGINT, on_signal);
        signal(SIGTERM, on_signal);
        signal(SIGWINCH, on_signal);
    }
}

#[cfg(not(unix))]
fn install_watch_signals() {}

/// Like `while true; do dashboard; sleep N; clear; done`, minus the flicker:
/// draws on the alternate screen, and each frame homes the cursor and
/// overwrites the previous one in a single write (clearing only each line's
/// tail and whatever is left below), instead of blanking the screen first.
fn run_watch(home: Option<PathBuf>, no_color: bool, width: Option<u32>, interval: u64) {
    use std::io::Write;
    use std::sync::atomic::Ordering;

    if !std::io::stdout().is_terminal() {
        eprintln!("error: `dashboard watch` needs a terminal — use `dashboard status` instead");
        std::process::exit(1);
    }
    install_watch_signals();

    let mut out = std::io::stdout().lock();
    // Alternate screen, hide cursor, disable auto-wrap (so an overlong line
    // can't push the frame down a row).
    let _ = write!(out, "\x1b[?1049h\x1b[?25l\x1b[?7l");
    let _ = out.flush();

    let tick = std::time::Duration::from_millis(100);
    let interval = std::time::Duration::from_secs(interval.max(1));
    while !WATCH_STOP.load(Ordering::SeqCst) {
        let text = render_status(home.clone(), !no_color, width).unwrap_or_else(|e| format!("error: {e}"));
        let mut lines: Vec<&str> = text.lines().collect();
        if let Some(rows) = ui::detect_height() {
            lines.truncate(rows);
        }
        let mut frame = String::from("\x1b[H");
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                frame.push_str("\r\n");
            }
            frame.push_str(line);
            frame.push_str("\x1b[0m\x1b[K");
        }
        frame.push_str("\x1b[J");
        let _ = out.write_all(frame.as_bytes());
        let _ = out.flush();

        let next = std::time::Instant::now() + interval;
        while std::time::Instant::now() < next
            && !WATCH_STOP.load(Ordering::SeqCst)
            && !WATCH_RESIZED.swap(false, Ordering::SeqCst)
        {
            std::thread::sleep(tick);
        }
    }

    // Restore auto-wrap, cursor, and the main screen.
    let _ = write!(out, "\x1b[?7h\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
}

// --------------------------------------------------------------------------
// dashboard player
// --------------------------------------------------------------------------

fn run_player(home: Option<PathBuf>, once: bool, interval: u64) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            std::process::exit(1);
        }
    };
    let home = match paths::resolve_home(home, &cwd) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let sessions = build_sessions(&home);
    let mut player = match player::Player::open(home, sessions) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    if once {
        match player.tick() {
            Ok(report) => println!("{report}"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    loop {
        match player.tick() {
            Ok(report) => {
                if !report.is_empty() {
                    println!("{report}");
                }
            }
            Err(e) => eprintln!("error: {e}"),
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
}

// --------------------------------------------------------------------------
// dashboard relaunch
// --------------------------------------------------------------------------

fn run_relaunch(home: Option<PathBuf>, slug: &str) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            std::process::exit(1);
        }
    };
    let home = match paths::resolve_home(home, &cwd) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let sessions = build_sessions(&home);
    let mut player = match player::Player::open(home, sessions) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    match player.relaunch(slug) {
        Ok(()) => println!("relaunched task/{slug}"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

// --------------------------------------------------------------------------
// dashboard wait
// --------------------------------------------------------------------------

/// How often to re-check the event log while blocking. The point isn't a
/// tight deadline — it's that the director's *own* session never polls; a
/// background `dashboard wait` doing this internally is fine (step 7).
const WAIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

fn run_wait(home: Option<PathBuf>, timeout: Option<u64>, all: bool) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            std::process::exit(1);
        }
    };
    let home = match paths::resolve_home(home, &cwd) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    if all {
        for event in events::read_all(&home) {
            println!("{}\n", event.human());
        }
        return;
    }

    let cursor = events::read_cursor(&home);
    let deadline = timeout.map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
    let mut warned_stale = false;

    loop {
        let new: Vec<_> = events::read_all(&home).into_iter().filter(|e| e.seq > cursor).collect();
        if !new.is_empty() {
            for event in &new {
                println!("{}\n", event.human());
            }
            let last_seq = new.last().expect("checked non-empty above").seq;
            if let Err(e) = events::write_cursor(&home, last_seq) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            return;
        }

        if !warned_stale {
            if events::heartbeat_stale(&home) {
                eprintln!("player doesn't seem to be running — start `dashboard player`");
            }
            warned_stale = true;
        }

        if let Some(deadline) = deadline {
            if std::time::Instant::now() >= deadline {
                eprintln!("no events within {} s", timeout.expect("deadline implies timeout"));
                std::process::exit(2);
            }
        }

        std::thread::sleep(WAIT_POLL_INTERVAL);
    }
}
