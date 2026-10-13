//! AMD graphics cards (sys/class/drm, vendor 0x1002).

use std::fs;
use std::path::Path;

use crate::sys::linux::reader::PFile;
use crate::sys::linux::sensors::{hwmon_dirs, number, take_input, text, Input, Monitor};
use crate::sys::{GpuSample, Root, SensorKind};

/// The PCI vendor id the `vendor` file of an AMD card holds.
const AMD: &str = "0x1002";

/// One AMD card, with its files kept open.
pub struct Card {
    name: String,
    busy: Option<PFile>,
    vram_used: Option<PFile>,
    vram_total: Option<PFile>,
    temp: Option<Input>,
    fan: Option<Input>,
    power: Option<Input>,
}

/// Opens the card whose `device` folder is `dev`, if it is an AMD card.
pub fn open(root: &Root, dev: &Path) -> Option<Card> {
    if text(&dev.join("vendor"))? != AMD {
        return None;
    }
    let name = text(&dev.join("device"))
        .and_then(|id| device_name(root, &id))
        .unwrap_or_else(|| "AMD GPU".to_string());

    let (mut temp, mut fan, mut power) = (None, None, None);
    if let Some(mut monitor) = hwmon_dirs(&dev.join("hwmon"))
        .iter()
        .find_map(|d| Monitor::open(d))
    {
        temp = take_input(&mut monitor.inputs, SensorKind::Temp, 1);
        fan = take_input(&mut monitor.inputs, SensorKind::Fan, 1);
        power = take_input(&mut monitor.inputs, SensorKind::Power, 1);
    }

    let file = |n: &str| PFile::open(&dev.join(n), 64);
    Some(Card {
        name,
        busy: file("gpu_busy_percent"),
        vram_used: file("mem_info_vram_used"),
        vram_total: file("mem_info_vram_total"),
        temp,
        fan,
        power,
    })
}

/// The name of a device in `usr/share/hwdata/pci.ids`, under the AMD vendor. `id` is as the
/// `device` file has it, such as `0x73bf`.
fn device_name(root: &Root, id: &str) -> Option<String> {
    let want = id.trim().trim_start_matches("0x").to_ascii_lowercase();
    if want.is_empty() {
        return None;
    }
    // Read line by line: the file is large and only the one name is kept.
    let ids = std::io::BufReader::new(fs::File::open(root.path("usr/share/hwdata/pci.ids")).ok()?);
    let mut in_amd = false;
    for line in std::io::BufRead::lines(ids) {
        let Ok(line) = line else {
            return None;
        };
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.strip_prefix('\t') {
            // A device line; a subsystem line (two tabs) is not one.
            Some(device) if !device.starts_with('\t') => {
                if let Some((dev, name)) = device.split_once("  ") {
                    if in_amd && dev == want {
                        return Some(name.trim().to_string());
                    }
                }
            }
            Some(_) => {}
            // A vendor line, or a class section, which ends the vendor before it.
            None => in_amd = line.starts_with("1002  "),
        }
    }
    None
}

impl Card {
    /// The card before its first reading: name and vendor, every value unknown.
    pub fn blank(&self) -> GpuSample {
        GpuSample {
            name: self.name.clone(),
            vendor: "amd",
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
        out.pct = self.busy.as_mut().and_then(number).map(|p| p as f32);
        out.vram_used = self.vram_used.as_mut().and_then(number);
        out.vram_total = self.vram_total.as_mut().and_then(number);
        out.temp_c = self.temp.as_mut().and_then(Input::value);
        out.power_w = self.power.as_mut().and_then(Input::value);
        out.fan_rpm = self.fan.as_mut().and_then(Input::value).map(|r| r as u32);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_device_name_comes_from_the_amd_section_only() {
        let root = Root::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sensors"));
        assert_eq!(
            device_name(&root, "0x73bf").as_deref(),
            Some("Example GPU 73bf")
        );
        assert_eq!(device_name(&root, "0x1234"), None);
        assert_eq!(device_name(&root, ""), None);
    }
}
