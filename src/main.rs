//! The command line program. The list of commands is `COMMANDS` below.
#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod cli;
mod cmd;
mod out;

use std::io::Write;
use std::process::ExitCode;

/// A command runs from the parsed arguments and gives its exit code.
type Run = fn(&cli::Parsed) -> ExitCode;

/// THE list of commands. A new command is one file in `cmd/` and one line here.
const COMMANDS: &[(&str, Run)] = &[
    ("live", cmd::live::run),
    ("proc", cmd::proc::run),
    ("kill", cmd::kill::run),
    ("check", cmd::check::run),
    ("help", cli::help),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let parsed = match cli::parse(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            let _ = writeln!(std::io::stderr(), "aether: {message}");
            let _ = writeln!(std::io::stderr(), "Try `aether help`.");
            return ExitCode::from(2);
        }
    };
    match COMMANDS.iter().find(|(name, _)| *name == parsed.command) {
        Some((_, run)) => run(&parsed),
        None => ExitCode::from(2),
    }
}
