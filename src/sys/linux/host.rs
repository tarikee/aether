//! Uptime and the thread count of the machine.

use crate::sys::{HostSample, Root};

/// The host source, with its files kept open.
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
