//! The same names as the Linux folder, with empty readings. Used on the OSes whose source is
//! not built yet; each source's entry in `sources()` stays `NotYet` until its OS support lands.

use super::{
    BatterySample, Built, CpuSample, DiskSample, EndError, GpuSample, HostSample, MemSample,
    NetSample, Proc, ProcDetail, Root, Sensor,
};
use std::path::Path;

/// Every source and whether it is built.
pub fn sources() -> &'static [(&'static str, Built)] {
    &[
        ("cpu", Built::NotYet),
        ("memory", Built::NotYet),
        ("host", Built::NotYet),
        ("gpu", Built::NotYet),
        ("sensors", Built::NotYet),
        ("disks", Built::NotYet),
        ("network", Built::NotYet),
        ("battery", Built::NotYet),
        ("processes", Built::NotYet),
    ]
}

/// Ends a process: a signal, then a wait; with `force`, a kill.
pub fn end(_pid: u32, _force: bool) -> Result<(), EndError> {
    Err(EndError::Refused)
}

/// One process in full.
pub fn command_line(_root: &Root, _pid: u32) -> Option<String> {
    None
}

pub fn detail(_root: &Root, _pid: u32) -> Option<ProcDetail> {
    None
}

/// A file kept open between readings.
pub struct PFile {
    buf: Vec<u8>,
}

impl PFile {
    pub fn open(_path: &Path, _cap: usize) -> Option<PFile> {
        None
    }

    pub fn read(&mut self) -> &[u8] {
        &self.buf
    }
}

/// The lines of a buffer, without empty ones.
pub fn lines(_buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::empty()
}

/// The whitespace-separated fields of a line.
pub fn fields(_line: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::empty()
}

/// A plain decimal number, or `None`.
pub fn num(_field: &[u8]) -> Option<u64> {
    None
}

/// The first number after `key:` on its line.
pub fn value_of(_buf: &[u8], _key: &str) -> Option<u64> {
    None
}

/// Per-thread and total CPU, clock, temperature and load.
pub struct Cpu {
    sample: CpuSample,
}

impl Cpu {
    pub fn open(_root: &Root) -> Cpu {
        Cpu {
            sample: CpuSample::default(),
        }
    }

    pub fn read(&mut self) -> &CpuSample {
        &self.sample
    }
}

/// Memory and swap.
pub struct Mem {
    sample: MemSample,
}

impl Mem {
    pub fn open(_root: &Root) -> Mem {
        Mem {
            sample: MemSample::default(),
        }
    }

    pub fn read(&mut self) -> &MemSample {
        &self.sample
    }
}

/// Uptime and thread count.
pub struct Host {
    sample: HostSample,
}

impl Host {
    pub fn open(_root: &Root) -> Host {
        Host {
            sample: HostSample::default(),
        }
    }

    pub fn read(&mut self) -> &HostSample {
        &self.sample
    }
}

/// Graphics cards.
pub struct Gpus {
    list: Vec<GpuSample>,
}

impl Gpus {
    pub fn open(_root: &Root) -> Gpus {
        Gpus { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[GpuSample] {
        &self.list
    }
}

/// Per-process GPU use: pid, percent, VRAM bytes.
pub struct GpuProcs {
    out: Vec<(u32, f32, u64)>,
}

impl GpuProcs {
    pub fn open(_root: &Root) -> GpuProcs {
        GpuProcs { out: Vec::new() }
    }

    pub fn read(&mut self, _pids: &[u32]) -> &[(u32, f32, u64)] {
        &self.out
    }
}

/// Temperatures, fans and power from the hardware monitors.
pub struct Sensors {
    list: Vec<Sensor>,
}

impl Sensors {
    pub fn open(_root: &Root) -> Sensors {
        Sensors { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[Sensor] {
        &self.list
    }
}

/// Mounted disks: space and rates.
pub struct Disks {
    list: Vec<DiskSample>,
}

impl Disks {
    pub fn open(_root: &Root) -> Disks {
        Disks { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[DiskSample] {
        &self.list
    }
}

/// Network interfaces: rates.
pub struct Nets {
    list: Vec<NetSample>,
}

impl Nets {
    pub fn open(_root: &Root) -> Nets {
        Nets { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[NetSample] {
        &self.list
    }
}

/// The battery, when there is one.
pub struct Battery {
    sample: Option<BatterySample>,
}

impl Battery {
    pub fn open(_root: &Root) -> Battery {
        Battery { sample: None }
    }

    pub fn read(&mut self) -> Option<&BatterySample> {
        self.sample.as_ref()
    }
}

/// Every process.
pub struct Procs {
    list: Vec<Proc>,
}

impl Procs {
    pub fn open(_root: &Root) -> Procs {
        Procs { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[Proc] {
        &self.list
    }
}
