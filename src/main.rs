// Console dashboard for a project's docs/roadmap/tasks.yaml.
//
// Shared across projects — lives outside any single repo. Run it from inside
// a project directory (or pass --file) and it renders that project's roadmap;
// the header title comes from tasks.yaml's own `title` field.
//
// This is the legacy tasks.yaml renderer (see src/legacy.rs). spec.md
// describes a director/player/subagent model, computed from git, that will
// replace it; until that lands the CLI below keeps working unchanged.

mod config;
mod git;
mod legacy;
mod paths;
mod ui;

use clap::Parser;
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

/// Console dashboard for a project's docs/roadmap/tasks.yaml.
#[derive(Parser)]
#[command(name = "dashboard")]
struct Args {
    /// Path to tasks.yaml (default: ./docs/roadmap/tasks.yaml or ./tasks.yaml,
    /// resolved against the current directory).
    #[arg(long)]
    file: Option<PathBuf>,

    /// Disable ANSI colors.
    #[arg(long)]
    no_color: bool,

    /// Override detected terminal width.
    #[arg(long)]
    width: Option<u32>,
}

fn main() {
    let args = Args::parse();

    let file = match args.file.or_else(default_tasks_file) {
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

    let width = ui::detect_width(args.width);
    let color = !args.no_color && std::io::stdout().is_terminal();

    println!("{}", legacy::render(&data, color, width));
}
