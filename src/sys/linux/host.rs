//! Uptime and the thread count of the machine.

use super::cpu::count_threads;
use super::reader::{fields, PFile};
use crate::sys::{HostSample, Root};

const UPTIME_CAP: usize = 64;

/// The host source, with its files kept open.
pub struct Host {
    uptime: Option<PFile>,
    sample: HostSample,
}

impl Host {
    /// The thread count is the number of logical CPUs, counted once here.
    pub fn open(root: &Root) -> Host {
        let stat = std::fs::read(root.path("proc/stat")).map_or(0, |b| count_threads(&b));
        Host {
            uptime: PFile::open(&root.path("proc/uptime"), UPTIME_CAP),
            sample: HostSample {
                uptime_s: 0,
                threads: u32::try_from(stat).unwrap_or(0),
            },
        }
    }

    pub fn read(&mut self) -> &HostSample {
        self.sample.uptime_s = self.uptime.as_mut().and_then(read_uptime).unwrap_or(0);
        &self.sample
    }
}

/// The first number of `proc/uptime`, in whole seconds.
fn read_uptime(file: &mut PFile) -> Option<u64> {
    let text = std::str::from_utf8(fields(file.read()).next()?).ok()?;
    let secs: f64 = text.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn tree(rel: &str) -> Root {
        Root::at(format!(
            "{}/fixtures/basic/{rel}",
            env!("CARGO_MANIFEST_DIR")
        ))
    }

    #[test]
    fn uptime_and_threads() {
        let mut host = Host::open(&tree(""));
        let s = host.read();
        assert_eq!(s.uptime_s, 5400);
        assert_eq!(s.threads, 2);
    }

    #[test]
    fn a_missing_tree_reads_as_nothing() {
        let mut host = Host::open(&tree("none"));
        assert_eq!(*host.read(), HostSample::default());
    }
}
