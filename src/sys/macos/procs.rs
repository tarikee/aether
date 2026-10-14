//! Processes: the kernel's table of every process (name, parent, owner, start, state), then for
//! each one this user may look into its task counters (CPU time, resident size, threads) and
//! its resource usage (memory footprint, bytes read and written). Memory is the footprint
//! Activity Monitor shows. A process of another user cannot be looked into by a program that
//! is not root: it is listed with what the table gives, and its CPU, memory, threads and I/O
//! are left empty rather than made up.

use super::calls;
use crate::sys::{EndError, Proc, ProcDetail, Root, Thread};
use std::collections::HashMap;
use std::thread::sleep;
use std::time::{Duration, Instant};

const WAIT_STEP: Duration = Duration::from_millis(50);
const WAIT_STEPS: u32 = 60;

/// What a process needs kept from the last reading.
#[derive(Clone, Copy)]
struct Meta {
    start: u64,
    cpu_ns: Option<u64>,
    read: Option<u64>,
    write: Option<u64>,
}

/// Every process.
pub struct Procs {
    list: Vec<Proc>,
    meta: Vec<Meta>,
    next: Vec<Proc>,
    next_meta: Vec<Meta>,
    index: HashMap<u32, usize>,
    /// The kernel's table of processes, kept between readings.
    table: Vec<u8>,
    at: Option<Instant>,
    boot: u64,
    threads: f64,
}

impl Procs {
    /// No GPU process scan on this OS.
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
            table: Vec::new(),
            at: None,
            boot: calls::boot_time().unwrap_or(0),
            threads: f64::from(calls::thread_count()),
        }
    }

    pub fn read(&mut self) -> &[Proc] {
        let now = Instant::now();
        let seconds = self
            .at
            .map(|a| now.duration_since(a).as_secs_f64())
            .filter(|s| *s > 0.0);
        self.at = Some(now);
        let Procs {
            list,
            meta,
            next,
            next_meta,
            index,
            table,
            boot,
            threads,
            ..
        } = self;
        index.clear();
        index.extend(list.iter().enumerate().map(|(i, p)| (p.pid, i)));
        next.clear();
        next_meta.clear();
        let count = calls::kinfo_all(table);
        for kp in (0..count).filter_map(|i| calls::kinfo_at(table, i)) {
            let pid = kp.pid;
            let old = index
                .get(&pid)
                .copied()
                .filter(|&i| meta.get(i).is_some_and(|m| m.start == kp.start));
            let prev = old.and_then(|i| meta.get(i)).copied();
            let started_s = (kp.start / 1_000_000).saturating_sub(*boot);
            // The task of a process of another user cannot be read: the table's own fields
            // (name cut to 15 bytes, parent, owner, start, state) are all there is, and the
            // program's path gives the full name.
            let Some((bsd, task)) = calls::task_all(pid) else {
                let name = match old {
                    Some(i) => std::mem::take(&mut list[i].name),
                    None => calls::exe_path(pid)
                        .and_then(|p| p.rsplit('/').next().map(str::to_string))
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| kp.comm()),
                };
                next.push(Proc {
                    pid,
                    ppid: kp.ppid,
                    uid: kp.uid,
                    threads: 0,
                    name,
                    state: state_of(u32::from(kp.stat), 0),
                    cpu_pct: None,
                    gpu_pct: None,
                    mem: 0,
                    rss: 0,
                    started_s,
                    vram: None,
                    read_bps: None,
                    write_bps: None,
                });
                next_meta.push(Meta {
                    start: kp.start,
                    cpu_ns: None,
                    read: None,
                    write: None,
                });
                continue;
            };
            let name = match old {
                Some(i) if same_name(&list[i].name, &bsd) => std::mem::take(&mut list[i].name),
                _ => calls::bsd_name(&bsd),
            };
            let used = calls::usage(pid);
            let cpu_ns =
                calls::ticks_to_ns(task.pti_total_user.saturating_add(task.pti_total_system));
            let rate = |was: Option<u64>, is: Option<u64>| {
                let (a, b, s) = (was?, is?, seconds?);
                Some((b.saturating_sub(a) as f64 / s) as u64)
            };
            let cpu_pct = prev.zip(seconds).and_then(|(m, s)| {
                let a = m.cpu_ns?;
                Some((cpu_ns.saturating_sub(a) as f64 / 1e9 / s / *threads * 100.0) as f32)
            });
            let rss = task.pti_resident_size;
            next.push(Proc {
                pid,
                ppid: bsd.pbi_ppid,
                uid: bsd.pbi_uid,
                threads: u32::try_from(task.pti_threadnum).unwrap_or(0),
                name,
                state: state_of(bsd.pbi_status, task.pti_numrunning),
                cpu_pct,
                gpu_pct: None,
                mem: used.map_or(rss, |u| u.footprint),
                rss,
                started_s,
                vram: None,
                read_bps: prev.and_then(|m| rate(m.read, used.map(|u| u.read))),
                write_bps: prev.and_then(|m| rate(m.write, used.map(|u| u.written))),
            });
            next_meta.push(Meta {
                start: kp.start,
                cpu_ns: Some(cpu_ns),
                read: used.map(|u| u.read),
                write: used.map(|u| u.written),
            });
        }
        std::mem::swap(list, next);
        std::mem::swap(meta, next_meta);
        &self.list
    }
}

