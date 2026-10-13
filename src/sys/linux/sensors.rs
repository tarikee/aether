//! Temperatures, fans and power from the hardware monitors (sys/class/hwmon).
//! The monitor helpers here are shared with the graphics cards, which carry their own monitor.

use std::fs;
use std::path::{Path, PathBuf};

use super::reader::{fields, num, PFile};
use crate::sys::{Root, Sensor, SensorKind};

/// Reads a small text file once, at open (never per tick). `None` if it cannot be read.
pub(crate) fn text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// The first number of a file, read in place. `None` if it is not a plain number.
pub(crate) fn number(file: &mut PFile) -> Option<u64> {
    let buf = file.read();
    fields(buf).next().and_then(num)
}

/// A number that may be negative, as a temperature can be.
fn signed(buf: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(buf).ok()?.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let n = i64::try_from(num(digits.as_bytes())?).ok()?;
    Some(if negative { -n } else { n })
}

/// The `hwmonN` folders inside `parent`, in number order (hwmon2 before hwmon10).
pub(crate) fn hwmon_dirs(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("hwmon"))
        })
        .collect();
    dirs.sort_by_key(|p| (p.as_os_str().len(), p.clone()));
    dirs
}

/// One input file of a hardware monitor, kept open: a temperature, a fan or a power reading.
pub(crate) struct Input {
    pub(crate) kind: SensorKind,
    pub(crate) n: u32,
    pub(crate) label: String,
    file: PFile,
}

impl Input {
    /// The value in its own unit: °C, RPM or watts. `None` if the file cannot be read now.
    pub(crate) fn value(&mut self) -> Option<f32> {
        let raw = signed(self.file.read())?;
        Some(match self.kind {
            SensorKind::Temp => raw as f32 / 1000.0,
            SensorKind::Fan => raw as f32,
            SensorKind::Power => raw as f32 / 1_000_000.0,
        })
    }
}

/// Takes the input of one kind and number out of a monitor's list.
pub(crate) fn take_input(inputs: &mut Vec<Input>, kind: SensorKind, n: u32) -> Option<Input> {
    let at = inputs.iter().position(|i| i.kind == kind && i.n == n)?;
    Some(inputs.swap_remove(at))
}

fn prefix(kind: SensorKind) -> &'static str {
    match kind {
        SensorKind::Temp => "temp",
        SensorKind::Fan => "fan",
        SensorKind::Power => "power",
    }
}

fn rank(kind: SensorKind) -> u8 {
    match kind {
        SensorKind::Temp => 0,
        SensorKind::Fan => 1,
        SensorKind::Power => 2,
    }
}

/// `tempN_input`, `fanN_input`, `powerN_average` or `powerN_input`: the kind, the number and
/// the suffix, for a file name.
fn parse_name(name: &str) -> Option<(SensorKind, u32, &str)> {
    let (kind, rest) = [SensorKind::Temp, SensorKind::Fan, SensorKind::Power]
        .into_iter()
        .find_map(|k| name.strip_prefix(prefix(k)).map(|r| (k, r)))?;
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    let (digits, suffix) = rest.split_at(end);
    Some((kind, digits.parse().ok()?, suffix))
}

/// One hardware monitor folder: its chip name and its inputs, kept open.
pub(crate) struct Monitor {
    pub(crate) chip: String,
    pub(crate) inputs: Vec<Input>,
}

