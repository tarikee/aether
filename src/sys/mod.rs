//! The sample types every OS folder fills in, and the root the OS files are read from.
//! Each OS folder exports the same names; this file picks one folder at compile time.

use std::path::{Path, PathBuf};

pub mod socket;
pub use socket::Socket;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unbuilt;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub use unbuilt::*;

/// Where the OS files are read from: `/`, or `$AETHER_ROOT` (fixture trees in tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    base: PathBuf,
}

impl Root {
    /// The running system.
    pub fn system() -> Root {
        let base = std::env::var_os("AETHER_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Root { base }
    }

    /// A tree rooted at `base`.
    pub fn at(base: impl Into<PathBuf>) -> Root {
        Root { base: base.into() }
    }

    /// A path under the root, for example `root.path("proc/stat")`.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }

    /// The root itself.
    pub fn base(&self) -> &Path {
        &self.base
    }
}

/// Whether a source is built for this OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Built {
    Yes,
    NotYet,
}

/// Why a process could not be ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndError {
    NotFound,
    NotAllowed,
    StillRunning,
    Refused,
}

/// Total and per-thread CPU, clock, temperature and load.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CpuSample {
    pub pct: Option<f32>,
    pub threads: Vec<f32>,
    pub mhz: Option<u32>,
    pub temp_c: Option<f32>,
    pub load: Option<[f32; 3]>,
    /// The clock of each thread in MHz, in thread order (empty when not readable).
    pub threads_mhz: Vec<u32>,
    /// Package power in watts from the energy counter (`None` when not readable).
    pub power_w: Option<f32>,
}

/// Memory and swap, in bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemSample {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub cached: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub pressure: Option<f32>,
}

/// One graphics card.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuSample {
    pub name: String,
    pub vendor: &'static str,
    pub pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub power_w: Option<f32>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    pub fan_rpm: Option<u32>,
}

/// What a sensor measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensorKind {
    Temp,
    Fan,
    Power,
}

/// One temperature, fan or power reading, with its chip and label.
#[derive(Clone, Debug, PartialEq)]
pub struct Sensor {
    pub chip: String,
    pub label: String,
    pub kind: SensorKind,
    pub value: f32,
}

/// One disk, by mount point.
#[derive(Clone, Debug, PartialEq)]
pub struct DiskSample {
    pub mount: String,
    pub dev: String,
    pub total: u64,
    pub used: u64,
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
    /// Share of the time the whole device was busy since the last reading, at most 100.
    pub busy_pct: Option<f32>,
}

/// One network interface.
#[derive(Clone, Debug, PartialEq)]
pub struct NetSample {
    pub name: String,
    pub rx_bps: Option<u64>,
    pub tx_bps: Option<u64>,
    /// The link speed in bits per second, when the system says it.
    pub link_bps: Option<u64>,
    /// The IPv4 and IPv6 addresses (not link-local); empty when not read.
    pub addrs: Vec<String>,
}

/// The battery of a laptop.
#[derive(Clone, Debug, PartialEq)]
pub struct BatterySample {
    pub pct: f32,
    pub state: &'static str,
    pub power_w: Option<f32>,
    pub minutes_left: Option<u32>,
}

/// Uptime and the machine's thread count.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostSample {
    pub uptime_s: u64,
    pub threads: u32,
}

/// One process.
#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub threads: u32,
    pub name: String,
    pub state: u8,
    pub cpu_pct: Option<f32>,
    pub gpu_pct: Option<f32>,
    pub mem: u64,
    pub rss: u64,
    pub started_s: u64,
    pub vram: Option<u64>,
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
}

/// A group of processes with the same name ("firefox x23").
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub name: String,
    pub count: u32,
    pub cpu_pct: Option<f32>,
    pub gpu_pct: Option<f32>,
    pub mem: u64,
    pub vram: Option<u64>,
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
}

/// One process in full, for `aether proc`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcDetail {
    pub cmdline: Option<String>,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub parents: Vec<(u32, String)>,
    pub cpu_s: f64,
    pub open_files: Option<u32>,
}

