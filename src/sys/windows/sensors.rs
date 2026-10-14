//! Sensors: Windows offers temperatures, fans and power only through a vendor's driver or as
//! an administrator, so none are read. The list stays empty; nothing is made up.

use crate::sys::{Root, Sensor};

/// Temperatures, fans and power: none on Windows.
pub struct Sensors {
    list: Vec<Sensor>,
}

impl Sensors {
    pub fn open(_root: &Root) -> Sensors {
        Sensors { list: Vec::new() }
    }

    pub fn read(&mut self) -> &[Sensor] {
        &self.list
    }
}
