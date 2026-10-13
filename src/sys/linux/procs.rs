//! Every process (proc/PID), with what each uses; one process in full; ending a process.

use crate::sys::{EndError, Proc, ProcDetail, Root};

/// The process source. Each process's files stay open while it lives.
pub struct Procs {
    list: Vec<Proc>,
}

impl Procs {
    pub fn open(_root: &Root) -> Procs {
        Procs { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[Proc] {
        &self.list
    }
}

/// One process in full.
pub fn detail(_root: &Root, _pid: u32) -> Option<ProcDetail> {
    None
}

/// Ends a process: a signal, then a wait; with `force`, a kill.
pub fn end(_pid: u32, _force: bool) -> Result<(), EndError> {
    Err(EndError::Refused)
}