/// One thread of a process: its CPU time so far and its state, read on each look. A name or a
/// state the OS does not give is `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub tid: u32,
    pub name: Option<String>,
    pub state: Option<u8>,
    pub cpu_s: Option<f64>,
}

/// A thread with its CPU % over the time since the last look; `None` on the first look, or
/// where a CPU time is missing.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadRow {
    pub thread: Thread,
    pub cpu_pct: Option<f32>,
}

/// The number of CPUs a thread % is a share of: `count` (the threads of a CPU reading) when
/// there is one, else the system's count.
pub fn logical_cpus(count: usize) -> f64 {
    if count > 0 {
        return count as f64;
    }
    std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64)
}

/// Pairs each thread of `now` with its CPU % over the `secs` since the look `prev`, most CPU
/// first (no % last), then by TID. A thread not in `prev` has no %. The % is a share of the
/// whole machine (`cpus` CPUs, at least 1), like the process list's.
pub fn thread_rows(prev: &[Thread], now: &[Thread], secs: f64, cpus: f64) -> Vec<ThreadRow> {
    let before: std::collections::HashMap<u32, f64> = prev
        .iter()
        .filter_map(|t| Some((t.tid, t.cpu_s?)))
        .collect();
    let mut rows: Vec<ThreadRow> = now
        .iter()
        .map(|t| {
            let cpu_pct = match (before.get(&t.tid), t.cpu_s) {
                (Some(&a), Some(b)) if secs > 0.0 && b >= a => {
                    Some(((b - a) / secs / cpus.max(1.0) * 100.0) as f32)
                }
                _ => None,
            };
            ThreadRow {
                thread: t.clone(),
                cpu_pct,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        let by_cpu = match (a.cpu_pct, b.cpu_pct) {
            (Some(x), Some(y)) => y.total_cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        by_cpu.then(a.thread.tid.cmp(&b.thread.tid))
    });
    rows
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod thread_tests {
    use super::*;

    fn t(tid: u32, cpu_s: Option<f64>) -> Thread {
        Thread {
            tid,
            name: None,
            state: None,
            cpu_s,
        }
    }

    #[test]
    fn cpu_percent_is_the_change_over_the_seconds_between_looks() {
        let before = [t(1, Some(1.0)), t(2, Some(4.0)), t(3, None)];
        let now = [
            t(1, Some(1.5)),
            t(2, Some(4.0)),
            t(3, Some(9.0)),
            t(4, Some(2.0)),
        ];
        let rows = thread_rows(&before, &now, 2.0, 1.0);
        let pct = |tid: u32| rows.iter().find(|r| r.thread.tid == tid).unwrap().cpu_pct;
        assert_eq!(pct(1), Some(25.0));
        assert_eq!(pct(2), Some(0.0));
        assert_eq!(pct(3), None);
        assert_eq!(pct(4), None);
    }

    #[test]
    fn the_first_look_has_no_percent_and_the_busiest_thread_comes_first() {
        let now = [t(5, Some(0.5)), t(6, Some(3.0)), t(2, None)];
        let rows = thread_rows(&[], &now, 1.0, 1.0);
        assert!(rows.iter().all(|r| r.cpu_pct.is_none()));
        let order: Vec<u32> = rows.iter().map(|r| r.thread.tid).collect();
        assert_eq!(order, vec![2, 5, 6]);
        let before = [t(5, Some(0.0)), t(6, Some(1.0))];
        let now = [t(5, Some(2.0)), t(6, Some(1.5)), t(2, None)];
        let order: Vec<u32> = thread_rows(&before, &now, 1.0, 1.0)
            .iter()
            .map(|r| r.thread.tid)
            .collect();
        assert_eq!(order, vec![5, 6, 2]);
    }

    #[test]
    fn thread_percent_is_a_share_of_the_whole_machine() {
        // Two threads each use 1 s of CPU in 1 s on a 4-CPU machine: 25 % of the machine each.
        let before = [t(1, Some(0.0)), t(2, Some(0.0))];
        let now = [t(1, Some(1.0)), t(2, Some(1.0))];
        let rows = thread_rows(&before, &now, 1.0, 4.0);
        assert!(rows.iter().all(|r| r.cpu_pct == Some(25.0)));
        assert_eq!(logical_cpus(4), 4.0);
    }
}
