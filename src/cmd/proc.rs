//! `aether proc PID|NAME`: one process in full.

use crate::cli::Parsed;
use crate::cmd::live::finish;
use crate::out::json::{self, ProcJson};
use crate::out::mask::mask_secrets;
use aether::sys::{
    detail, logical_cpus, thread_list, thread_rows, Cpu, Proc, ProcDetail, Procs, Root, Thread,
    ThreadRow,
};
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
    if parsed.json {
        return run_json(&root, target);
    }
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
    // The CPU % of each thread is over one second between two looks, a share of the whole machine.
    let mut cpu = Cpu::open(&root);
    let cpus = logical_cpus(cpu.read().threads.len());
    let first = thread_list(&root, pid).unwrap_or_default();
    let began = Instant::now();
    sleep(Duration::from_secs(1));
    let threads = thread_list(&root, pid)
        .map(|now| thread_rows(&first, &now, began.elapsed().as_secs_f64(), cpus));
    let mut out = io::stdout().lock();
    finish(write_detail(&mut out, pid, &name, &d, threads.as_deref()))
}

/// `proc --json`: every process the target names, one object each (an array when there are
/// several). The CPU % of a process and of its threads are over the same second as the thread
/// list, a share of the whole machine, as the reading's process list has them.
fn run_json(root: &Root, target: &str) -> ExitCode {
    let found = match matches(root, target) {
        Ok(found) => found,
        Err(message) => {
            eprintln!("aether: {message}");
            return ExitCode::FAILURE;
        }
    };
    let mut procs = Procs::open(root);
    procs.read();
    let mut cpu = Cpu::open(root);
    let cpus = logical_cpus(cpu.read().threads.len());
    let firsts: Vec<Vec<Thread>> = found
        .iter()
        .map(|(pid, _)| thread_list(root, *pid).unwrap_or_default())
        .collect();
    let began = Instant::now();
    sleep(Duration::from_secs(1));
    let secs = began.elapsed().as_secs_f64();
    let list = procs.read();
    let mut rows = Vec::new();
    for ((pid, name), first) in found.iter().zip(firsts.iter()) {
        let Some(detail) = detail(root, *pid) else {
            eprintln!("aether: no process {pid}");
            return ExitCode::FAILURE;
        };
        let live = list.iter().find(|p| p.pid == *pid);
        let shown = live
            .map(|p| p.name.clone())
            .or_else(|| (!name.is_empty()).then(|| name.clone()));
        let threads = thread_list(root, *pid).map(|now| thread_rows(first, &now, secs, cpus));
        rows.push(ProcJson {
            pid: *pid,
            name: shown,
            cpu_pct: live.and_then(|p| p.cpu_pct),
            detail,
            threads,
        });
    }
    let mut buf = Vec::new();
    json::write_procs(&rows, found.len() != 1, &mut buf);
    let mut out = io::stdout().lock();
    finish(out.write_all(&buf))
}

