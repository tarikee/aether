//! Processes: every process from the kernel's process table in one system call a reading
//! (pid, parent, threads, name, start, CPU time, working sets, I/O bytes), whoever owns it.
//! The account is read from the token once per new process. Memory is the private working
//! set (resident and not shared, as Task Manager shows it); I/O counts every kind of I/O, not
//! the disk alone. GPU use per process is not read: `None`.

use super::{calls, users};
use crate::sys::{EndError, Proc, ProcDetail, Root, Thread};
use std::collections::HashMap;
use std::time::Instant;

/// What a process needs kept from the last reading.
#[derive(Clone, Copy)]
struct Meta {
    created: u64,
    cpu: u64,
    read: u64,
    write: u64,
}

/// Every process.
pub struct Procs {
    list: Vec<Proc>,
    meta: Vec<Meta>,
    next: Vec<Proc>,
    next_meta: Vec<Meta>,
    /// Pid to its place in `list`, made again each reading in the same memory.
    index: HashMap<u32, usize>,
    /// The kernel's process table, kept between readings.
    buf: Vec<u64>,
    at: Option<Instant>,
    /// When the machine started, in 100 ns since 1601.
    boot: u64,
    threads: f64,
}

impl Procs {
    /// No GPU process scan on this OS yet.
    pub fn gpu_shares(&self) -> &[(String, f32, u64)] {
        &[]
    }

    pub fn open(_root: &Root) -> Procs {
        Procs {
            list: Vec::new(),
            meta: Vec::new(),
            next: Vec::new(),
            next_meta: Vec::new(),
            index: HashMap::new(),
            buf: Vec::new(),
            at: None,
            boot: 0,
            threads: 1.0,
        }
    }

    /// No GPU use per process is read on this OS, so there is no GPU scan to skip.
    pub fn skip_gpu(&mut self, _skip: bool) {}

    pub fn read(&mut self) -> &[Proc] {
        let now = Instant::now();
        let seconds = self.at.map(|a| now.duration_since(a).as_secs_f64());
        self.at = Some(now);
        if self.boot == 0 {
            self.boot = calls::now_100ns().saturating_sub(calls::uptime_ms() * 10_000);
            self.threads = f64::from(calls::thread_count().max(1));
        }
        let Procs {
            list,
            meta,
            next,
            next_meta,
            index,
            buf,
            boot,
            threads,
            ..
        } = self;
        index.clear();
        index.extend(list.iter().enumerate().map(|(i, p)| (p.pid, i)));
        next.clear();
        next_meta.clear();
        let ok = calls::processes(buf, |raw| {
            // Pid 0 is the idle time, not a process.
            if raw.pid == 0 {
                return;
            }
            let old = index
                .get(&raw.pid)
                .copied()
                .filter(|&i| meta.get(i).is_some_and(|m| m.created == raw.created));
            let (name, uid, prev) = match old {
                Some(i) => (std::mem::take(&mut list[i].name), list[i].uid, meta.get(i)),
                None => (name_of(raw.name), users::uid_of(raw.pid), None),
            };
            let rate = |a: u64, b: u64| {
                prev.zip(seconds)
                    .filter(|(_, s)| *s > 0.0)
                    .map(|(_, s)| (b.saturating_sub(a) as f64 / s) as u64)
            };
            let cpu_pct = prev.zip(seconds).filter(|(_, s)| *s > 0.0).map(|(m, s)| {
                (raw.cpu.saturating_sub(m.cpu) as f64 / 1e7 / s / *threads * 100.0) as f32
            });
            next.push(Proc {
                pid: raw.pid,
                ppid: raw.ppid,
                uid,
                threads: raw.threads,
                name,
                state: raw.state,
                cpu_pct,
                gpu_pct: None,
                mem: raw.private_working_set,
                rss: raw.working_set,
                started_s: raw.created.saturating_sub(*boot) / 10_000_000,
                vram: None,
                read_bps: rate(prev.map_or(0, |m| m.read), raw.read_bytes),
                write_bps: rate(prev.map_or(0, |m| m.write), raw.write_bytes),
            });
            next_meta.push(Meta {
                created: raw.created,
                cpu: raw.cpu,
                read: raw.read_bytes,
                write: raw.write_bytes,
            });
        });
        if ok {
            std::mem::swap(list, next);
            std::mem::swap(meta, next_meta);
        }
        &self.list
    }
}

