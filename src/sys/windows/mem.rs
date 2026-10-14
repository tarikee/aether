//! Memory: physical memory and the system cache from the performance information, swap from
//! the page files. Windows has no memory pressure figure: `None`.

use super::calls;
use crate::sys::{MemSample, Root};

/// Memory and swap.
pub struct Mem {
    sample: MemSample,
    /// The kernel's table of page files, kept between readings.
    buf: Vec<u64>,
    page: u64,
}

impl Mem {
    pub fn open(_root: &Root) -> Mem {
        Mem {
            sample: MemSample::default(),
            buf: Vec::new(),
            page: 0,
        }
    }

    pub fn read(&mut self) -> &MemSample {
        if self.page == 0 {
            self.page = calls::page_size();
            self.buf.resize(128, 0);
        }
        if let Some((total, available, cached)) = calls::memory() {
            self.sample.total = total;
            self.sample.available = available.min(total);
            self.sample.used = total - self.sample.available;
            self.sample.cached = cached;
        }
        let (swap_total, swap_used) = calls::page_files(&mut self.buf).unwrap_or((0, 0));
        self.sample.swap_total = swap_total * self.page;
        self.sample.swap_used = swap_used.min(swap_total) * self.page;
        &self.sample
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn total_is_more_than_used_which_is_more_than_nothing() {
        let mut mem = Mem::open(&Root::system());
        let s = mem.read();
        assert!(s.total > s.used, "total {} used {}", s.total, s.used);
        assert!(s.used > 0);
        assert!(s.swap_used <= s.swap_total);
        assert!(s.pressure.is_none());
    }
}
