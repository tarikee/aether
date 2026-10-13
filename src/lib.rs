//! The core: the samples every OS fills in, and the sources that read them.
#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod sys;

use sys::{
    BatterySample, CpuSample, DiskSample, GpuSample, HostSample, MemSample, NetSample, Proc, Sensor,
};

/// Everything the machine shows, in one reading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// Seconds since the program started.
    pub t: f64,
    pub cpu: CpuSample,
    pub mem: MemSample,
    pub gpus: Vec<GpuSample>,
    pub sensors: Vec<Sensor>,
    pub disks: Vec<DiskSample>,
    pub nets: Vec<NetSample>,
    pub battery: Option<BatterySample>,
    pub host: HostSample,
    pub procs: Vec<Proc>,
}
