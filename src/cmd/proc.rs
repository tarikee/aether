//! `aether proc PID|NAME`: one process in full.

use crate::cli::Parsed;
use crate::cmd::live::finish;
use aether::sys::{detail, thread_list, thread_rows, Proc, ProcDetail, Procs, Root, ThreadRow};
use std::io::{self, Write};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Prints one process in full.
pub fn run(parsed: &Parsed) -> ExitCode {
    let Some(target) = parsed.target.as_deref() else {
        eprintln!("aether: proc needs one PID or NAME");
        return ExitCode::from(2);
    };
    let root = Root::system();
    let (pid, name) = match resolve(&root, target) {
        Ok(found) => found,
        Err(message) => {
            eprintln!("aether: {message}");
            return ExitCode::FAILURE;
        }
    };
    let Some(d) = detail(&root, pid) else {
        eprintln!("aether: no process {pid}");
        return ExitCode::FAILURE;
    };
    // The CPU % of each thread is over one second between two looks.
    let first = thread_list(&root, pid).unwrap_or_default();
    let began = Instant::now();
    sleep(Duration::from_secs(1));
    let threads =
        thread_list(&root, pid).map(|now| thread_rows(&first, &now, began.elapsed().as_secs_f64()));
    let mut out = io::stdout().lock();
    finish(write_detail(&mut out, pid, &name, &d, threads.as_deref()))
}

/// The process a target names: a PID as it is, or the one process with that name.
pub(crate) fn resolve(root: &Root, target: &str) -> Result<(u32, String), String> {
    if let Ok(pid) = target.parse::<u32>() {
        return Ok((pid, String::new()));
    }
    if target.starts_with(':') {
        let Some(port) = crate::cmd::port::parse_port(target) else {
            return Err(format!("{target} is not a port"));
        };
        let found = crate::cmd::port::listeners(root, port)?;
        return match found.as_slice() {
            [one] => Ok(one.clone()),
            many => {
                let pids: Vec<String> = many.iter().map(|(p, _)| p.to_string()).collect();
                Err(format!(
                    "{} processes listen on port {port} ({}): give one PID",
                    many.len(),
                    pids.join(", ")
                ))
            }
        };
    }
    let mut procs = Procs::open(root);
    let found = named(procs.read(), target);
    match found.as_slice() {
        [] => Err(format!("no process named {target}")),
        [one] => Ok(one.clone()),
        many => {
            let pids: Vec<String> = many.iter().take(8).map(|(p, _)| p.to_string()).collect();
            Err(format!(
                "{} processes named {target} ({}{}): give one PID",
                many.len(),
                pids.join(", "),
                if many.len() > 8 { ", ..." } else { "" }
            ))
        }
    }
}

/// The processes whose name is `name` (ASCII case ignored), by pid.
pub(crate) fn named(procs: &[Proc], name: &str) -> Vec<(u32, String)> {
    let mut found: Vec<(u32, String)> = procs
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| (p.pid, p.name.clone()))
        .collect();
    found.sort_unstable_by_key(|(pid, _)| *pid);
    found
}

