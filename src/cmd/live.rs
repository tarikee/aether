//! `aether` (live, or once with `--once`): the readings, written out.

use crate::cli::Parsed;
use std::process::ExitCode;

/// Runs the live view or one reading.
pub fn run(_parsed: &Parsed) -> ExitCode {
    ExitCode::SUCCESS
}
