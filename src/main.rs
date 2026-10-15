//! The command line program. The list of commands is `COMMANDS` below.
#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod cli;
mod cmd;
mod defaults;
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
    ("record", cmd::record::run),
    ("peaks", cmd::record::peaks),
    ("alert", cmd::alert::run),
    ("port", cmd::port::run),
    ("check", cmd::check::run),
    ("fields", cmd::fields::run),
    ("version", cli::version),
    ("help", cli::help),
];

/// Raises the open-file limit once, before any source opens its files.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn raise_limits() {
    aether::sys::calls::raise_files();
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn raise_limits() {}

/// Parses the command line. The live view and `--once` first take the arguments of the file of
/// default arguments, which come before the command line's own, so the command line wins.
/// Nothing else reads the file, and `--no-defaults` skips it.
fn with_defaults(args: &[String]) -> Result<cli::Parsed, String> {
    let first = cli::parse(args)?;
    if first.command != "live" || first.no_defaults {
        return Ok(first);
    }
    let defaults = defaults::arguments();
    if defaults.is_empty() {
        return Ok(first);
    }
    let mut all = defaults;
    all.extend_from_slice(args);
    let parsed = cli::parse(&all).map_err(|m| {
        let file = defaults::path()
            .map_or_else(|| "the defaults".to_string(), |p| p.display().to_string());
        format!("{file}: {m}")
    })?;
    if parsed.command != "live" {
        return Err("the defaults file holds options only".to_string());
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    raise_limits();
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let parsed = match with_defaults(&args) {
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
