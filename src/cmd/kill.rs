//! `aether kill PID|NAME`: asks a process to stop, then waits; `--force` kills it. With
//! `-s SIGNAL` it sends that one signal and does not wait.

use crate::cli::Parsed;
use crate::cmd::live::finish;
use crate::cmd::proc::named;
use aether::sys::{end, send_signal, EndError, Procs, Root};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

/// The signals `-s` knows by name, as Linux and the other Unix systems number them.
#[cfg(unix)]
const NAMES: &[(&str, i32)] = &[
    ("HUP", libc::SIGHUP),
    ("INT", libc::SIGINT),
    ("QUIT", libc::SIGQUIT),
    ("KILL", libc::SIGKILL),
    ("TERM", libc::SIGTERM),
    ("USR1", libc::SIGUSR1),
    ("USR2", libc::SIGUSR2),
    ("CONT", libc::SIGCONT),
    ("STOP", libc::SIGSTOP),
];

/// Windows has two: TERM asks the windows to close, KILL terminates.
#[cfg(windows)]
const NAMES: &[(&str, i32)] = &[("KILL", 9), ("TERM", 15)];

#[cfg(not(any(unix, windows)))]
const NAMES: &[(&str, i32)] = &[];

/// What the answer to an unknown signal suggests.
#[cfg(not(windows))]
const SIGNAL_HINT: &str = "use a name such as TERM or STOP, or a number from 1 to 64";
#[cfg(windows)]
const SIGNAL_HINT: &str = "Windows has only TERM (15) and KILL (9)";

/// Why a process is still there.
#[cfg(not(windows))]
const STILL_RUNNING: &str = "still running after 3 s; use --force";
#[cfg(windows)]
const STILL_RUNNING: &str =
    "still running: only a program with a window can be asked to close (it gets 3 s); use --force";

/// The number of a signal given as a name (`TERM`, `SIGTERM`, any case) or as a number 1 to 64
/// (on Windows, 9 or 15).
pub(crate) fn signal_number(text: &str) -> Option<i32> {
    if let Ok(n) = text.parse::<i32>() {
        if cfg!(windows) {
            return NAMES.iter().any(|(_, k)| *k == n).then_some(n);
        }
        return (1..=64).contains(&n).then_some(n);
    }
    let upper = text.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(upper.as_str());
    NAMES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, n)| *n)
}

/// How a signal is written in the answer: `SIGSTOP` if it has a name, else `signal 99`.
fn shown(sig: i32) -> String {
    match NAMES.iter().find(|(_, n)| *n == sig) {
        Some((name, _)) => format!("SIG{name}"),
        None => format!("signal {sig}"),
    }
}

/// Ends one process or the processes with a name, or sends them the `-s` signal.
pub fn run(parsed: &Parsed) -> ExitCode {
    let Some(target) = parsed.target.as_deref() else {
        eprintln!("aether: kill needs one PID or NAME");
        return ExitCode::from(2);
    };
    let signal = match parsed.signal.as_deref() {
        None => None,
        Some(text) => match signal_number(text) {
            Some(sig) => Some(sig),
            None => {
                eprintln!("aether: unknown signal {text}; {SIGNAL_HINT}");
                return ExitCode::from(2);
            }
        },
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
        let outcome = match signal {
            Some(sig) => send_signal(*pid, sig).map(|()| format!("sent {} to {label}", shown(sig))),
            None => end(*pid, parsed.force).map(|()| format!("ended {label}")),
        };
        match outcome {
            Ok(line) => {
                let _ = writeln!(out, "{line}");
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
        EndError::StillRunning => STILL_RUNNING.to_string(),
        EndError::Refused => "refused".to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn signals_by_name_with_or_without_sig_and_by_number() {
        assert_eq!(signal_number("STOP"), signal_number("SIGSTOP"));
        assert_eq!(signal_number("sigterm"), signal_number("TERM"));
        assert_eq!(signal_number("0"), None);
        assert_eq!(signal_number("65"), None);
        assert_eq!(signal_number("NOPE"), None);
        assert_eq!(signal_number("SIG"), None);
        #[cfg(not(windows))]
        {
            assert_eq!(signal_number("19"), Some(19));
            assert_eq!(signal_number("1"), Some(1));
            assert_eq!(signal_number("64"), Some(64));
        }
        #[cfg(unix)]
        assert_eq!(signal_number("TERM"), Some(libc::SIGTERM));
    }

    #[cfg(windows)]
    #[test]
    fn windows_knows_term_and_kill_only() {
        assert_eq!(signal_number("TERM"), Some(15));
        assert_eq!(signal_number("sigkill"), Some(9));
        assert_eq!(signal_number("9"), Some(9));
        assert_eq!(signal_number("15"), Some(15));
        assert_eq!(signal_number("HUP"), None);
        assert_eq!(signal_number("STOP"), None);
        assert_eq!(signal_number("1"), None);
    }

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
