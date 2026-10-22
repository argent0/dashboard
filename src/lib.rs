// Library crate behind the `dashboard` binary (src/main.rs). Splitting it out
// lets integration tests in tests/ exercise these modules directly instead of
// only through the CLI. See CLAUDE.md and spec.md for the overall design.

pub mod arch;
pub mod checks;
pub mod config;
pub mod draft;
pub mod events;
pub mod flaky;
pub mod fsm;
pub mod git;
pub mod idl;
pub mod init;
pub mod lean;
pub mod lint;
pub mod paths;
pub mod player;
pub mod prompts;
pub mod queue;
pub mod raise;
pub mod records;
pub mod render;
pub mod report;
pub mod sandbox;
pub mod speedup;
pub mod spike;
pub mod trim;
pub mod state;
pub mod ui;
