//! `aether check`: what this machine and OS offer, and what is missing.

use crate::cli::Parsed;
use std::process::ExitCode;

/// Prints what can be read here.
pub fn run(_parsed: &Parsed) -> ExitCode {
    ExitCode::SUCCESS
}
