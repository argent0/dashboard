// The player for a git-hub-backed roadmap loop (spec.md): imports and
// validates director/worker commits, runs every check itself, launches
// workers in tmux, and merges accepted work — plus `dashboard status`
// (the default when no subcommand is given), which renders the roadmap
// purely from what's derived from the hub (src/state.rs, src/render.rs).
//
// This binary is a thin CLI over the `dashboard` library crate (src/lib.rs);
// integration tests in tests/ exercise the library directly.

use clap::{Parser, Subcommand};
use dashboard::git::Git;
use dashboard::paths::Home;
use dashboard::{config, events, git, init, paths, player, queue, render, state, ui};
use std::io::IsTerminal;
use std::path::PathBuf;

// --------------------------------------------------------------------------
// CLI
// --------------------------------------------------------------------------

/// The player for a git-hub-backed roadmap loop: imports and validates
/// director/worker commits, runs every check itself, launches workers in
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
enum ArchAction {
    /// Check the checkout in --root against its architecture (the Lean
    /// package `architecture/`): total modules and roots, each one's byte
    /// and AST budgets, file ownership, every API's budgets and bindings,
    /// and each module's tests in its jail view, in generator-first order.
    /// Prints every budget's headroom; exit 1 on any violation.
    Check {
        /// The checkout (default: the current directory).
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Regenerate every API's bindings from its IDL, and the vendored
    /// `architecture/Dashboard.lean` library. On a root without
    /// `architecture/`, write a starter package (no modules, one
    /// `covers = all` check). With --check, write nothing and exit 1 if
    /// anything is stale.
    Gen {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        check: bool,
    },
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
    /// Run the player loop: import and validate director/worker commits,
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
    /// Manually restart a task's worker tmux session after it died (spec.md
    /// §9's launch row leaves automatic relaunch deferred). Capped at
    /// `[limits] max_relaunches` since its last progress; past it the task
    /// escalates instead, and `--reset` resumes it once the operator has
    /// fixed the cause.
    Relaunch {
        /// The task's slug.
        slug: String,
        /// Restart the task's task agent (spec.md §17) instead of its worker.
        #[arg(long)]
        agent: bool,
        /// Clear a relaunch-limit escalation for this session (worker, or
        /// task agent with --agent), start its relaunch count over and
        /// relaunch it. Only for that cause: every other escalation is
        /// retire-only. The cleared escalation stays in the hub as evidence.
        #[arg(long)]
        reset: bool,
    },
    /// Paste a question from the director into a task agent's session
    /// (spec.md §17); its answer arrives as an `agent_report` event.
    Ask {
        /// The task's slug.
        slug: String,
        /// The question.
        #[arg(required = true, num_args = 1..)]
        question: Vec<String>,
    },
    /// Print the task's scope from a worker's or task agent's clone: its
    /// modules, the paths it may write, what else its working tree holds
    /// (read-only) and the checks that judge it.
    Scope,
    /// Run the task's run set from a worker's or task agent's worktree, the
    /// way the player will: snapshots the working tree and prints each
    /// check's result and output. Runs it here, in its own worktree, unless
    /// `[player] runs = "queued"`, where it waits its turn in the player's
    /// queue. Exits 1 if the run set fails. A failing run is never a strike.
    Test {
        /// Print the result file's path and return instead of waiting.
        #[arg(long)]
        no_wait: bool,
        /// Give up (exit 2) after this many seconds waiting.
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Architecture checks (architect-v3.md): budgets and module boundaries
    /// from the Lean package `architecture/`. Meant to run as an ordinary
    /// suite check.
    Arch {
        #[command(subcommand)]
        action: ArchAction,
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
        Some(Command::Watch { interval }) => {
            run_watch(args.home, args.no_color, args.width, interval)
        }
        Some(Command::Player { once, interval }) => run_player(args.home, once, interval),
        Some(Command::Relaunch { slug, agent, reset }) => {
            run_relaunch(args.home, &slug, agent, reset)
        }
        Some(Command::Ask { slug, question }) => run_ask(args.home, &slug, &question.join(" ")),
        Some(Command::Scope) => run_scope(args.home),
        Some(Command::Test { no_wait, timeout }) => run_test(args.home, no_wait, timeout),
        Some(Command::Arch { action: ArchAction::Check { root } }) => run_arch_check(&root),
        Some(Command::Arch { action: ArchAction::Gen { root, check } }) => run_arch_gen(&root, check),
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
    let hub_state = state::derive(&hub, &|slug| {
        state::tmux_alive(&state::tmux_session_name(&project, slug))
    })
    .map_err(|e| format!("{e} — run `dashboard init`"))?;
    let events = events::read_all(&home);
    Ok(render::render(
        &hub_state,
        &events,
        color,
        ui::detect_width(width),
    ))
}

// --------------------------------------------------------------------------
// dashboard watch
// --------------------------------------------------------------------------

/// Set from a signal handler: SIGTERM asks the loop to stop (Ctrl-C is a
/// key press in raw mode, not a signal).
static WATCH_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
fn install_watch_signals() {
    const SIGTERM: i32 = 15;
    unsafe extern "C" {
        fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    }
    extern "C" fn on_signal(_: i32) {
        WATCH_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    unsafe {
        signal(SIGTERM, on_signal);
    }
}

#[cfg(not(unix))]
fn install_watch_signals() {}

/// Puts the terminal back (raw mode off, auto-wrap, cursor, main screen) —
/// on drop and, through the panic hook, before a panic message prints.
fn restore_terminal() {
    use crossterm::{cursor, execute, terminal};
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        terminal::EnableLineWrap,
        cursor::Show,
        terminal::LeaveAlternateScreen
    );
}

struct WatchTerminal;

impl WatchTerminal {
    fn enter() -> std::io::Result<Self> {
        use crossterm::{cursor, execute, terminal};
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            hook(info);
        }));
        // Alternate screen, hide cursor, disable auto-wrap (so an overlong
        // line can't push the frame down a row), raw mode for key presses.
        execute!(
            std::io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide,
            terminal::DisableLineWrap
        )?;
        terminal::enable_raw_mode()?;
        Ok(WatchTerminal)
    }
}

