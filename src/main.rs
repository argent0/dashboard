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
use dashboard::marker::{Args as MarkerArgs, Marker};
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
    /// The call graph of the modules' frames (architecture/Frames/): one
    /// `module.fn -> callee` line per declared call, then each API fn with
    /// the frame fns calling it, and the fns of an API a framed module
    /// consumes that no frame fn calls.
    Calls {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Only this module's frame.
        module: Option<String>,
    },
    /// In the director clone, on main at the hub's tip: add module <name>
    /// to architecture/Architecture.lean from the project's scaffold
    /// (architecture/scaffold/: module.lean, optional api.lean, files/ with
    /// {name} and {crate} substituted), each --api with a stub IDL in
    /// architecture/Apis/, write the module's files and the bindings, and
    /// run the module's build_cmd so lockfiles follow. Leaves it all
    /// uncommitted: fill the IDLs, `dashboard arch gen`, then commit it as
    /// one main commit (the player admits a new module's scaffold, its
    /// shared files and gen/ bindings there).
    AddModule {
        name: String,
        /// `[<name>=]<generator>:<consumer>`, named `<generator>-<consumer>`
        /// by default; repeat for each API. The new module may be either end.
        #[arg(long = "api")]
        apis: Vec<dashboard::scaffold::ApiSpec>,
    },
    /// Like add-module, for APIs between modules and roots already in the
    /// manifest: entries, stub IDLs and bindings, uncommitted.
    AddApi {
        /// `[<name>=]<generator>:<consumer>`, one or more.
        #[arg(required = true)]
        apis: Vec<dashboard::scaffold::ApiSpec>,
    },
}

