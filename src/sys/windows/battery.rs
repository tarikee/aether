//! The battery, from the system power status. Its power draw is not in that status: `None`.

use super::calls;
use crate::sys::{BatterySample, Root};

/// Battery flags: charging, no system battery, unknown.
const CHARGING: u8 = 8;
const NO_BATTERY: u8 = 128;
const UNKNOWN: u8 = 255;

/// The battery, when there is one.
pub struct Battery {
    sample: Option<BatterySample>,
}

impl Battery {
    pub fn open(_root: &Root) -> Battery {
        Battery { sample: None }
    }

    pub fn read(&mut self) -> Option<&BatterySample> {
        self.sample = calls::power_status().and_then(sample);
        self.sample.as_ref()
    }
}

/// A battery sample from (on mains, flags, percent, seconds left); `None` without a battery.
fn sample((ac, flags, pct, seconds): (u8, u8, u8, u32)) -> Option<BatterySample> {
    if flags == UNKNOWN || flags & NO_BATTERY != 0 || pct > 100 {
        return None;
    }
    let state = if flags & CHARGING != 0 {
        "charging"
    } else if ac == 0 {
        "discharging"
    } else if ac == 1 && pct == 100 {
        "full"
    } else if ac == 1 {
        "not charging"
    } else {
        "unknown"
    };
    let minutes_left = (state == "discharging" && seconds != u32::MAX).then_some(seconds / 60);
    Some(BatterySample {
        pct: f32::from(pct),
        state,
        power_w: None,
        minutes_left,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn states_from_the_power_status() {
        assert!(sample((1, NO_BATTERY, 255, u32::MAX)).is_none());
        assert!(sample((1, UNKNOWN, 50, u32::MAX)).is_none());
        let s = sample((0, 1, 80, 5400)).unwrap();
        assert_eq!(
            (s.pct, s.state, s.minutes_left),
            (80.0, "discharging", Some(90))
        );
        assert_eq!(
            sample((1, CHARGING, 40, u32::MAX)).unwrap().state,
            "charging"
        );
        assert_eq!(sample((1, 1, 100, u32::MAX)).unwrap().state, "full");
        assert_eq!(sample((0, 2, 30, u32::MAX)).unwrap().minutes_left, None);
    }

    #[test]
    fn reading_does_not_fail() {
        let mut b = Battery::open(&Root::system());
        if let Some(s) = b.read() {
            assert!((0.0..=100.0).contains(&s.pct));
        }
    }
}
