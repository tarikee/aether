//! Windows: the OS folder. It reads the kernel's tables and counters through system calls,
//! which are in `calls.rs` only. Load average and sensors do not exist here without a driver:
//! they are left out, never made up.

pub mod battery;
pub mod calls;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod host;
pub mod mem;
pub mod net;
pub mod procs;
pub mod sensors;
pub mod sockets;
pub mod users;

pub use battery::Battery;
pub use cpu::Cpu;
pub use disk::Disks;
pub use gpu::{GpuProcs, Gpus};
pub use host::Host;
pub use mem::Mem;
pub use net::Nets;
pub use procs::{command_line, detail, end, send_signal, thread_list, Procs};
pub use sensors::Sensors;
pub use sockets::{attach_owners, read_sockets};

use super::Built;

/// Every source and whether it is built. Sensors need a driver on Windows: not offered.
pub fn sources() -> &'static [(&'static str, Built)] {
    &[
        ("cpu", Built::Yes),
        ("memory", Built::Yes),
        ("host", Built::Yes),
        ("gpu", Built::Yes),
        ("sensors", Built::NotYet),
        ("disks", Built::Yes),
        ("network", Built::Yes),
        ("battery", Built::Yes),
        ("processes", Built::Yes),
    ]
}

/// What `aether check` lists on Windows: each call the sources use, and whether it answers
/// here, as (what, the call, answers).
pub fn offers() -> Vec<(&'static str, &'static str, bool)> {
    let mut buf = Vec::new();
    let mut times = Vec::new();
    let mut mhz = Vec::new();
    let mut ifaces = Vec::new();
    let mut adapters = Vec::new();
    calls::adapters(&mut adapters);
    let c = calls::wide("C:\\");
    vec![
        (
            "CPU counters",
            "NtQuerySystemInformation",
            calls::processor_times(&mut buf, &mut times),
        ),
        (
            "CPU clock",
            "CallNtPowerInformation",
            calls::processor_mhz(&mut buf, calls::thread_count() as usize, &mut mhz),
        ),
        ("memory", "GetPerformanceInfo", calls::memory().is_some()),
        (
            "page files",
            "NtQuerySystemInformation",
            calls::page_files(&mut buf).is_some(),
        ),
        (
            "disk space",
            "GetDiskFreeSpaceExW",
            calls::space(&c).is_some(),
        ),
        (
            "disk counters",
            "IOCTL_DISK_PERFORMANCE",
            disk::counters_answer(),
        ),
        (
            "network counters",
            "GetIfTable2",
            calls::interfaces(&mut ifaces),
        ),
        (
            "processes",
            "NtQuerySystemInformation",
            calls::processes(&mut buf, |_| {}),
        ),
        (
            "user names",
            "OpenProcessToken",
            calls::process_sid(std::process::id()).is_some(),
        ),
        (
            "battery",
            "GetSystemPowerStatus",
            calls::power_status().is_some_and(|s| s.1 != 128 && s.1 != 255),
        ),
        (
            "graphics cards",
            "D3DKMTQueryStatistics",
            adapters.iter().any(|a| a.render && !a.software),
        ),
        ("sensors", "none without a driver", false),
    ]
}
