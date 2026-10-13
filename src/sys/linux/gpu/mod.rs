//! Graphics cards and the processes that use them. Each vendor is one file; the list of
//! vendors is `VENDORS` below.

pub mod amd;
pub mod procs;

pub use procs::GpuProcs;

use crate::sys::{GpuSample, Root};

/// THE list of vendors, by the names their files use.
pub const VENDORS: &[&str] = &["amd"];

/// The graphics card source, with its files kept open.
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
