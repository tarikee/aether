//! Network interfaces: rates from proc/net/dev.

use crate::sys::{NetSample, Root};

/// The network source, with its files kept open.
pub struct Nets {
    list: Vec<NetSample>,
}

impl Nets {
    pub fn open(_root: &Root) -> Nets {
        Nets { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[NetSample] {
        &self.list
    }
}
