//! `aether kill PID|NAME`: asks a process to stop, then waits; `--force` kills it.

use crate::cli::Parsed;
use crate::cmd::live::finish;
use crate::cmd::proc::named;
use aether::sys::{end, EndError, Procs, Root};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

/// Ends one process or the processes with a name.
pub fn run(parsed: &Parsed) -> ExitCode {
    let Some(target) = parsed.target.as_deref() else {
        eprintln!("aether: kill needs one PID or NAME");
        return ExitCode::from(2);
    };
    let root = Root::system();
    let me = std::process::id();
    let picks: Vec<(u32, String)> = match target.parse::<u32>() {
        Ok(pid) => {
            if refused(pid, me) {
                eprintln!("aether: refused: pid {pid} is init or aether itself");
                return ExitCode::FAILURE;
            }
            vec![(pid, String::new())]
        }
        Err(_) => {
            let mut procs = Procs::open(&root);
            let mut found = named(procs.read(), target);
            found.retain(|(pid, _)| !refused(*pid, me));
            if found.is_empty() {
                eprintln!("aether: no process named {target}");
                return ExitCode::FAILURE;
            }
            if !confirm(&found, parsed.yes) {
                return ExitCode::FAILURE;
            }
            found
        }
    };
    let mut out = io::stdout().lock();
    let mut all_ended = true;
    for (pid, name) in &picks {
        let label = if name.is_empty() {
            format!("pid {pid}")
        } else {
            format!("pid {pid} ({name})")
        };
        match end(*pid, parsed.force) {
            Ok(()) => {
                let _ = writeln!(out, "ended {label}");
            }
            Err(e) => {
                all_ended = false;
                eprintln!("aether: {label}: {}", reason(e, *pid));
            }
        }
    }
    if all_ended {
        finish(Ok(()))
    } else {
        ExitCode::FAILURE
    }
}

/// Pid 0 and 1 (init) and aether's own pid are never ended.
pub(crate) fn refused(pid: u32, me: u32) -> bool {
    pid <= 1 || pid == me
}

/// Lists the matches, then asks unless `yes` is set. Without a terminal and without `yes`
/// it does not wait for an answer; it refuses.
fn confirm(found: &[(u32, String)], yes: bool) -> bool {
    for (pid, name) in found {
        eprintln!("  pid {pid} {name}");
    }
    if yes {
        return true;
    }
    if !io::stdin().is_terminal() {
        eprintln!(
            "aether: {} match; pass -y to end without asking",
            found.len()
        );
        return false;
    }
    eprint!("End {} processes? [y/N] ", found.len());
    let _ = io::stderr().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let answer = line.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// The words for a process that was not ended.
fn reason(e: EndError, pid: u32) -> String {
    match e {
        EndError::NotFound => format!("no such process ({pid})"),
        EndError::NotAllowed => "not allowed: it belongs to another user".to_string(),
        EndError::StillRunning => "still running after 3 s; use --force".to_string(),
        EndError::Refused => "refused".to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn init_and_itself_are_refused() {
        assert!(refused(0, 500));
        assert!(refused(1, 500));
        assert!(refused(500, 500));
        assert!(!refused(501, 500));
    }

    /// Ends a `sleep` this test starts, by pid and by name.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_sleep_is_ended_by_pid_and_by_name() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        assert_eq!(end(pid, false), Ok(()));
        let _ = child.wait();

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut procs = Procs::open(&Root::system());
        let found = named(procs.read(), "sleep");
        assert!(found.iter().any(|(p, _)| *p == pid));
        assert_eq!(end(pid, true), Ok(()));
        let _ = child.wait();
    }
}
