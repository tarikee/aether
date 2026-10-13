//! Linux: the OS folder. It reads /proc, /sys and the file system; the kernel calls are in
//! `calls.rs` only.

pub mod battery;
mod calls;
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

/// Every source and whether it is built. A source's package flips its own entry to `Yes`.
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
