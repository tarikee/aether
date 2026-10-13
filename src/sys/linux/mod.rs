//! Linux: the OS folder. It reads /proc, /sys and the file system; the kernel calls are in
//! `calls.rs` only.

pub mod battery;
pub mod calls;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod host;
pub mod mem;
pub mod net;
pub mod procs;
pub mod reader;
pub mod sensors;

pub use battery::Battery;
pub use cpu::Cpu;
pub use disk::Disks;
pub use gpu::{GpuProcs, Gpus};
pub use host::Host;
pub use mem::Mem;
pub use net::Nets;
pub use procs::{detail, end, Procs};
pub use reader::{fields, lines, num, value_of, PFile};
pub use sensors::Sensors;

use super::Built;

/// Every source and whether it is built. A source's module flips its own entry to `Yes`.
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
