// Console dashboard for a director/player/subagent roadmap tracked in git
// (spec.md), plus — until step 8 of the migration plan — the legacy
// tasks.yaml renderer (src/legacy.rs) that this CLI grew up rendering.
//
// This binary is a thin CLI over the `dashboard` library crate (src/lib.rs);
// integration tests in tests/ exercise the library directly.

use clap::{Parser, Subcommand};
use dashboard::{init, legacy, paths, player, ui};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

// --------------------------------------------------------------------------
// CLI
// --------------------------------------------------------------------------

fn default_tasks_file() -> Option<PathBuf> {
    for candidate in [Path::new("docs/roadmap/tasks.yaml"), Path::new("tasks.yaml")] {
        if candidate.exists() {
            return std::fs::canonicalize(candidate).ok();
        }
    }
    None
}

/// Console dashboard for a project's roadmap.
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

    /// Path to tasks.yaml (legacy renderer only; default:
    /// ./docs/roadmap/tasks.yaml or ./tasks.yaml, resolved against the
    /// current directory).
    #[arg(long)]
    file: Option<PathBuf>,

    /// Disable ANSI colors.
    #[arg(long)]
    no_color: bool,

    /// Override detected terminal width.
    #[arg(long)]
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
}

fn main() {
    let args = Args::parse();

    match args.command {
        Some(Command::Init { name }) => run_init(name, args.home),
        Some(Command::Player { once, interval }) => run_player(args.home, once, interval),
        None => run_legacy(args.file, args.no_color, args.width),
    }
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
    // step 6 swaps NullSessions for the real tmux-backed Sessions impl.
    let mut player = match player::Player::open(home, player::NullSessions::default()) {
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
// legacy tasks.yaml renderer
// --------------------------------------------------------------------------

fn run_legacy(file: Option<PathBuf>, no_color: bool, width: Option<u32>) {
    let file = match file.or_else(default_tasks_file) {
        Some(f) => f,
        None => {
            eprintln!("error: no tasks.yaml found (tried ./docs/roadmap/tasks.yaml and ./tasks.yaml) — pass --file");
            std::process::exit(1);
        }
    };
    if !file.exists() {
        eprintln!("error: {} not found", file.display());
        std::process::exit(1);
    }

    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {} could not be read: {e}", file.display());
            std::process::exit(1);
        }
    };

    let data: legacy::Roadmap = match serde_yaml::from_str(&text) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {} is not valid: {e}", file.display());
            std::process::exit(1);
        }
    };

    let width = ui::detect_width(width);
    let color = !no_color && std::io::stdout().is_terminal();

    println!("{}", legacy::render(&data, color, width));
}
