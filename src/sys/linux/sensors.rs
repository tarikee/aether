//! Temperatures, fans and power from the hardware monitors (sys/class/hwmon).

use crate::sys::{Root, Sensor};

/// The sensor source, with its files kept open.
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
