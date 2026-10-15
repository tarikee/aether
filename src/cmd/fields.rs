//! `aether fields [--json]`: every value `aether --json` prints, with its unit, when it is null
//! and which systems give it.

use crate::cli::Parsed;
use crate::cmd::live::finish;
use crate::out::fields;
use std::io::{self, Write};
use std::process::ExitCode;

/// Prints the table, or with `--json` the object a program reads.
pub fn run(parsed: &Parsed) -> ExitCode {
    let mut buf = Vec::new();
    if parsed.json {
        fields::write_json(&mut buf);
    } else {
        fields::write_text(&mut buf);
    }
    finish(io::stdout().lock().write_all(&buf))
}