impl Drop for WatchTerminal {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// What a key press asks `watch` to do.
fn watch_key(key: crossterm::event::KeyEvent) -> Option<Result<ui::Scroll, ()>> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    use ui::Scroll;
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(Ok(match key.code {
        KeyCode::Char('c') if ctrl => return Some(Err(())),
        KeyCode::Char('q') | KeyCode::Esc => return Some(Err(())),
        KeyCode::Up | KeyCode::Char('k') => Scroll::Lines(-1),
        KeyCode::Down | KeyCode::Char('j') => Scroll::Lines(1),
        KeyCode::PageUp | KeyCode::Char('b') => Scroll::Pages(-1),
        KeyCode::Char('u') if ctrl => Scroll::Pages(-1),
        KeyCode::PageDown | KeyCode::Char(' ') => Scroll::Pages(1),
        KeyCode::Char('d') if ctrl => Scroll::Pages(1),
        KeyCode::Home | KeyCode::Char('g') => Scroll::Top,
        KeyCode::End | KeyCode::Char('G') => Scroll::Bottom,
        _ => return None,
    }))
}

/// Like `while true; do dashboard; sleep N; clear; done`, minus the flicker
/// and with scrolling: draws on the alternate screen, and each frame homes
/// the cursor and overwrites the previous one in a single write (clearing
/// only each line's tail and whatever is left below), instead of blanking
/// the screen first. A frame taller than the terminal scrolls by keyboard
/// (`ui::Viewport`); the wheel works too wherever the terminal turns it into
/// arrow keys on the alternate screen (its "alternate scroll" mode), since
/// the mouse is never captured and text selection stays native.
fn run_watch(home: Option<PathBuf>, no_color: bool, width: Option<u32>, interval: u64) {
    use crossterm::event::{self, Event};
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    if !std::io::stdout().is_terminal() {
        eprintln!("error: `dashboard watch` needs a terminal — use `dashboard status` instead");
        std::process::exit(1);
    }
    install_watch_signals();
    let _terminal = match WatchTerminal::enter() {
        Ok(t) => t,
        Err(e) => {
            restore_terminal();
            eprintln!("error: cannot set up the terminal: {e}");
            std::process::exit(1);
        }
    };

    let mut out = std::io::stdout().lock();
    let interval = Duration::from_secs(interval.max(1));
    // Checked between polls, so a SIGTERM is noticed within this.
    let tick = Duration::from_millis(200);
    let mut viewport = ui::Viewport::default();
    let mut text = String::new();
    let mut next_render = Instant::now();
    let mut dirty = true;
    while !WATCH_STOP.load(Ordering::SeqCst) {
        let rows = crossterm::terminal::size().map_or(24, |(_, r)| r as usize).max(1);
        if Instant::now() >= next_render {
            text = render_status(home.clone(), !no_color, width)
                .unwrap_or_else(|e| format!("error: {e}"));
            next_render = Instant::now() + interval;
            dirty = true;
        }
        let lines: Vec<&str> = text.lines().collect();
        if dirty {
            let (body, footer) = viewport.frame(&lines, rows, !no_color);
            let mut frame = String::from("\x1b[H");
            for (i, line) in body.iter().chain(footer.as_deref().iter()).enumerate() {
                if i > 0 {
                    frame.push_str("\r\n");
                }
                frame.push_str(line);
                frame.push_str("\x1b[0m\x1b[K");
            }
            frame.push_str("\x1b[J");
            let _ = out.write_all(frame.as_bytes());
            let _ = out.flush();
            dirty = false;
        }

        let wait = next_render.saturating_duration_since(Instant::now()).min(tick);
        if !event::poll(wait).unwrap_or(false) {
            continue;
        }
        match event::read() {
            Ok(Event::Key(key)) => match watch_key(key) {
                Some(Ok(by)) => {
                    viewport.scroll(by, lines.len(), rows);
                    dirty = true;
                }
                Some(Err(())) => break,
                None => {}
            },
            // A new width re-renders (the layout depends on it), not just redraws.
            Ok(Event::Resize(..)) => next_render = Instant::now(),
            Ok(_) => {}
            Err(_) => break,
        }
    }
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
    // Preflight the jail before anything runs: a player that can't sandbox
    // its checks refuses to start rather than run them unjailed.
    match player.check_sandbox() {
        Ok(desc) if !once => eprintln!("sandbox: {desc}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
    // Then main's suite under that jail: a cache hit unless main, the
    // policy or the player changed, in which case it's one full run. Impure
    // checks there don't stop the player (only a task can fix them); work
    // failing only on them is held instead of struck.
    if !once {
        eprintln!("checking main's suite under the sandbox…");
    }
    match player.verify_main() {
        Ok(impure) if impure.is_empty() => {
            if !once {
                eprintln!("main's suite: clean");
            }
        }
        Ok(impure) => {
            eprintln!(
                "warning: main has impure checks; work failing only on them is held until a task fixes them:"
            );
            for why in impure.values() {
                eprintln!("  {why}");
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }

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

fn run_relaunch(home: Option<PathBuf>, slug: &str, agent: bool, reset: bool) {
    let mut player = open_player(home);
    let result = if reset {
        player.reset_relaunch(slug, agent)
    } else if agent {
        player.relaunch_agent(slug)
    } else {
        player.relaunch(slug)
    };
    let cleared = if reset {
        format!("cleared task/{slug}'s relaunch-limit escalation and ")
    } else {
        String::new()
    };
    match result {
        Ok(()) if agent => println!("{cleared}relaunched task/{slug}'s task agent"),
        Ok(()) => println!("{cleared}relaunched task/{slug}"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

// --------------------------------------------------------------------------
// dashboard ask
// --------------------------------------------------------------------------

fn run_ask(home: Option<PathBuf>, slug: &str, question: &str) {
    let mut player = open_player(home);
    match player.ask(slug, question) {
        Ok(()) => println!(
            "asked task/{slug}'s task agent; its answer arrives as an agent_report event (dashboard wait)"
        ),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

/// `dashboard test`: see `queue.rs`.
/// What `dashboard scope` and `dashboard test` start from: the home, the
/// worktree they run in, its task's slug and whether it's a task agent's.
struct TaskClone {
    home: Home,
    worktree: git::Git,
    slug: String,
    agent: bool,
}

fn fail(msg: String) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn task_clone(home: Option<PathBuf>) -> TaskClone {
    let cwd = std::env::current_dir().unwrap_or_else(|e| fail(format!("cannot read current directory: {e}")));
    let home = paths::resolve_home(home, &cwd).unwrap_or_else(|e| fail(e));
    let worktree = git::Git::new(&cwd);
    let branch = worktree
        .run(&["symbolic-ref", "--short", "HEAD"])
        .unwrap_or_else(|e| fail(e.to_string()));
    let slug = queue::slug_of_branch(&branch)
        .unwrap_or_else(|| fail(format!("branch {branch} is not a task/<slug> or agent/<slug> branch")))
        .to_string();
    TaskClone { home, worktree, slug, agent: branch.starts_with("agent/"), }
}

/// The roadmap on the hub's main.
fn hub_roadmap(hub: &git::Git) -> config::RoadmapConfig {
    hub.rev_parse("refs/heads/main")
        .and_then(|tip| hub.show_file(&tip, "roadmap.toml"))
        .and_then(|text| config::roadmap_config_from_str(&text).ok())
        .unwrap_or_else(|| fail("no valid roadmap.toml on the hub's main".into()))
}

/// The CLI's runner (`Runner::for_cli`) under the roadmap's jail and budget.
fn cli_runner(home: &Home, roadmap: &config::RoadmapConfig) -> dashboard::checks::Runner {
    let mut runner = dashboard::checks::Runner::for_cli(home);
    let sandbox = dashboard::sandbox::Sandbox::from_config(&roadmap.sandbox).unwrap_or_else(|e| fail(e));
    runner.set_sandbox(sandbox);
    runner.set_budget(roadmap.budget);
    runner
}

fn run_scope(home: Option<PathBuf>) {
    let c = task_clone(home);
    let hub = git::Git::new(c.home.hub());
    let runner = cli_runner(&c.home, &hub_roadmap(&hub));
    let slug = c.slug.as_str();
    let tip = hub
        .rev_parse(&format!("refs/heads/task/{slug}"))
        .unwrap_or_else(|| fail(format!("the hub has no task/{slug}")));
    let ex = Some(runner.exporter());
    let workspace = dashboard::records::workspace_at(&hub, slug, &tip, ex).unwrap_or_else(|e| fail(e));
    let scope = &workspace.scope;
    let suite = dashboard::records::suite_at(&hub, &tip, ex).unwrap_or_else(|e| fail(e));
    println!("task/{slug}  modules: {}", scope.modules.join(", "));
    println!("writable:");
    if c.agent {
        for p in dashboard::records::agent_scope_at(&hub, slug, &tip, ex) {
            println!("  {p}");
        }
    } else {
        for (p, why) in scope.paths.iter().zip(&scope.why) {
            println!("  {p:<30} ({why})");
        }
    }
    if c.agent {
        println!("working tree: full");
    } else {
        println!("working tree:");
        for (p, why) in &workspace.readonly {
            println!("  {p:<30} ({why})");
        }
    }
    println!("run set ({} checks):", scope.run_set.len());
    for name in &scope.run_set {
        let Some(check) = suite.check(name) else { continue };
        let mut line = format!("  {name:<20}");
        if check.source.as_deref() == Some(config::ARCH_LEAN_MANIFEST) {
            line.push_str(" module test ");
        }
        if check.all {
            line.push_str(" all");
        } else {
            line.push_str(&format!(" covers {}", check.covers.join(", ")));
        }
        if !check.files.is_empty() {
            line.push_str(&format!("  files {}", check.files.join(", ")));
        }
        println!("{}", line.trim_end());
    }
}

fn run_test(home: Option<PathBuf>, no_wait: bool, timeout: Option<u64>) {
    let c = task_clone(home);
    let hub = git::Git::new(c.home.hub());
    let roadmap = hub_roadmap(&hub);
    let commit = queue::snapshot(&c.worktree).unwrap_or_else(|e| fail(e));
    if roadmap.player.runs != config::Runs::Queued {
        // Here and now, in the CLI's own worktree: fetch the snapshot into
        // the hub (the cache is its notes), run, drop the ref.
        let slug = c.slug.as_str();
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let local = format!("refs/dashboard/test/{millis:016}-{slug}");
        let repo = c
            .worktree
            .run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .unwrap_or_else(|e| fail(e.to_string()));
        c.worktree.update_ref(&local, &commit).unwrap_or_else(|e| fail(e.to_string()));
        let fetched = hub.run(&["fetch", "--quiet", &repo, &format!("+{local}:{local}")]);
        let _ = c.worktree.delete_ref(&local);
        fetched.unwrap_or_else(|e| fail(e.to_string()));
        let runner = cli_runner(&c.home, &roadmap);
        let tip = hub.rev_parse(&format!("refs/heads/task/{slug}"));
        let modules = tip
            .as_deref()
            .and_then(|t| dashboard::records::task_config_at(&hub, t, slug))
            .map(|cfg| cfg.modules)
            .unwrap_or_default();
        let suite = match runner.run_set_for(&commit, &modules, tip.as_deref()) {
            Ok(set) => runner.run_set(&commit, &set),
            Err(e) => dashboard::checks::SuiteResult::failing(e),
        };
        let _ = hub.delete_ref(&local);
        print!("{}", queue::render(&suite));
        std::process::exit(if suite.pass { 0 } else { 1 });
    }
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let req = queue::enqueue(&c.home, &c.worktree, &c.slug, &commit, millis).unwrap_or_else(|e| fail(e));
    let result = queue::result_path(&c.home, &req.id);
    if no_wait {
        println!("{}", result.display());
        return;
    }
    eprintln!("queued {}; waiting for the player", req.id);
    match queue::wait(&c.home, &req.id, timeout.map(std::time::Duration::from_secs)) {
        Some((pass, report)) => {
            let _ = c.worktree.delete_ref(&req.ref_name());
            print!("{report}");
            std::process::exit(if pass { 0 } else { 1 });
        }
        None => {
            eprintln!("still queued; the result will appear at {}", result.display());
            std::process::exit(2);
        }
    }
}

/// Resolves the home and opens a tmux-backed player, exiting on error.
fn open_player(home: Option<PathBuf>) -> player::Player<player::TmuxSessions> {
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
    match player::Player::open(home, sessions) {
        Ok(p) => p,
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
    let deadline =
        timeout.map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
    let mut warned_stale = false;

    loop {
        let new: Vec<_> = events::read_all(&home)
            .into_iter()
            .filter(|e| e.seq > cursor)
            .collect();
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
                eprintln!(
                    "no events within {} s",
                    timeout.expect("deadline implies timeout")
                );
                std::process::exit(2);
            }
        }

        std::thread::sleep(WAIT_POLL_INTERVAL);
    }
}

fn run_arch_check(root: &std::path::Path) {
    let loaded = load_arch(root);
    let cfg = &loaded.cfg;
    // The suite already jails this check; views run plain inside it.
    let scratch = std::env::temp_dir();
    let rep = dashboard::arch::check(root, &loaded, &dashboard::sandbox::Sandbox::off(), &scratch);
    println!(
        "modules {}{}",
        cfg.modules.len(),
        if cfg.max_modules > 0 { format!("/{}", cfg.max_modules) } else { String::new() }
    );
    if !cfg.roots.is_empty() || cfg.max_roots > 0 {
        println!("roots {}{}", cfg.roots.len(), if cfg.max_roots > 0 { format!("/{}", cfg.max_roots) } else { String::new() });
    }
    for h in &rep.headroom {
        println!("{h}");
    }
    if !rep.order.is_empty() {
        println!("test order: {}", rep.order.join(", "));
    }
    for p in &rep.problems {
        eprintln!("FAIL {p}");
    }
    if !rep.pass() {
        std::process::exit(1);
    }
    println!("architecture ok");
}

fn load_arch(root: &std::path::Path) -> dashboard::arch::Loaded {
    match dashboard::arch::load(root) {
        Ok(c) => c,
        Err(e) => {
            for l in e.lines() {
                eprintln!("dashboard: {l}");
            }
            std::process::exit(1);
        }
    }
}

fn run_arch_gen(root: &std::path::Path, check: bool) {
    // The vendored library first: a Lean package can't be loaded through a
    // stale copy of it.
    let mut vendored = Vec::new();
    if !check && !root.join(dashboard::config::ARCH_DIR).exists() {
        match dashboard::lean::write_starter(root) {
            Ok(w) => vendored = w,
            Err(e) => {
                eprintln!("dashboard: {e}");
                std::process::exit(1);
            }
        }
    } else if !check && dashboard::lean::present(root) {
        match dashboard::lean::write_vendored(root) {
            Ok(w) => vendored = w,
            Err(e) => {
                eprintln!("dashboard: {e}");
                std::process::exit(1);
            }
        }
    }
    let cfg = load_arch(root);
    for f in &vendored {
        println!("wrote {f}");
    }
    let (changed, errs) = dashboard::arch::sync_bindings(root, &cfg, !check);
    for f in &changed {
        println!("{} {f}", if check { "stale" } else { "wrote" });
    }
    for e in &errs {
        eprintln!("FAIL {e}");
    }
    if !errs.is_empty() || (check && !changed.is_empty()) {
        std::process::exit(1);
    }
}
