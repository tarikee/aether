//! Uptime and the machine's thread count.

use super::calls;
use crate::sys::{HostSample, Root};

/// Uptime and thread count.
pub struct Host {
    sample: HostSample,
    boot: u64,
}

impl Host {
    pub fn open(_root: &Root) -> Host {
        Host {
            sample: HostSample::default(),
            boot: calls::boot_time().unwrap_or(0),
        }
    }

    pub fn read(&mut self) -> &HostSample {
        self.sample.uptime_s = if self.boot > 0 {
            calls::now_s().saturating_sub(self.boot)
        } else {
            0
        };
        self.sample.threads = calls::thread_count();
        &self.sample
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn threads_and_uptime() {
        let s = Host::open(&Root::system()).read().clone();
        assert!(s.threads > 0);
        assert!(s.uptime_s > 0);
    }
}
