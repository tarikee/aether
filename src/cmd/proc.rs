//! `aether proc PID|NAME`: one process in full.

use crate::cli::Parsed;
use std::process::ExitCode;

/// Prints one process.
pub fn run(_parsed: &Parsed) -> ExitCode {
    ExitCode::SUCCESS
}
