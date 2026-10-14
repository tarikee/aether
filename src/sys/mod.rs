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

#[cfg(not(any(target_os = "linux", windows)))]
mod unbuilt;
#[cfg(not(any(target_os = "linux", windows)))]
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
