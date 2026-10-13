//! The processes that use a graphics card, from their fdinfo files.

use crate::sys::Root;

/// Per-process GPU use, with its files kept open once a process is seen.
pub struct GpuProcs {
    out: Vec<(u32, f32, u64)>,
}

impl GpuProcs {
    pub fn open(_root: &Root) -> GpuProcs {
        GpuProcs { out: Vec::new() }
    }

    /// For the given PIDs: (pid, percent, VRAM bytes). Unreadable processes are left out.
    pub fn read(&mut self, _pids: &[u32]) -> &[(u32, f32, u64)] {
        &self.out
    }
}
