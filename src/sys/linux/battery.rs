//! The battery of a laptop (sys/class/power_supply).

use std::fs;
use std::path::Path;

use super::reader::PFile;
use super::sensors::number;
use crate::sys::{BatterySample, Root};

/// Power in µW from a current in µA and a voltage in µV.
pub fn power_uw(current_ua: u64, voltage_uv: u64) -> u64 {
    current_ua.saturating_mul(voltage_uv) / 1_000_000
}

/// The kernel's status text as the sample's word.
fn state_of(status: &str) -> &'static str {
    match status {
        "Charging" => "charging",
        "Discharging" => "discharging",
        "Full" => "full",
        "Not charging" => "not charging",
        _ => "unknown",
    }
}

/// The battery source, with its files kept open. `None` when there is no battery.
///
/// If a machine has more than one `BAT*`, the first by name is read.
pub struct Battery {
    capacity: Option<PFile>,
    status: Option<PFile>,
    power_now: Option<PFile>,
    current: Option<PFile>,
    voltage: Option<PFile>,
    energy_now: Option<PFile>,
    sample: Option<BatterySample>,
}

impl Battery {
    pub fn open(root: &Root) -> Battery {
        let base = root.path("sys/class/power_supply");
        let mut names: Vec<String> = fs::read_dir(&base)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with("BAT"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        match names.first() {
            Some(name) => Battery::at(&base.join(name)),
            None => Battery::none(),
        }
    }

    fn none() -> Battery {
        Battery {
            capacity: None,
            status: None,
            power_now: None,
            current: None,
            voltage: None,
            energy_now: None,
            sample: None,
        }
    }

    fn at(dir: &Path) -> Battery {
        let file = |name: &str| PFile::open(&dir.join(name), 64);
        Battery {
            capacity: file("capacity"),
            status: file("status"),
            power_now: file("power_now"),
            current: file("current_now"),
            voltage: file("voltage_now"),
            energy_now: file("energy_now"),
            sample: None,
        }
    }

    /// The battery now. `None` when there is no battery or its capacity cannot be read.
    pub fn read(&mut self) -> Option<&BatterySample> {
        let Some(pct) = self.capacity.as_mut().and_then(number) else {
            self.sample = None;
            return None;
        };
        let state = match self.status.as_mut() {
            Some(f) => state_of(std::str::from_utf8(f.read()).unwrap_or("").trim()),
            None => "unknown",
        };
        let uw = match self.power_now.as_mut().and_then(number) {
            Some(p) => Some(p),
            None => match (
                self.current.as_mut().and_then(number),
                self.voltage.as_mut().and_then(number),
            ) {
                (Some(c), Some(v)) => Some(power_uw(c, v)),
                _ => None,
            },
        };
        let energy = self.energy_now.as_mut().and_then(number);
        let minutes_left = match (state, energy, uw) {
            ("discharging", Some(e), Some(p)) if p > 0 => {
                u32::try_from(e.saturating_mul(60) / p).ok()
            }
            _ => None,
        };
        self.sample = Some(BatterySample {
            pct: pct as f32 / 100.0,
            state,
            power_w: uw.map(|p| p as f32 / 1_000_000.0),
            minutes_left,
        });
        self.sample.as_ref()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> Root {
        Root::at(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(rel),
        )
    }

    #[test]
    fn a_discharging_battery_gives_its_time_left() {
        let mut b = Battery::open(&fixture("sensors"));
        let s = b.read().cloned().expect("BAT0 is in the fixture");
        assert_eq!(s.pct, 0.8);
        assert_eq!(s.state, "discharging");
        assert_eq!(s.power_w, Some(12.0));
        assert_eq!(s.minutes_left, Some(240));
    }

    #[test]
    fn no_battery_gives_none() {
        let mut b = Battery::open(&fixture("none"));
        assert!(b.read().is_none());
    }

    #[test]
    fn current_and_voltage_give_power_and_state_words() {
        assert_eq!(power_uw(2_000_000, 12_000_000), 24_000_000);
        assert_eq!(state_of("Not charging"), "not charging");
        assert_eq!(state_of("Weird"), "unknown");
    }
}