/// Whether a name is the one BSD info gives (checked without making a string).
fn same_name(name: &str, bsd: &libc::proc_bsdinfo) -> bool {
    let raw = if bsd.pbi_name[0] != 0 {
        &bsd.pbi_name[..]
    } else {
        &bsd.pbi_comm[..]
    };
    let mut bytes = name.bytes();
    for &c in raw.iter().take_while(|&&c| c != 0) {
        if bytes.next() != Some(c as u8) {
            return false;
        }
    }
    bytes.next().is_none()
}

/// The letter for a process: running when a thread is, else sleeping, with the BSD states for
/// the rest.
fn state_of(status: u32, running: i32) -> u8 {
    match status {
        1 => b'I',
        4 => b'T',
        5 => b'Z',
        _ if running > 0 => b'R',
        _ => b'S',
    }
}

/// A process's arguments, cut to 256 bytes.
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

/// The threads of one process: id, name, state and CPU time, for a process of this user.
pub fn thread_list(_root: &Root, pid: u32) -> Option<Vec<Thread>> {
    let mut ids = Vec::new();
    if !calls::thread_ids(pid, &mut ids) {
        return None;
    }
    let mut out = Vec::with_capacity(ids.len());
    for tid in ids {
        let Some(info) = calls::thread_info(pid, tid) else {
            continue;
        };
        let Ok(tid) = u32::try_from(tid) else {
            continue;
        };
        out.push(Thread {
            tid,
            name: Some(info.name).filter(|n| !n.is_empty()),
            state: Some(info.state),
            cpu_s: Some(info.cpu_ns as f64 / 1e9),
        });
    }
    Some(out)
}

/// One process in full. A process of another user shows what the kernel's table and its program
/// path give; its CPU time, command line, folder and open files are not readable: they are
/// left out (the CPU time is NaN).
pub fn detail(_root: &Root, pid: u32) -> Option<ProcDetail> {
    let kp = calls::kinfo_pid(pid)?;
    let mut parents = Vec::new();
    let mut cur = kp.ppid;
    while cur != 0 && parents.len() < 64 {
        let Some(p) = calls::kinfo_pid(cur) else {
            break;
        };
        let name = calls::bsd_info(cur).map_or_else(|| p.comm(), |b| calls::bsd_name(&b));
        parents.push((cur, name));
        if p.ppid == cur {
            break;
        }
        cur = p.ppid;
    }
    let cpu_s = calls::task_info(pid).map_or(f64::NAN, |t| {
        calls::ticks_to_ns(t.pti_total_user.saturating_add(t.pti_total_system)) as f64 / 1e9
    });
    Some(ProcDetail {
        cmdline: calls::command_line(pid),
        exe: calls::exe_path(pid),
        cwd: calls::cwd_path(pid),
        parents,
        cpu_s,
        open_files: calls::open_file_count(pid),
    })
}