/// A process name as shown: the image name without its `.exe`.
fn name_of(units: &[u16]) -> String {
    let mut name = String::from_utf16_lossy(units);
    let cut = name.len().saturating_sub(4);
    if name.len() > 4 && name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".exe") {
        name.truncate(cut);
    }
    name
}

/// The command line a process was started with, cut at 256 bytes (for the filter).
pub fn command_line(_root: &Root, pid: u32) -> Option<String> {
    let mut line = calls::command_line(pid)?;
    if line.len() > 256 {
        let mut cut = 256;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
    }
    Some(line.trim_end().to_string())
}

/// The threads of one process, from a thread snapshot filtered by its pid. Windows gives no
/// thread names here, so a name is `None`; a thread whose CPU cannot be read has no CPU time.
pub fn thread_list(_root: &Root, pid: u32) -> Option<Vec<Thread>> {
    let found = calls::thread_times(pid)?;
    Some(
        found
            .into_iter()
            .map(|(tid, cpu_s)| Thread {
                tid,
                name: None,
                state: None,
                cpu_s,
            })
            .collect(),
    )
}

/// One process in full. The folder it works in is in its own memory, which needs more rights
/// than reading: `None`. Handles are not files on Windows, so no count of open files either.
pub fn detail(_root: &Root, pid: u32) -> Option<ProcDetail> {
    // (pid, parent, start, CPU time, name) of every process.
    let mut table: Vec<(u32, u32, u64, u64, String)> = Vec::new();
    let mut buf = Vec::new();
    calls::processes(&mut buf, |p| {
        table.push((p.pid, p.ppid, p.created, p.cpu, name_of(p.name)));
    });
    let (_, ppid, created, cpu, _) = table.iter().find(|e| e.0 == pid && pid != 0)?.clone();
    Some(ProcDetail {
        cmdline: calls::command_line(pid),
        exe: calls::image_path(pid),
        cwd: None,
        parents: parents(&table, ppid, created),
        cpu_s: cpu as f64 / 1e7,
        open_files: None,
    })
}

/// The chain of parents from `ppid` up. Windows reuses pids, so a parent counts only if it
/// started before its child.
fn parents(table: &[(u32, u32, u64, u64, String)], ppid: u32, created: u64) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let (mut cur, mut child_start) = (ppid, created);
    while cur != 0 && out.len() < 64 {
        let Some(p) = table.iter().find(|e| e.0 == cur && e.2 <= child_start) else {
            break;
        };
        if out.iter().any(|(pid, _)| *pid == cur) {
            break;
        }
        out.push((cur, p.4.clone()));
        child_start = p.2;
        cur = p.1;
    }
    out
}

/// Pid 0 (idle), pid 4 (the kernel) and this program are never ended.
fn refused(pid: u32) -> bool {
    pid == 0 || pid == 4 || pid == std::process::id()
}

fn open(pid: u32) -> Result<calls::Handle, EndError> {
    match calls::open_to_end(pid) {
        Ok(h) if calls::wait_end(&h, 0) => Err(EndError::NotFound),
        Ok(h) => Ok(h),
        Err(calls::OpenError::Gone) => Err(EndError::NotFound),
        Err(calls::OpenError::Denied) => Err(EndError::NotAllowed),
    }
}

