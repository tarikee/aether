//! Memory and swap (proc/meminfo and the memory pressure file).

use crate::sys::{MemSample, Root};

/// The memory source, with its files kept open.
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