/// Every process a target names, as (pid, name): a PID, every listener of a :PORT, or the
/// processes with that name (or, when none has it, the ones whose name starts with it).
fn matches(root: &Root, target: &str) -> Result<Vec<(u32, String)>, String> {
    if let Ok(pid) = target.parse::<u32>() {
        return Ok(vec![(pid, String::new())]);
    }
    if target.starts_with(':') {
        let Some(port) = crate::cmd::port::parse_port(target) else {
            return Err(format!("{target} is not a port"));
        };
        let found = crate::cmd::port::listeners(root, port)?;
        if found.is_empty() {
            return Err(format!("no process listens on port {port}"));
        }
        return Ok(found);
    }
    let mut procs = Procs::open(root);
    let list = procs.read();
    let mut found = named(list, target);
    if found.is_empty() {
        found = starting(list, target);
    }
    if found.is_empty() {
        return Err(not_found(list, target));
    }
    Ok(found)
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
    let list = procs.read();
    let mut found = named(list, target);
    if found.is_empty() {
        // Reading only: a name that starts with the target is shown, never ended.
        found = starting(list, target);
    }
    match found.as_slice() {
        [] => Err(not_found(list, target)),
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

/// Linux cuts a process name at this many bytes.
const CUT_LEN: usize = 15;

/// The processes whose name is `name` (ASCII case ignored), by pid. A name Linux cut at 15
/// bytes also matches a longer `name` that starts with it: `RocketLeague.ex` for
/// `RocketLeague.exe`. Used by `kill` too, so it never matches by prefix or by "contains".
pub(crate) fn named(procs: &[Proc], name: &str) -> Vec<(u32, String)> {
    collect(procs, |p| {
        p.name.eq_ignore_ascii_case(name) || is_cut_of(&p.name, name)
    })
}

/// Whether `proc_name` is `name` cut by the system at 15 bytes, with `name` longer.
fn is_cut_of(proc_name: &str, name: &str) -> bool {
    proc_name.len() == CUT_LEN
        && name.len() > CUT_LEN
        && name
            .get(..CUT_LEN)
            .is_some_and(|head| head.eq_ignore_ascii_case(proc_name))
}

/// The processes whose name starts with `name` (ASCII case ignored), by pid. Only `proc` uses
/// this (it reads and never ends); an empty `name` matches nothing.
pub(crate) fn starting(procs: &[Proc], name: &str) -> Vec<(u32, String)> {
    collect(procs, |p| {
        !name.is_empty()
            && p.name
                .get(..name.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(name))
    })
}

/// Up to three processes whose name contains `name` (ASCII case ignored), by pid. Only for
/// the error text: an empty `name` gives none.
fn similar(procs: &[Proc], name: &str) -> Vec<(u32, String)> {
    let wanted = name.to_ascii_lowercase();
    let mut found = collect(procs, |p| {
        !wanted.is_empty() && p.name.to_ascii_lowercase().contains(&wanted)
    });
    found.truncate(3);
    found
}

/// The error for a name nothing matched: the same name, then up to three similar names.
pub(crate) fn not_found(procs: &[Proc], name: &str) -> String {
    let similar = similar(procs, name);
    if similar.is_empty() {
        return format!("no process named {name}");
    }
    let shown: Vec<String> = similar.iter().map(|(p, n)| format!("{n} ({p})")).collect();
    format!("no process named {name}; similar: {}", shown.join(", "))
}

/// The processes that `keep` accepts, as (pid, name), by pid.
fn collect(procs: &[Proc], keep: impl Fn(&Proc) -> bool) -> Vec<(u32, String)> {
    let mut found: Vec<(u32, String)> = procs
        .iter()
        .filter(|p| keep(p))
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
    let command = d.cmdline.as_deref().map(mask_secrets);
    writeln!(out, "command    {}", command.as_deref().unwrap_or("-"))?;
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
    if d.cpu_s.is_finite() {
        writeln!(out, "cpu time   {:.2} s", d.cpu_s)?;
    } else {
        writeln!(out, "cpu time   -")?;
    }
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
    fn an_exact_name_or_a_cut_name_matches_for_proc_and_kill() {
        // Linux cuts the name at 15 bytes: RocketLeague.exe is shown as RocketLeague.ex.
        let list = vec![proc(4242, "RocketLeague.ex"), proc(5, "sleep")];
        assert_eq!(
            named(&list, "RocketLeague.ex"),
            vec![(4242, "RocketLeague.ex".to_string())]
        );
        assert_eq!(
            named(&list, "RocketLeague.exe"),
            vec![(4242, "RocketLeague.ex".to_string())]
        );
        assert_eq!(
            named(&list, "rocketleague.EXE"),
            vec![(4242, "RocketLeague.ex".to_string())]
        );
        // Only a 15-byte name is cut, and only a longer target matches it.
        assert!(named(&list, "sleep.exe").is_empty());
        assert!(named(&[proc(9, "abc")], "abc.exe").is_empty());
    }

    #[test]
    fn a_prefix_matches_only_for_proc_and_never_for_kill() {
        let list = vec![proc(4242, "RocketLeague.ex"), proc(7, "sleep")];
        // proc: a name that starts with the target is found.
        assert_eq!(
            starting(&list, "RocketLeague"),
            vec![(4242, "RocketLeague.ex".to_string())]
        );
        assert_eq!(starting(&list, "rocket").len(), 1);
        assert!(starting(&list, "").is_empty());
        // kill uses named, which never takes a prefix or a part of a name.
        assert!(named(&list, "RocketLeague").is_empty());
        assert!(named(&list, "rocket").is_empty());
        assert!(named(&list, "Leag").is_empty());
        assert!(named(&list, "").is_empty());
    }

    #[test]
    fn a_missing_name_names_up_to_three_similar_processes() {
        let list = vec![
            proc(4242, "RocketLeague.ex"),
            proc(10, "rocket-sim"),
            proc(11, "rocketd"),
            proc(12, "my-rocket-app"),
            proc(13, "sleep"),
        ];
        assert_eq!(
            not_found(&list, "rocket"),
            "no process named rocket; similar: rocket-sim (10), rocketd (11), my-rocket-app (12)"
        );
        assert_eq!(
            not_found(&list[..1], "rocket"),
            "no process named rocket; similar: RocketLeague.ex (4242)"
        );
        assert_eq!(not_found(&list, "zzz"), "no process named zzz");
        assert_eq!(not_found(&[], "rocket"), "no process named rocket");
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

    #[test]
    fn a_secret_in_the_command_line_is_masked_in_the_text() {
        let d = ProcDetail {
            cmdline: Some("tool --token abc --port 80".to_string()),
            exe: None,
            cwd: None,
            parents: vec![],
            cpu_s: 0.0,
            open_files: None,
        };
        let mut out: Vec<u8> = Vec::new();
        write_detail(&mut out, 42, "tool", &d, None).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("command    tool --token *** --port 80\n"),
            "{text}"
        );
    }
}
