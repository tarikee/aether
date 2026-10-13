//! Mounted disks: space by statvfs and read/write rates from proc/diskstats.

use crate::sys::{DiskSample, Root};

/// The disk source, with its files kept open.
pub struct Disks {
    list: Vec<DiskSample>,
}

impl Disks {
    pub fn open(_root: &Root) -> Disks {
        Disks { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[DiskSample] {
        &self.list
    }
}
