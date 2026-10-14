//! Intel graphics cards (sys/class/drm), driven by the i915 or the xe driver.

use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::sys::linux::reader::PFile;
use crate::sys::linux::sensors::{hwmon_dirs, number, take_input, text, Input, Monitor};
use crate::sys::{GpuSample, SensorKind};

/// The drivers whose cards are read here.
const DRIVERS: &[&str] = &["i915", "xe"];

/// One Intel card, with its files kept open.
pub struct Card {
    name: String,
    vram_total: Option<PFile>,
    temp: Option<Input>,
    power: Option<Input>,
    energy: Option<PFile>,
    energy_last: Option<(u64, Instant)>,
}

/// Opens the card whose `device` folder is `dev`, if its driver is i915 or xe.
pub fn open(dev: &Path) -> Option<Card> {
    let driver = fs::read_link(dev.join("driver")).ok()?;
    let driver = driver.file_name()?.to_str()?;
    if !DRIVERS.contains(&driver) {
        return None;
    }
    let name = match text(&dev.join("device")) {
        Some(id) => format!("Intel GPU {id}"),
        None => "Intel GPU".to_string(),
    };

    let (mut temp, mut power, mut energy) = (None, None, None);
    for dir in hwmon_dirs(&dev.join("hwmon")) {
        if let Some(mut monitor) = Monitor::open(&dir) {
            temp = temp.or_else(|| take_input(&mut monitor.inputs, SensorKind::Temp, 1));
            power = power.or_else(|| take_input(&mut monitor.inputs, SensorKind::Power, 1));
        }
        energy = energy.or_else(|| PFile::open(&dir.join("energy1_input"), 64));
    }

    // The memory of a card that has its own: i915's file, else xe's tile 0 VRAM size. Integrated
    // cards have neither and share system memory.
    let vram_total = PFile::open(&dev.join("lmem_total_bytes"), 64)
        .or_else(|| PFile::open(&dev.join("tile0/vram0/size"), 64));
    Some(Card {
        name,
        vram_total,
        temp,
        power,
        energy,
        energy_last: None,
    })
}

impl Card {
    /// The card before its first reading: name and vendor, every value unknown.
    pub fn blank(&self) -> GpuSample {
        GpuSample {
            name: self.name.clone(),
            vendor: "intel",
            pct: None,
            temp_c: None,
            power_w: None,
            vram_used: None,
            vram_total: None,
            fan_rpm: None,
        }
    }

    /// Fills the card's values in place.
    pub fn read(&mut self, out: &mut GpuSample) {
        self.read_at(out, Instant::now());
    }

    fn read_at(&mut self, out: &mut GpuSample, now: Instant) {
        // The card's use is not read yet: the process scan gives each process's share, and the
        // card's share is not summed from them.
        out.pct = None;
        out.vram_total = self.vram_total.as_mut().and_then(number);
        out.temp_c = self.temp.as_mut().and_then(Input::value);
        out.power_w = self
            .power
            .as_mut()
            .and_then(Input::value)
            .or_else(|| self.energy_watts(now));
    }

    /// The power from the energy counter (µJ), as its rate since the last reading. `None` on the
    /// first reading, when the counter went back, or without the file.
    fn energy_watts(&mut self, now: Instant) -> Option<f32> {
        let uj = self.energy.as_mut().and_then(number)?;
        let (before, then) = self.energy_last.replace((uj, now))?;
        let secs = now.checked_duration_since(then)?.as_secs_f64();
        let used = uj.checked_sub(before)?;
        (secs > 0.0).then(|| (used as f64 / 1e6 / secs) as f32)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::sys::Root;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::time::Duration;

    /// A made-up sysfs tree with one card: its driver link names `driver`. The files are the
    /// `(path below device, text)` pairs given.
    fn tree(name: &str, driver: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("aether-intel-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dev = root.join("sys/class/drm/card0/device");
        fs::create_dir_all(&dev).unwrap();
        symlink(
            format!("../../../bus/pci/drivers/{driver}"),
            dev.join("driver"),
        )
        .unwrap();
        for (rel, text) in files {
            let path = dev.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        root
    }

    fn dev_of(root: &Path) -> PathBuf {
        root.join("sys/class/drm/card0/device")
    }

    #[test]
    fn an_i915_card_reads_temperature_memory_and_energy_as_power() {
        let root = tree(
            "i915",
            "i915",
            &[
                ("device", "0x56a0\n"),
                ("lmem_total_bytes", "8589934592\n"),
                ("hwmon/hwmon3/temp1_input", "54000\n"),
                ("hwmon/hwmon3/energy1_input", "1000000\n"),
            ],
        );
        let dev = dev_of(&root);
        let mut card = open(&dev).expect("an i915 card");
        let mut out = card.blank();
        assert_eq!(out.name, "Intel GPU 0x56a0");
        assert_eq!(out.vendor, "intel");

        let t0 = Instant::now();
        card.read_at(&mut out, t0);
        assert_eq!(out.temp_c, Some(54.0));
        assert_eq!(out.vram_total, Some(8_589_934_592));
        assert_eq!((out.pct, out.vram_used, out.power_w), (None, None, None));

        fs::write(dev.join("hwmon/hwmon3/energy1_input"), "3000000\n").unwrap();
        card.read_at(&mut out, t0 + Duration::from_secs(2));
        assert_eq!(out.power_w, Some(1.0));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_xe_card_reads_its_vram_size_and_hwmon_power_in_microwatts() {
        let root = tree(
            "xe",
            "xe",
            &[
                ("device", "0xe20b\n"),
                ("tile0/vram0/size", "4294967296\n"),
                ("hwmon/hwmon1/power1_input", "12500000\n"),
            ],
        );
        let mut card = open(&dev_of(&root)).expect("an xe card");
        let mut out = card.blank();
        card.read_at(&mut out, Instant::now());
        assert_eq!(out.name, "Intel GPU 0xe20b");
        assert_eq!(out.vram_total, Some(4_294_967_296));
        assert_eq!(out.power_w, Some(12.5));
        assert_eq!(out.temp_c, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_files_give_unknown_values_and_the_name_falls_back() {
        let root = tree("bare", "i915", &[]);
        let mut card = open(&dev_of(&root)).expect("an i915 card");
        let mut out = card.blank();
        card.read_at(&mut out, Instant::now());
        assert_eq!(out.name, "Intel GPU");
        assert_eq!(
            (
                out.pct,
                out.temp_c,
                out.power_w,
                out.vram_total,
                out.fan_rpm
            ),
            (None, None, None, None, None)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn other_drivers_are_not_intel_cards_and_the_scan_finds_the_intel_one() {
        let amd = tree("amd", "amdgpu", &[("device", "0x73bf\n")]);
        assert!(open(&dev_of(&amd)).is_none());
        let _ = fs::remove_dir_all(&amd);

        let root = tree("found", "xe", &[("device", "0xe20b\n")]);
        let list = super::super::Gpus::open(&Root::at(&root))
            .read(None)
            .to_vec();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].vendor, "intel");
        let _ = fs::remove_dir_all(&root);
    }
}
