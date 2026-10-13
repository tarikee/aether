//! CPU: total and per thread, clock, temperature and load (proc/stat and friends).

use crate::sys::{CpuSample, Root};

/// The CPU source, with its files kept open.
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