/// Writes the detail, one labelled line each. A missing value is "-".
fn write_detail(
    out: &mut dyn Write,
    pid: u32,
    name: &str,
    d: &ProcDetail,
    threads: Option<&[ThreadRow]>,
) -> io::Result<()> {
    writeln!(out, "pid        {pid}")?;
    if !name.is_empty() {
        writeln!(out, "name       {name}")?;
    }
    writeln!(out, "command    {}", d.cmdline.as_deref().unwrap_or("-"))?;
    writeln!(out, "program    {}", d.exe.as_deref().unwrap_or("-"))?;
    writeln!(out, "folder     {}", d.cwd.as_deref().unwrap_or("-"))?;
    let parents = if d.parents.is_empty() {
        "-".to_string()
    } else {
        d.parents
            .iter()
            .map(|(p, n)| format!("{n} ({p})"))
            .collect::<Vec<_>>()
            .join(" <- ")
    };
    writeln!(out, "parents    {parents}")?;
    writeln!(out, "cpu time   {:.2} s", d.cpu_s)?;
    match threads {
        None => writeln!(out, "threads    -")?,
        Some(rows) => {
            writeln!(out, "threads    {}", rows.len())?;
            writeln!(
                out,
                "  {:<8} {:<16} {:<3} {:>7}",
                "tid", "name", "st", "cpu"
            )?;
            for row in rows {
                let t = &row.thread;
                let state = t
                    .state
                    .map_or("-".to_string(), |s| char::from(s).to_string());
                let cpu = row.cpu_pct.map_or("-".to_string(), |p| format!("{p:.1} %"));
                writeln!(
                    out,
                    "  {:<8} {:<16} {:<3} {:>7}",
                    t.tid,
                    t.name.as_deref().unwrap_or("-"),
                    state,
                    cpu
                )?;
            }
        }
    }
    let files = d.open_files.map_or("-".to_string(), |n| n.to_string());
    writeln!(out, "open files {files}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn proc(pid: u32, name: &str) -> Proc {
        Proc {
            pid,
            ppid: 1,
            uid: 0,
            threads: 1,
            name: name.to_string(),
            state: b'S',
            cpu_pct: None,
            gpu_pct: None,
            mem: 0,
            rss: 0,
            started_s: 0,
            vram: None,
            read_bps: None,
            write_bps: None,
        }
    }

    #[test]
    fn names_match_ignoring_case_and_sort_by_pid() {
        let list = vec![proc(30, "Sleep"), proc(7, "bash"), proc(12, "sleep")];
        assert_eq!(
            named(&list, "sleep"),
            vec![(12, "sleep".to_string()), (30, "Sleep".to_string())]
        );
        assert!(named(&list, "slee").is_empty());
    }

    #[test]
    fn threads_are_listed_one_row_each_with_dashes_for_missing_values() {
        use aether::sys::Thread;
        let d = ProcDetail {
            cmdline: None,
            exe: None,
            cwd: None,
            parents: vec![],
            cpu_s: 0.0,
            open_files: None,
        };
        let rows = vec![
            ThreadRow {
                thread: Thread {
                    tid: 7,
                    name: Some("main".to_string()),
                    state: Some(b'S'),
                    cpu_s: Some(1.0),
                },
                cpu_pct: Some(12.5),
            },
            ThreadRow {
                thread: Thread {
                    tid: 9,
                    name: None,
                    state: None,
                    cpu_s: None,
                },
                cpu_pct: None,
            },
        ];
        let mut out: Vec<u8> = Vec::new();
        write_detail(&mut out, 42, "app", &d, Some(&rows)).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("threads    2\n"));
        assert!(text.contains("  7 "));
        assert!(text.contains("main"));
        assert!(text.contains("12.5 %\n"));
        assert!(text.contains("  9 "));
        assert!(text.ends_with("open files -\n"));
    }

    #[test]
    fn a_full_detail_is_written_with_dashes_for_missing_values() {
        let d = ProcDetail {
            cmdline: None,
            exe: Some("/usr/bin/sleep".to_string()),
            cwd: None,
            parents: vec![(1200, "bash".to_string()), (1, "systemd".to_string())],
            cpu_s: 0.5,
            open_files: Some(4),
        };
        let mut out: Vec<u8> = Vec::new();
        write_detail(&mut out, 42, "sleep", &d, None).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("name       sleep\n"));
        assert!(text.contains("command    -\n"));
        assert!(text.contains("program    /usr/bin/sleep\n"));
        assert!(text.contains("parents    bash (1200) <- systemd (1)\n"));
        assert!(text.contains("cpu time   0.50 s\n"));
        assert!(text.ends_with("open files 4\n"));
    }
}
