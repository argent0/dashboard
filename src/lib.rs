// Library crate behind the `dashboard` binary (src/main.rs). Splitting it out
// lets integration tests in tests/ exercise these modules directly instead of
// only through the CLI. See CLAUDE.md and spec.md for the overall design.

pub mod config;
pub mod git;
pub mod paths;
pub mod ui;
pub mod legacy;
pub mod init;
pub mod checks;
pub mod records;
pub mod state;
