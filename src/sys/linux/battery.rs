//! The battery of a laptop (sys/class/power_supply).

use crate::sys::{BatterySample, Root};

/// The battery source, with its files kept open. `None` when there is no battery.
pub struct Battery {
    sample: Option<BatterySample>,
}

impl Battery {
    pub fn open(_root: &Root) -> Battery {
        Battery { sample: None }
    }

    pub fn read(&mut self) -> Option<&BatterySample> {
        self.sample.as_ref()
    }
}