/// Whether the process is there and not a zombie.
fn alive(pid: u32) -> bool {
    calls::kinfo_pid(pid).is_some_and(|k| k.stat != 5)
}

/// Sends a signal; a failure is `NotAllowed` only if the process is still there.
fn signal(pid: u32, sig: i32) -> Result<(), EndError> {
    if calls::signal(pid, sig).is_ok() || !alive(pid) {
        Ok(())
    } else {
        Err(EndError::NotAllowed)
    }
}

/// Looks every 50 ms for up to 3 s whether the process has gone.
fn wait_gone(pid: u32) -> bool {
    for _ in 0..WAIT_STEPS {
        if !alive(pid) {
            return true;
        }
        sleep(WAIT_STEP);
    }
    !alive(pid)
}

/// Ends a process: TERM, then up to 3 s for it to go; with `force`, KILL after that.
/// Pid 0, pid 1 and this program's own pid are refused.
pub fn end(pid: u32, force: bool) -> Result<(), EndError> {
    if pid <= 1 || pid == std::process::id() {
        return Err(EndError::Refused);
    }
    if !alive(pid) {
        return Err(EndError::NotFound);
    }
    signal(pid, libc::SIGTERM)?;
    if wait_gone(pid) {
        return Ok(());
    }
    if !force {
        return Err(EndError::StillRunning);
    }
    signal(pid, libc::SIGKILL)?;
    if wait_gone(pid) {
        Ok(())
    } else {
        Err(EndError::StillRunning)
    }
}

pub use calls::send_signal;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn states() {
        assert_eq!(state_of(2, 1), b'R');
        assert_eq!(state_of(2, 0), b'S');
        assert_eq!(state_of(5, 0), b'Z');
        assert_eq!(state_of(4, 3), b'T');
    }

    #[test]
    fn the_list_holds_this_test_with_a_name_and_the_second_reading_has_rates() {
        let mut p = Procs::open(&Root::system());
        let first = p.read().to_vec();
        let me = std::process::id();
        let mine = first.iter().find(|x| x.pid == me).unwrap();
        assert!(!mine.name.is_empty());
        assert!(mine.threads > 0 && mine.rss > 0 && mine.mem > 0);
        assert!(mine.cpu_pct.is_none(), "no percent from one reading");
        std::thread::sleep(Duration::from_millis(300));
        let second = p.read();
        let mine = second.iter().find(|x| x.pid == me).unwrap();
        assert!(mine.cpu_pct.is_some_and(|c| (0.0..=100.0).contains(&c)));
        assert!(mine.read_bps.is_some() && mine.write_bps.is_some());
    }

    #[test]
    fn detail_and_threads_of_this_process() {
        let me = std::process::id();
        let d = detail(&Root::system(), me).unwrap();
        assert!(d.exe.is_some() && d.cwd.is_some());
        assert!(d.open_files.is_some_and(|n| n > 0));
        assert!(!d.parents.is_empty());
        let t = thread_list(&Root::system(), me).unwrap();
        assert!(!t.is_empty());
        assert!(t.iter().all(|t| t.cpu_s.is_some() && t.state.is_some()));
    }

    #[test]
    fn a_sleep_is_ended_by_pid() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        assert_eq!(end(pid, false), Ok(()));
        let _ = child.wait();
        assert_eq!(end(pid, false), Err(EndError::NotFound));
    }

    #[test]
    fn init_and_itself_are_refused() {
        assert_eq!(end(0, true), Err(EndError::Refused));
        assert_eq!(end(1, true), Err(EndError::Refused));
        assert_eq!(end(std::process::id(), true), Err(EndError::Refused));
    }
}
