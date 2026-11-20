//! Memory and swap (proc/meminfo and the memory pressure file).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::reader::{fields, lines, num, value_of, PFile};
use crate::sys::{MemSample, Root};

/// Room for `proc/meminfo`: the keys read here all sit in its first kilobyte.
const MEMINFO_CAP: usize = 1024;
const PRESSURE_CAP: usize = 256;
const KIB: u64 = 1024;
/// Room for the ZFS cache counters (about 8 KB on a busy machine).
const ARC_CAP: usize = 64 * 1024;

/// The memory source, with its files kept open.
pub struct Mem {
    meminfo: Option<PFile>,
    pressure: Option<PFile>,
    arc: Option<PFile>,
    sample: MemSample,
}

impl Mem {
    pub fn open(root: &Root) -> Mem {
        // The RAM's kind and speed do not change while the machine runs: read once, kept.
        let (ram_type, ram_mts) = ram(&root.path("run/udev/data/+dmi:id"));
        Mem {
            meminfo: PFile::open(&root.path("proc/meminfo"), MEMINFO_CAP),
            pressure: PFile::open(&root.path("proc/pressure/memory"), PRESSURE_CAP),
            arc: PFile::open(&root.path("proc/spl/kstat/zfs/arcstats"), ARC_CAP),
            sample: MemSample {
                ram_type,
                ram_mts,
                ..MemSample::default()
            },
        }
    }

    pub fn read(&mut self) -> &MemSample {
        if let Some(file) = self.meminfo.as_mut() {
            let buf = file.read();
            let s = &mut self.sample;
            s.total = kib(buf, "MemTotal");
            s.available = kib(buf, "MemAvailable");
            s.used = s.total.saturating_sub(s.available);
            s.cached = kib(buf, "Cached");
            s.swap_total = kib(buf, "SwapTotal");
            s.swap_used = s.swap_total.saturating_sub(kib(buf, "SwapFree"));
            // ZFS keeps its cache in memory: it counts as cache, not as used.
            if let Some(arc) = self.arc.as_mut().and_then(|f| arc_size(f.read())) {
                s.cached = s.cached.saturating_add(arc);
                s.used = s.used.saturating_sub(arc);
            }
        }
        self.sample.pressure = self.pressure.as_mut().and_then(read_pressure);
        &self.sample
    }
}

/// A `key: N kB` value in bytes; 0 when the key is missing.
fn kib(buf: &[u8], key: &str) -> u64 {
    value_of(buf, key).unwrap_or(0).saturating_mul(KIB)
}

/// The current size of the ZFS cache in bytes: the third field of its `size` line.
fn arc_size(buf: &[u8]) -> Option<u64> {
    let line = lines(buf).find(|l| fields(l).next() == Some(&b"size"[..]))?;
    fields(line).nth(2).and_then(num)
}

/// One DMI memory device, from its udev properties. Each is `None` when the line is missing.
#[derive(Default)]
struct Device {
    kind: Option<String>,
    speed: Option<u32>,
    configured: Option<u32>,
}

impl Device {
    /// Whether the device reports a speed: a configured one or, failing that, its maximum.
    fn has_speed(&self) -> bool {
        self.speed.or(self.configured).is_some_and(|m| m > 0)
    }
}