impl Monitor {
    /// Opens the monitor in `dir`. Power uses the average where the chip has one.
    /// `None` if the folder cannot be read.
    pub(crate) fn open(dir: &Path) -> Option<Monitor> {
        let names: Vec<String> = fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        let chip = text(&dir.join("name")).unwrap_or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("hwmon")
                .to_string()
        });

        let mut found: Vec<(SensorKind, u32, String)> = names
            .iter()
            .filter_map(|name| {
                let (kind, n, suffix) = parse_name(name)?;
                let wanted = match kind {
                    SensorKind::Power => suffix == "_average" || suffix == "_input",
                    _ => suffix == "_input",
                };
                wanted.then(|| (kind, n, suffix.to_string()))
            })
            .collect();
        // Where a chip gives both an average and an instant power, the average is the one kept.
        let averaged: Vec<u32> = found
            .iter()
            .filter(|(k, _, s)| *k == SensorKind::Power && s == "_average")
            .map(|(_, n, _)| *n)
            .collect();
        found.retain(|(k, n, s)| {
            !(*k == SensorKind::Power && s == "_input" && averaged.contains(n))
        });
        found.sort_by_key(|(k, n, _)| (rank(*k), *n));

        let inputs = found
            .into_iter()
            .filter_map(|(kind, n, suffix)| {
                let file = PFile::open(&dir.join(format!("{}{n}{suffix}", prefix(kind))), 64)?;
                let label = text(&dir.join(format!("{}{n}_label", prefix(kind))))
                    .unwrap_or_else(|| format!("{}{n}", prefix(kind)));
                Some(Input {
                    kind,
                    n,
                    label,
                    file,
                })
            })
            .collect();
        Some(Monitor { chip, inputs })
    }
}

/// The sensor source, with its files kept open.
///
/// Each reading is one list entry, in the order of the monitors. An input whose file cannot be
/// read is dropped from the list at that tick; a sensor does not come back once dropped.
pub struct Sensors {
    inputs: Vec<Input>,
    list: Vec<Sensor>,
    keep: Vec<bool>,
}

impl Sensors {
    pub fn open(root: &Root) -> Sensors {
        let mut inputs = Vec::new();
        let mut list = Vec::new();
        for dir in hwmon_dirs(&root.path("sys/class/hwmon")) {
            let Some(monitor) = Monitor::open(&dir) else {
                continue;
            };
            for input in monitor.inputs {
                list.push(Sensor {
                    chip: monitor.chip.clone(),
                    label: input.label.clone(),
                    kind: input.kind,
                    value: 0.0,
                });
                inputs.push(input);
            }
        }
        Sensors {
            inputs,
            list,
            keep: Vec::new(),
        }
    }

    pub fn read(&mut self) -> &[Sensor] {
        self.keep.clear();
        for (input, sensor) in self.inputs.iter_mut().zip(self.list.iter_mut()) {
            match input.value() {
                Some(v) => {
                    sensor.value = v;
                    self.keep.push(true);
                }
                None => self.keep.push(false),
            }
        }
        if self.keep.iter().any(|k| !k) {
            let mut flags = self.keep.iter();
            self.inputs.retain(|_| *flags.next().unwrap_or(&false));
            let mut flags = self.keep.iter();
            self.list.retain(|_| *flags.next().unwrap_or(&false));
        }
        &self.list
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A fixture tree under `fixtures/`, read through a `Root`.
    fn fixture(rel: &str) -> Root {
        Root::at(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(rel),
        )
    }

    #[test]
    fn names_and_signed_numbers() {
        assert_eq!(
            parse_name("temp12_input"),
            Some((SensorKind::Temp, 12, "_input"))
        );
        assert_eq!(parse_name("powerX_input"), None);
        assert_eq!(parse_name("name"), None);
        assert_eq!(signed(b"-4000\n"), Some(-4000));
        assert_eq!(signed(b"48250\n"), Some(48250));
        assert_eq!(signed(b"x"), None);
    }

    #[test]
    fn reads_every_chip_in_order() {
        let mut s = Sensors::open(&fixture("sensors"));
        let rows: Vec<String> = s
            .read()
            .iter()
            .map(|x| format!("{} {} {:?} {:.2}", x.chip, x.label, x.kind, x.value))
            .collect();
        assert_eq!(
            rows,
            vec![
                "k10temp Tctl Temp 48.25",
                "k10temp temp2 Temp 47.00",
                "nct6798 CPU Fan Fan 812.00",
                "nct6798 fan2 Fan 0.00",
                "nct6798 Package Power 12.50",
                "nvme temp1 Temp 39.85",
                "testchip temp1 Temp -4.00",
            ]
        );
    }

    #[test]
    fn a_tree_without_monitors_gives_an_empty_list() {
        let mut s = Sensors::open(&fixture("none"));
        assert!(s.read().is_empty());
    }
}
