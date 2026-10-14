//! Temperatures from the HID thermometers of Apple silicon, which need no root. Fans and power
//! are not read here. A reading at or below 0 degrees is a sensor with nothing attached. Each
//! sensor costs an exchange with the system (about 85 microseconds), so the temperatures are read
//! on every fourth reading and are never more than four seconds old. A sensor that stops answering makes the list be made again.

use super::calls::Thermometers;
use crate::sys::{Root, Sensor, SensorKind};

/// Temperatures with their names.
pub struct Sensors {
    thermo: Option<Thermometers>,
    ids: Vec<usize>,
    list: Vec<Sensor>,
    dirty: bool,
    ticks: u32,
}

impl Sensors {
    pub fn open(_root: &Root) -> Sensors {
        let mut s = Sensors {
            thermo: None,
            ids: Vec::new(),
            list: Vec::new(),
            dirty: true,
            ticks: 0,
        };
        s.build();
        s
    }

    /// Finds the sensors that answer now.
    fn build(&mut self) {
        self.dirty = false;
        self.thermo = Thermometers::open();
        self.ids.clear();
        self.list.clear();
        let Some(t) = &self.thermo else {
            return;
        };
        let mut order: Vec<usize> = (0..t.names.len()).collect();
        order.sort_by(|&a, &b| t.names[a].cmp(&t.names[b]));
        for i in order {
            let name = &t.names[i];
            let Some(value) = t.celsius(i).filter(|_| !name.is_empty()) else {
                continue;
            };
            self.ids.push(i);
            self.list.push(Sensor {
                chip: "apple".to_string(),
                label: name.clone(),
                kind: SensorKind::Temp,
                value,
            });
        }
    }

    pub fn read(&mut self) -> &[Sensor] {
        self.ticks = self.ticks.wrapping_add(1);
        if self.dirty {
            self.build();
        } else if self.ticks % 4 != 1 {
            return &self.list;
        }
        if let Some(t) = &self.thermo {
            for (&i, s) in self.ids.iter().zip(&mut self.list) {
                match t.celsius(i) {
                    Some(v) => s.value = v,
                    None => self.dirty = true,
                }
            }
        }
        &self.list
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn what_is_read_is_a_sane_temperature() {
        let mut s = Sensors::open(&Root::system());
        for t in s.read() {
            assert!(t.value > 0.0 && t.value < 150.0, "{} {}", t.label, t.value);
            assert_eq!(t.kind, SensorKind::Temp);
        }
    }
}