/// Ends a process: its windows are asked to close and it gets 3 s; with `force` it is then
/// terminated. A program without a window can only be terminated, so without `force` it is
/// `StillRunning`.
pub fn end(pid: u32, force: bool) -> Result<(), EndError> {
    if refused(pid) {
        return Err(EndError::Refused);
    }
    let process = open(pid)?;
    if calls::close_windows(pid) > 0 && calls::wait_end(&process, 3000) {
        return Ok(());
    }
    if !force {
        return Err(EndError::StillRunning);
    }
    if !calls::terminate(&process) && !calls::wait_end(&process, 0) {
        return Err(EndError::NotAllowed);
    }
    if calls::wait_end(&process, 3000) {
        Ok(())
    } else {
        Err(EndError::StillRunning)
    }
}

/// Sends one "signal" for `aether kill --signal`: TERM (15) asks the windows to close, KILL
/// (9) terminates. Windows has no other signals.
pub fn send_signal(pid: u32, sig: i32) -> Result<(), EndError> {
    if refused(pid) {
        return Err(EndError::Refused);
    }
    let process = open(pid)?;
    match sig {
        15 if calls::close_windows(pid) > 0 => Ok(()),
        15 => Err(EndError::StillRunning),
        9 if calls::terminate(&process) => Ok(()),
        9 => Err(EndError::NotAllowed),
        _ => Err(EndError::Refused),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    fn ping() -> Child {
        Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .expect("ping starts")
    }

    #[test]
    fn names_lose_their_exe() {
        let w: Vec<u16> = "Ping.EXE".encode_utf16().collect();
        assert_eq!(name_of(&w), "Ping");
        let w: Vec<u16> = "System".encode_utf16().collect();
        assert_eq!(name_of(&w), "System");
        let w: Vec<u16> = ".exe".encode_utf16().collect();
        assert_eq!(name_of(&w), ".exe");
    }

    #[test]
    fn the_list_holds_this_test_with_a_name_and_rates_come_second() {
        let me = std::process::id();
        let mut procs = Procs::open(&Root::system());
        let first = procs.read().to_vec();
        let p = first.iter().find(|p| p.pid == me).expect("this process");
        assert!(!p.name.is_empty());
        assert!(p.threads > 0 && p.rss > 0);
        assert!(p.cpu_pct.is_none());
        assert_ne!(p.uid, users::UNKNOWN, "its own account is readable");
        std::thread::sleep(Duration::from_millis(300));
        let second = procs.read();
        let p = second
            .iter()
            .find(|p| p.pid == me)
            .expect("this process again");
        let pct = p.cpu_pct.expect("a percent on the second reading");
        assert!((0.0..=100.0).contains(&pct), "{pct}");
        assert!(p.read_bps.is_some());
        assert!(!second.iter().any(|p| p.pid == 0));
    }

    #[test]
    fn detail_of_this_process() {
        let d = detail(&Root::system(), std::process::id()).expect("detail");
        assert!(d
            .exe
            .as_deref()
            .is_some_and(|e| e.to_ascii_lowercase().ends_with(".exe")));
        assert!(d.cmdline.is_some());
        assert!(!d.parents.is_empty());
        assert!(detail(&Root::system(), 0).is_none());
    }

    #[test]
    fn a_ping_is_ended_with_force_and_by_kill() {
        let mut child = ping();
        let pid = child.id();
        // A console program has no window to ask.
        assert_eq!(end(pid, false), Err(EndError::StillRunning));
        assert_eq!(end(pid, true), Ok(()));
        assert!(child.wait().is_ok());
        assert_eq!(end(pid, true), Err(EndError::NotFound));

        let mut child = ping();
        assert_eq!(send_signal(child.id(), 9), Ok(()));
        assert!(child.wait().is_ok());
    }

    #[test]
    fn idle_kernel_and_itself_are_refused() {
        assert_eq!(end(0, true), Err(EndError::Refused));
        assert_eq!(end(4, true), Err(EndError::Refused));
        assert_eq!(end(std::process::id(), true), Err(EndError::Refused));
        assert_eq!(send_signal(4, 9), Err(EndError::Refused));
    }
}
