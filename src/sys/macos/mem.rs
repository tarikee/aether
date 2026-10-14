//! Memory as Activity Monitor adds it up: used is the programs' memory, the wired memory and
//! the compressed memory; cached is the file cache and the purgeable pages; swap comes from
//! `vm.swapusage`. macOS has no memory pressure figure a program can read the way Linux's
//! is: `None`.

use super::calls;
use crate::sys::{MemSample, Root};

/// Memory and swap.
pub struct Mem {
    sample: MemSample,
    page: u64,
}

impl Mem {
    pub fn open(_root: &Root) -> Mem {
        Mem {
            sample: MemSample::default(),
            page: calls::page_size(),
        }
    }

    pub fn read(&mut self) -> &MemSample {
        if let Some(total) = calls::memory_size() {
            self.sample.total = total;
        }
        if let Some(v) = calls::vm_pages() {
            let used = (v.app + v.wired + v.compressed).saturating_mul(self.page);
            self.sample.used = used.min(self.sample.total);
            self.sample.available = self.sample.total - self.sample.used;
            self.sample.cached = v.cached.saturating_mul(self.page).min(self.sample.total);
        }
        let (swap_total, swap_used) = calls::swap_usage().unwrap_or((0, 0));
        self.sample.swap_total = swap_total;
        self.sample.swap_used = swap_used.min(swap_total);
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
        assert_eq!(s.used + s.available, s.total);
        assert!(s.pressure.is_none());
    }
}
