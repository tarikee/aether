//! The battery from the I/O registry (`AppleSmartBattery`): charge, whether it is charging,
//! the power flowing and, while it discharges, the minutes left. A Mac without a battery
//! gives none.

use super::calls::{self, SmartBattery};
use crate::sys::{BatterySample, Root};

/// The battery, when there is one.
pub struct Battery {
    node: Option<SmartBattery>,
    sample: Option<BatterySample>,
}

impl Battery {
    pub fn open(_root: &Root) -> Battery {
        Battery {
            node: calls::smart_battery(),
            sample: None,
        }
    }

    pub fn read(&mut self) -> Option<&BatterySample> {
        self.sample = self.node.as_ref().and_then(SmartBattery::read).map(|r| {
            let state = if r.charging {
                "charging"
            } else if r.full {
                "full"
            } else if r.external {
                "not charging"
            } else {
                "discharging"
            };
            BatterySample {
                pct: r.percent,
                state,
                power_w: r.watts,
                minutes_left: r.minutes.filter(|_| state == "discharging"),
            }
        });
        self.sample.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A desktop Mac has no battery: either way, what is read must be in range.
    #[test]
    fn a_battery_reads_a_percent_and_a_known_state() {
        let mut b = Battery::open(&Root::system());
        if let Some(s) = b.read() {
            assert!((0.0..=100.0).contains(&s.pct));
            assert!(["charging", "discharging", "full", "not charging"].contains(&s.state));
        }
    }
}