#[derive(Subcommand)]
enum DraftKind {
    /// A test-speedup naming, in `[[slow]]`, every check slow on main's
    /// recorded over-budget suite run (every killed one, then each at 10%
    /// of suite_fail or more). Refuses while main is within its [budget].
    Speedup {
        /// The new task's slug (default: test-speedup, or test-speedup-<n>).
        slug: Option<String>,
        /// The phase (default: the first phase of roadmap.toml with open
        /// tasks, else the last).
        #[arg(long)]
        phase: Option<String>,
    },
    /// A flaky-test-fix for suite check <check>, from its passes on retry in
    /// the flake ledger: `[[replace]]` from the tests its failing output
    /// names (the inventory of `list_cmd`, run on main), `fix_paths` from
    /// the files naming them, `[[evidence]]` and the stability `[check]`
    /// from its `cmd`. Refuses a check with no recorded flaky pass.
    Flaky {
        /// The flaky suite check.
        check: String,
        /// The new task's slug (default: fix-flaky-<check>).
        slug: Option<String>,
        /// The phase (default: as for speedup).
        #[arg(long)]
        phase: Option<String>,
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
    /// From a task agent's worktree: report to the director on the step the
    /// player waits on (spec.md §17). The player fills in slug, step and
    /// commit. A review closes with accept (with --suite) or unsure, a
    /// question with answer or unsure, a post or contest with unsure; a
    /// step closed by a commit (post, send-back, contest reply) takes its verdict from the player's import, so leave it out then.
    /// Exits 1, naming the verdicts allowed, on any other.
    Report {
        /// accept | unsure | answer, or posted | sent-back | replied,
        /// implied after an import.
        verdict: Option<String>,
        /// Two or three sentences: why this verdict.
        #[arg(long)]
        why: String,
        /// What the task's check asserts and why it fails / now passes.
        #[arg(long)]
        check: Option<String>,
        /// With accept: each check this task added, by name, keep or drop,
        /// and why.
        #[arg(long)]
        suite: Option<String>,
        /// Anything cross-task the director should know.
        #[arg(long)]
        concerns: Option<String>,
    },
    /// From the director clone: trim a tested task before its accept
    /// (spec.md §6.5). Fetches the hub and commits on task/<slug>, on top
    /// of the hub's tip, the removal of the named checks from
    /// architecture/Tests/<slug>.lean and <slug>.worker.lean (a file left
    /// with none goes whole) and the deletion of the test files only they
    /// own; a file another check also lists, under test_paths or already
    /// on main stays, named with its reason. The player imports it on its
    /// next tick and runs the run set.
    Trim {
        /// The task's slug.
        slug: String,
        /// The checks to drop, each declared in the task's own Lean files.
        #[arg(required = true, num_args = 1..)]
        checks: Vec<String>,
    },
    /// From the director clone: draft the plan of a test-speedup or a
    /// flaky-test-fix (spec.md §15, §15.1) from what the player recorded.
    /// Fetches the hub and creates task/<slug> on the hub's main with one
    /// plan commit adding tasks/<slug>/task.toml; the player imports it on
    /// its next tick. Prints what it derived and any field left as a
    /// `TODO(director)` placeholder: review, replan if needed, then stage.
    Draft {
        #[command(subcommand)]
        kind: DraftKind,
    },
    /// From the director clone: release a ready task (spec.md §17): its
    /// task agent launches. Fetches the hub and commits `stage: <slug>` on
    /// task/<slug>, on top of the hub's tip, once the player would take it.
    Stage {
        slug: String,
        /// Why now, as the marker's body.
        #[arg(long)]
        why: Option<String>,
    },
    /// From the director clone: put a staged task back to ready while no
    /// task agent exists (`unstage: <slug>`).
    Unstage {
        slug: String,
        #[arg(long)]
        why: Option<String>,
    },
    /// From the director clone: accept a tested task (spec.md §9, §11);
    /// the player lands it. Commits `accept: <slug>` with the why as its
    /// body; by default the task agent's latest accept report's why.
    Accept {
        slug: String,
        #[arg(long)]
        why: Option<String>,
    },
    /// From the director clone: retire a task (spec.md §9). Commits
    /// `retire: <slug>` with the post-mortem as its body, the only record
    /// of why it died.
    Retire {
        slug: String,
        /// The post-mortem.
        #[arg(long)]
        why: String,
    },
    /// From the director clone: conclude a tested spike (spec.md §14).
    /// Commits `conclude: <answer>`, the answer the player verified (a
    /// given one must be it, and one the question allows).
    Conclude {
        slug: String,
        answer: Option<String>,
        /// Why the answer stands, as agreed with the operator.
        #[arg(long)]
        why: String,
    },
    /// From the director clone, on main at the hub's tip: commit the
    /// budget edits in the working tree as a raise (architect-v3.md,
    /// "Budget increase path"): `raise(<slug>): <reason>`, judged first as
    /// the player will (only budget fields, within the step cap).
    Raise {
        /// The task that hit the budget.
        slug: Option<String>,
        /// The measured overage, discussed with the operator.
        #[arg(long)]
        reason: String,
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
        /// Run only these checks, each in the run set; none runs it whole.
        #[arg(num_args = 0..)]
        checks: Vec<String>,
        /// Print the result file's path and return instead of waiting.
        #[arg(long)]
        no_wait: bool,
        /// Give up (exit 2) after this many seconds waiting.
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Build the task's modules, or the named ones, from a worker's or task
    /// agent's worktree (spec §21): snapshots the working tree, then runs
    /// each module's `build_cmd` in its view, generators first, and each
    /// root's in a full worktree of the snapshot. Compiles only, streams
    /// the output and records no verdict; runs here even when `[player]
    /// runs = "queued"`. Jailed like a check: a build that needs the
    /// network needs `[sandbox] network = true`. Exits 1 at the first
    /// failure.
    Build {
        /// Modules and roots to build, each among the task's `modules`.
        #[arg(num_args = 0..)]
        nodes: Vec<String>,
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
        Some(Command::Report { verdict, why, check, suite, concerns }) => run_report(
            args.home,
            dashboard::report::Draft { verdict, why, check, suite, concerns },
        ),
        Some(Command::Trim { slug, checks }) => run_trim(args.home, &slug, &checks),
        Some(Command::Draft { kind }) => run_draft(args.home, kind),
        Some(Command::Stage { slug, why }) => run_marker(args.home, Marker::Stage, &slug, MarkerArgs { answer: None, why }),
        Some(Command::Unstage { slug, why }) => run_marker(args.home, Marker::Unstage, &slug, MarkerArgs { answer: None, why }),
        Some(Command::Accept { slug, why }) => run_marker(args.home, Marker::Accept, &slug, MarkerArgs { answer: None, why }),
        Some(Command::Retire { slug, why }) => run_marker(args.home, Marker::Retire, &slug, MarkerArgs { answer: None, why: Some(why) }),
        Some(Command::Conclude { slug, answer, why }) => {
            run_marker(args.home, Marker::Conclude, &slug, MarkerArgs { answer, why: Some(why) })
        }
        Some(Command::Raise { slug, reason }) => run_raise(args.home, slug.as_deref(), &reason),
        Some(Command::Scope) => run_scope(args.home),
        Some(Command::Test { checks, no_wait, timeout }) => run_test(args.home, &checks, no_wait, timeout),
        Some(Command::Build { nodes }) => run_build(args.home, &nodes),
        Some(Command::Arch { action: ArchAction::Check { root } }) => run_arch_check(&root),
        Some(Command::Arch { action: ArchAction::Gen { root, check } }) => run_arch_gen(&root, check),
        Some(Command::Arch { action: ArchAction::Calls { root, module } }) => run_arch_calls(&root, module.as_deref()),
        Some(Command::Arch { action: ArchAction::AddModule { name, apis } }) => {
            run_arch_add(args.home, dashboard::scaffold::Request { module: Some(name), apis })
        }
        Some(Command::Arch { action: ArchAction::AddApi { apis } }) => {
            run_arch_add(args.home, dashboard::scaffold::Request { module: None, apis })
        }
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

// --------------------------------------------------------------------------
// dashboard report
// --------------------------------------------------------------------------

fn run_report(home: Option<PathBuf>, draft: dashboard::report::Draft) {
    let c = task_clone(home);
    if !c.agent {
        fail("dashboard report runs in a task agent's worktree (branch agent/<slug>)".into());
    }
    let git_dir = c
        .worktree
        .run(&["rev-parse", "--absolute-git-dir"])
        .unwrap_or_else(|e| fail(e.to_string()));
    match dashboard::report::submit(std::path::Path::new(&git_dir), &draft) {
        Ok(f) => println!(
            "reported {} on {}; the player hands it to the director",
            f.verdict.as_str(),
            f.step
        ),
        Err(e) => fail(e),
    }
}

// --------------------------------------------------------------------------
// dashboard trim
// --------------------------------------------------------------------------

/// The home, hub and director clone for a director command (`cmd`), which
/// must run in the director clone.
fn director_clone(home: Option<PathBuf>, cmd: &str) -> (Home, git::Git, git::Git) {
    let cwd = std::env::current_dir().unwrap_or_else(|e| fail(format!("cannot read current directory: {e}")));
    let home = paths::resolve_home(home, &cwd).unwrap_or_else(|e| fail(e));
    let hub = git::Git::new(home.hub());
    let director = hub
        .config_get("dashboard.director")
        .unwrap_or_else(|| fail("hub is missing dashboard.director — run `dashboard init`".into()));
    let here = git::Git::new(&cwd)
        .run(&["rev-parse", "--show-toplevel"])
        .unwrap_or_else(|e| fail(e.to_string()));
    let same = |a: &str, b: &str| {
        std::fs::canonicalize(a).ok().is_some_and(|a| std::fs::canonicalize(b).ok() == Some(a))
    };
    if !same(&here, &director) {
        fail(format!("dashboard {cmd} runs in the director clone ({director})"));
    }
    (home, hub, git::Git::new(&director))
}

fn run_trim(home: Option<PathBuf>, slug: &str, checks: &[String]) {
    let (home, hub, director) = director_clone(home, "trim");
    let runner = cli_runner(&home, &hub_roadmap(&hub));
    let out = dashboard::trim::run(&hub, &director, slug, checks, runner.exporter())
        .unwrap_or_else(|e| fail(e));
    let plan = &out.plan;
    println!(
        "trim {} on task/{slug}: dropped {}",
        &out.commit[..out.commit.len().min(12)],
        plan.dropped.join(", ")
    );
    for (file, text) in &plan.lean {
        println!("  {} {file}", if text.is_some() { "edited " } else { "deleted" });
    }
    for path in &plan.deleted {
        println!("  deleted {path}");
    }
    for (path, why) in &plan.kept {
        println!("  kept    {path} ({why})");
    }
    println!("the player imports it on its next tick and runs the run set; then `dashboard accept {slug}`");
}

// --------------------------------------------------------------------------
// dashboard draft
// --------------------------------------------------------------------------

fn run_draft(home: Option<PathBuf>, kind: DraftKind) {
    let (home, hub, director) = director_clone(home, "draft");
    let runner = cli_runner(&home, &hub_roadmap(&hub));
    let out = match &kind {
        DraftKind::Speedup { slug, phase } => {
            dashboard::draft::speedup(&hub, &director, slug.as_deref(), phase.as_deref(), runner.exporter())
        }
        DraftKind::Flaky { check, slug, phase } => {
            dashboard::draft::flaky(&hub, &director, check, slug.as_deref(), phase.as_deref(), &runner)
        }
    }
    .unwrap_or_else(|e| fail(e));
    let d = &out.draft;
    println!(
        "planned task/{} ({}) in {}: tasks/{}/task.toml",
        d.slug,
        if d.phase.is_empty() { "no phase" } else { d.phase.as_str() },
        &out.commit[..out.commit.len().min(12)],
        d.slug
    );
    for line in &d.derived {
        println!("  derived {line}");
    }
    for line in &d.todo {
        println!("  TODO    {line}");
    }
    println!();
    print!("{}", d.toml);
    println!();
    if d.todo.is_empty() {
        println!("the player imports it on its next tick; review it (`git show task/{}`), replan with a commit if needed, then stage", d.slug);
    } else {
        println!(
            "the player imports it on its next tick; complete the TODO(director) fields with a replan commit on task/{} (`git checkout task/{}`), then stage",
            d.slug, d.slug
        );
    }
}

// --------------------------------------------------------------------------
// dashboard stage | unstage | accept | retire | conclude | raise
// --------------------------------------------------------------------------

fn run_marker(home: Option<PathBuf>, m: Marker, slug: &str, args: MarkerArgs) {
    let (_, hub, director) = director_clone(home, m.name());
    let out = dashboard::marker::run(&hub, &director, m, slug, &args).unwrap_or_else(|e| fail(e));
    let subject = out.message.lines().next().unwrap_or_default();
    println!("{subject} on task/{slug} in {}", &out.commit[..out.commit.len().min(12)]);
    for line in out.message.lines().skip(2) {
        println!("  {line}");
    }
    println!("the player imports it on its next tick");
}

fn run_raise(home: Option<PathBuf>, slug: Option<&str>, reason: &str) {
    let (home, hub, director) = director_clone(home, "raise");
    let runner = cli_runner(&home, &hub_roadmap(&hub));
    let out = dashboard::marker::raise(&hub, &director, slug, reason, runner.exporter()).unwrap_or_else(|e| fail(e));
    println!("raise on main in {}", &out.commit[..out.commit.len().min(12)]);
    for r in &out.raises {
        println!("  {}: {} -> {}", r.budget, r.old, r.new);
    }
    println!("the player imports it on its next tick and runs the suite");
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

/// The snapshot of `c`'s working tree (`queue::snapshot`), fetched into the
/// hub under a temporary `refs/dashboard/test/*` ref: `(commit, hub ref)`.
/// In a sparse tree the `files` of the worker's declared checks are opened
/// first (`records::widen`, spec §21) and the tree snapshotted again, so the
/// run sees the files the worker already wrote there. The caller deletes
/// the ref once done with the commit.
fn snapshot_for_run(c: &TaskClone, hub: &git::Git, runner: &dashboard::checks::Runner) -> (String, String) {
    let local = format!("refs/dashboard/test/{:016}-{}", now_millis(), c.slug);
    let commit = fetch_snapshot(c, hub, &local);
    let opened = dashboard::records::widen(hub, &c.worktree, &c.slug, &commit, Some(runner.exporter()))
        .unwrap_or_else(|e| {
            let _ = hub.delete_ref(&local);
            fail(e)
        });
    if opened.is_empty() {
        return (commit, local);
    }
    eprintln!("opened: {}", opened.join(", "));
    (fetch_snapshot(c, hub, &local), local)
}

/// Snapshots `c`'s working tree and fetches it into the hub as `local`.
fn fetch_snapshot(c: &TaskClone, hub: &git::Git, local: &str) -> String {
    let commit = queue::snapshot(&c.worktree).unwrap_or_else(|e| fail(e));
    let repo = c
        .worktree
        .run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .unwrap_or_else(|e| fail(e.to_string()));
    c.worktree.update_ref(local, &commit).unwrap_or_else(|e| fail(e.to_string()));
    let fetched = hub.run(&["fetch", "--quiet", &repo, &format!("+{local}:{local}")]);
    let _ = c.worktree.delete_ref(local);
    fetched.unwrap_or_else(|e| fail(e.to_string()));
    commit
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The task's modules at the hub tip of `task/<slug>`, with that tip.
fn task_modules(hub: &git::Git, slug: &str) -> (Option<String>, Vec<String>) {
    let tip = hub.rev_parse(&format!("refs/heads/task/{slug}"));
    let modules = tip
        .as_deref()
        .and_then(|t| dashboard::records::task_config_at(hub, t, slug))
        .map(|cfg| cfg.modules)
        .unwrap_or_default();
    (tip, modules)
}

fn run_test(home: Option<PathBuf>, checks: &[String], no_wait: bool, timeout: Option<u64>) {
    let c = task_clone(home);
    let hub = git::Git::new(c.home.hub());
    let roadmap = hub_roadmap(&hub);
    let runner = cli_runner(&c.home, &roadmap);
    let (commit, local) = snapshot_for_run(&c, &hub, &runner);
    let (tip, modules) = task_modules(&hub, &c.slug);
    let queued = roadmap.player.runs == config::Runs::Queued;
    // The run set at the snapshot, so the worker's own checks count. The
    // queued path computes it only to check the names.
    let set = (!queued || !checks.is_empty()).then(|| runner.run_set_for(&commit, &modules, tip.as_deref()));
    // Names outside the run set (or a run set that can't be built for
    // them) are the caller's error.
    let set = set.map(|set| match set.and_then(|s| s.restrict(checks)) {
        Err(e) if !checks.is_empty() => {
            let _ = hub.delete_ref(&local);
            fail(e)
        }
        other => other,
    });
    if !queued {
        // Here and now, in the CLI's own worktree (the cache is the hub's
        // notes); the snapshot's ref goes once the run is done.
        let (why, suite) = match set.expect("computed for a local run") {
            Ok(set) => (Some(set.why.clone()), runner.run_set(&commit, &set)),
            Err(e) => (None, dashboard::checks::SuiteResult::failing(e)),
        };
        let _ = hub.delete_ref(&local);
        print!("{}", queue::render(why.as_deref(), &suite));
        std::process::exit(if suite.pass { 0 } else { 1 });
    }
    let _ = hub.delete_ref(&local);
    let req = queue::enqueue(&c.home, &c.worktree, &c.slug, &commit, checks, now_millis()).unwrap_or_else(|e| fail(e));
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

fn run_build(home: Option<PathBuf>, nodes: &[String]) {
    let c = task_clone(home);
    let hub = git::Git::new(c.home.hub());
    let runner = cli_runner(&c.home, &hub_roadmap(&hub));
    let (commit, local) = snapshot_for_run(&c, &hub, &runner);
    let (_, modules) = task_modules(&hub, &c.slug);
    let built = runner.build(&commit, &modules, nodes);
    let _ = hub.delete_ref(&local);
    built.unwrap_or_else(|e| fail(e));
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

fn run_arch_calls(root: &std::path::Path, module: Option<&str>) {
    let loaded = load_arch(root);
    let cfg = &loaded.cfg;
    if let Some(m) = module
        && !cfg.frames.iter().any(|f| f.module == m)
    {
        eprintln!("no frame for {m} (architecture/Frames/{m}.lean)");
        std::process::exit(1);
    }
    let edges: Vec<(String, String)> = dashboard::frame::edges(cfg)
        .into_iter()
        .filter(|(from, _)| module.is_none_or(|m| from.split('.').next() == Some(m)))
        .collect();
    for (from, to) in &edges {
        println!("{from} -> {to}");
    }
    let framed: Vec<&str> = cfg.frames.iter().map(|f| f.module.as_str()).filter(|m| module.is_none_or(|x| x == *m)).collect();
    for a in cfg.apis.iter().filter(|a| framed.contains(&a.consumer.as_str())) {
        let Some(idl) = loaded.idls.get(&a.name) else { continue };
        for f in &idl.fns {
            let target = format!("{}.{}", a.name, f.name);
            let callers: Vec<&str> = edges.iter().filter(|(_, to)| *to == target).map(|(from, _)| from.as_str()).collect();
            if callers.is_empty() {
                println!("{target}: called by no frame fn of {}", a.consumer);
            } else {
                println!("{target} <- {}", callers.join(", "));
            }
        }
    }
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

fn run_arch_add(home: Option<PathBuf>, req: dashboard::scaffold::Request) {
    use dashboard::scaffold::Build;
    let cmd = if req.module.is_some() { "arch add-module" } else { "arch add-api" };
    let (_, hub, director) = director_clone(home, cmd);
    dashboard::scaffold::on_main_at_hub(&hub, &director).unwrap_or_else(|e| fail(e));
    let root = std::path::PathBuf::from(director.run(&["rev-parse", "--show-toplevel"]).unwrap_or_else(|e| fail(e.to_string())));
    let out = dashboard::scaffold::add(&root, &req).unwrap_or_else(|e| fail(e));
    println!("added to {}: {}", dashboard::config::ARCH_LEAN_MANIFEST, out.entries.join(", "));
    for f in &out.wrote {
        println!("  wrote     {f}");
    }
    for f in &out.generated {
        println!("  generated {f}");
    }
    let mut failed = false;
    match &out.build {
        Build::NoModule => {}
        Build::NoCmd => println!("  build: the module has no build_cmd; run the project's build yourself so lockfiles follow"),
        Build::Ran { cmd, ok: true, .. } => println!("  built     {cmd}"),
        Build::Ran { cmd, ok: false, tail } => {
            failed = true;
            println!("  build FAILED: {cmd}\n{tail}");
        }
    }
    for n in &out.notes {
        println!("  note: {n}");
    }
    let idls: Vec<String> = req.apis.iter().map(|a| dashboard::lean::api_file(&a.name)).collect();
    println!("next, all uncommitted on main:");
    if !idls.is_empty() {
        println!("  1. fill the IDLs ({}), then `dashboard arch gen`", idls.join(", "));
    }
    println!("  2. `dashboard arch check` passes");
    println!("  3. commit everything (`git add -A && git commit`) as one main commit; the player imports it");
    println!("  4. plan the tasks that write the code; their `modules` may now name it");
    if failed {
        std::process::exit(1);
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