/// The RAM's kind and configured speed in MT/s, from the `E:MEMORY_DEVICE_<n>_...` lines of the
/// udev data file. The speed is the largest configured one, else the largest maximum. The kind
/// is that of the first device with a speed, skipping `Unknown`, `Other` and empty kinds.
fn ram(file: &Path) -> (Option<String>, Option<u32>) {
    let Ok(text) = fs::read_to_string(file) else {
        return (None, None);
    };
    let mut devices: BTreeMap<u32, Device> = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("E:MEMORY_DEVICE_") else {
            continue;
        };
        let Some((n, entry)) = rest.split_once('_') else {
            continue;
        };
        let (Ok(n), Some((key, value))) = (n.parse::<u32>(), entry.split_once('=')) else {
            continue;
        };
        let device = devices.entry(n).or_default();
        let value = value.trim();
        match key {
            "TYPE" => device.kind = Some(value.to_string()),
            "SPEED_MTS" => device.speed = value.parse().ok(),
            "CONFIGURED_SPEED_MTS" => device.configured = value.parse().ok(),
            _ => {}
        }
    }
    let largest = |pick: fn(&Device) -> Option<u32>| {
        devices.values().filter_map(pick).filter(|&m| m > 0).max()
    };
    let mts = largest(|d| d.configured).or_else(|| largest(|d| d.speed));
    let kind = devices
        .values()
        .filter(|d| d.has_speed())
        .filter_map(|d| d.kind.as_deref())
        .find(|k| !matches!(*k, "" | "Unknown" | "Other"))
        .map(str::to_string);
    (kind, mts)
}

/// The `avg10` of the `some` line: the share of the last ten seconds in which some task waited.
fn read_pressure(file: &mut PFile) -> Option<f32> {
    let buf = file.read();
    let some = lines(buf).find(|l| l.starts_with(b"some"))?;
    let value = fields(some).find_map(|f| f.strip_prefix(b"avg10="))?;
    std::str::from_utf8(value).ok()?.parse().ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn tree(rel: &str) -> Root {
        Root::at(format!(
            "{}/fixtures/basic/{rel}",
            env!("CARGO_MANIFEST_DIR")
        ))
    }

    #[test]
    fn memory_and_swap_in_bytes() {
        let mut mem = Mem::open(&tree(""));
        let s = mem.read();
        assert_eq!(s.total, 16_000_000 * KIB);
        assert_eq!(s.available, 8_000_000 * KIB);
        assert_eq!(s.used, 8_000_000 * KIB);
        assert_eq!(s.cached, 4_000_000 * KIB);
        assert_eq!(s.swap_total, 2_000_000 * KIB);
        assert_eq!(s.swap_used, 500_000 * KIB);
        assert_eq!(s.pressure, Some(1.25));
    }

    #[test]
    fn zfs_cache_counts_as_cache_not_as_used() {
        let root = Root::at(format!("{}/fixtures/zfs", env!("CARGO_MANIFEST_DIR")));
        let s = Mem::open(&root).read().clone();
        assert_eq!(s.cached, 4_000_000 * KIB + 1_073_741_824);
        assert_eq!(s.used, 8_000_000 * KIB - 1_073_741_824);
    }

    #[test]
    fn the_ram_kind_and_speed_come_from_the_dmi_devices_and_survive_reads() {
        let base = std::env::temp_dir().join(format!("aether-ram-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let file = base.join("run/udev/data/+dmi:id");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "I:1\n\
             E:MEMORY_DEVICE_0_TYPE=Unknown\n\
             E:MEMORY_DEVICE_1_TYPE=DDR4\n\
             E:MEMORY_DEVICE_1_SPEED_MTS=2666\n\
             E:MEMORY_DEVICE_1_CONFIGURED_SPEED_MTS=2666\n\
             E:MEMORY_DEVICE_3_TYPE=DDR4\n\
             E:MEMORY_DEVICE_3_SPEED_MTS=2666\n",
        )
        .unwrap();
        let mut mem = Mem::open(&Root::at(&base));
        let s = mem.read();
        assert_eq!(s.ram_type.as_deref(), Some("DDR4"));
        assert_eq!(s.ram_mts, Some(2666));
        assert_eq!(s.total, 0);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_dmi_file_gives_no_ram_speed() {
        let mut mem = Mem::open(&tree("none"));
        let s = mem.read();
        assert_eq!((s.ram_type.clone(), s.ram_mts), (None, None));
    }

    #[test]
    fn a_missing_tree_reads_as_nothing() {
        let mut mem = Mem::open(&tree("none"));
        let s = mem.read();
        assert_eq!(*s, MemSample::default());
    }
}
