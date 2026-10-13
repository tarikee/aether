//! `aether kill PID|NAME`: asks a process to stop, then waits; `--force` kills it.

use crate::cli::Parsed;
use std::process::ExitCode;

/// Ends one process or the processes with a name.
pub fn run(_parsed: &Parsed) -> ExitCode {
    ExitCode::SUCCESS
}
