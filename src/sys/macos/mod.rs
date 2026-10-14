//! macOS: the OS folder. It reads the kernel's tables and counters through system calls (mach,
//! `proc_info`, sysctl, `getfsstat`, `getifaddrs`), which are in `calls.rs` only. The clock,
//! the temperature and the power of the processor need root on macOS: they are left out,
//! never made up.

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

use super::{Built, Root};

/// Every source and whether it is built.
pub fn sources() -> &'static [(&'static str, Built)] {
    &[
        ("cpu", Built::Yes),
        ("memory", Built::Yes),
        ("host", Built::Yes),
        ("gpu", Built::Yes),
        ("sensors", Built::Yes),
        ("disks", Built::Yes),
        ("network", Built::Yes),
        ("battery", Built::Yes),
        ("processes", Built::Yes),
    ]
}

/// What `aether check` lists on macOS: each call the sources use, and whether it answers here,
/// as (what, the call, answers).
pub fn offers() -> Vec<(&'static str, &'static str, bool)> {
    let me = std::process::id();
    let mut ticks = Vec::new();
    let mut mounted = Vec::new();
    let mut links = Vec::new();
    let mut pids = Vec::new();
    vec![
        (
            "CPU counters",
            "host_processor_info",
            calls::cpu_ticks(&mut ticks),
        ),
        (
            "load average",
            "getloadavg",
            calls::load_average().is_some(),
        ),
        ("memory", "host_statistics64", calls::vm_pages().is_some()),
        ("swap", "vm.swapusage", calls::swap_usage().is_some()),
        ("uptime", "kern.boottime", calls::boot_time().is_some()),
        ("disk space", "getfsstat", calls::mounts(&mut mounted)),
        ("network counters", "getifaddrs", calls::links(&mut links)),
        (
            "processes",
            "proc_listallpids",
            calls::list_pids(&mut pids) > 0,
        ),
        (
            "process CPU, memory",
            "proc_pidinfo",
            calls::task_info(me).is_some(),
        ),
        (
            "per-process I/O",
            "proc_pid_rusage",
            calls::usage(me).is_some(),
        ),
        (
            "command lines",
            "KERN_PROCARGS2",
            calls::command_line(me).is_some(),
        ),
        (
            "sockets",
            "proc_pidfdinfo",
            sockets::read_sockets(&Root::system()).is_some(),
        ),
        ("user names", "getpwent", !calls::user_names().is_empty()),
        (
            "disk counters",
            "IOBlockStorageDriver",
            !calls::block_drivers().is_empty(),
        ),
        (
            "graphics cards",
            "IOAccelerator",
            !calls::accelerators().is_empty(),
        ),
        (
            "battery",
            "AppleSmartBattery",
            calls::smart_battery().is_some_and(|b| b.read().is_some()),
        ),
        (
            "temperatures",
            "IOHIDEventSystemClient",
            calls::Thermometers::open()
                .is_some_and(|t| (0..t.names.len()).any(|i| t.celsius(i).is_some())),
        ),
        ("CPU clock and power", "needs root", false),
    ]
}
