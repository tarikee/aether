//! Graphics: the accelerator's name and its busy share from the I/O registry
//! (`IOAccelerator`, `PerformanceStatistics`, "Device Utilization %"). The memory of an Apple
//! silicon GPU is the machine's own memory, shared with the processor, so there is no video
//! memory to report: `None`. Temperature, fan and power need root: `None`.

use super::calls::{self, Accelerator};
use crate::sys::{GpuSample, Root};

/// Graphics cards.
pub struct Gpus {
    found: Vec<Accelerator>,
    list: Vec<GpuSample>,
}

impl Gpus {
    pub fn open(_root: &Root) -> Gpus {
        let found = calls::accelerators();
        let list = found
            .iter()
            .map(|a| GpuSample {
                name: a.name.clone(),
                vendor: "apple",
                pct: None,
                temp_c: None,
                power_w: None,
                vram_used: None,
                vram_total: None,
                fan_rpm: None,
            })
            .collect();
        Gpus { found, list }
    }

    pub fn read(&mut self, _shares: Option<&[(String, f32, u64)]>) -> &[GpuSample] {
        for (a, s) in self.found.iter().zip(&mut self.list) {
            s.pct = a.utilization();
        }
        &self.list
    }
}

/// Per-process GPU use: macOS gives it to no program that is not root, so there is none.
pub struct GpuProcs {
    out: Vec<(u32, f32, u64)>,
}

impl GpuProcs {
    pub fn open(_root: &Root) -> GpuProcs {
        GpuProcs { out: Vec::new() }
    }

    pub fn read(&mut self, _pids: &[u32]) -> &[(u32, f32, u64)] {
        &self.out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A Mac has a GPU, but a virtual machine may not show one: only the range is checked.
    #[test]
    fn a_gpu_reads_a_percent_in_range() {
        let mut g = Gpus::open(&Root::system());
        for s in g.read(None) {
            assert!(!s.name.is_empty());
            assert!(s.pct.is_none_or(|p| (0.0..=100.0).contains(&p)));
            assert!(s.vram_total.is_none() && s.temp_c.is_none());
        }
    }
}
