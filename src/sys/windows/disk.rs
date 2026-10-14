//! Disks: every fixed or removable drive letter with its space; read and write rates and busy
//! time from the counters of the physical disk it lives on (as on Linux, a drive's rates are
//! those of its whole disk). Handles stay open between readings; the list is made again only
//! when the drive letters change.

use super::calls::{self, Handle};
use crate::sys::{DiskSample, Root};

/// `GetDriveTypeW`: a removable drive (a card, a stick) and a fixed one.
const REMOVABLE: u32 = 2;
const FIXED: u32 = 3;

/// One disk's counters, read once a reading whatever number of drives live on it.
struct Device {
    handle: Handle,
    last: Option<[u64; 4]>,
    rates: (Option<u64>, Option<u64>, Option<f32>),
}

/// One drive letter.
struct Drive {
    root: Vec<u16>,
    device: Option<usize>,
}

/// Mounted disks: space and rates.
pub struct Disks {
    list: Vec<DiskSample>,
    drives: Vec<Drive>,
    devices: Vec<Device>,
    /// The drive letters the list was made from; 0 before the first reading.
    mask: u32,
}

impl Disks {
    pub fn open(_root: &Root) -> Disks {
        Disks {
            list: Vec::new(),
            drives: Vec::new(),
            devices: Vec::new(),
            mask: 0,
        }
    }

    pub fn read(&mut self) -> &[DiskSample] {
        let mask = calls::drive_mask();
        if mask != self.mask {
            if self.mask == 0 {
                calls::quiet_drive_errors();
            }
            self.mask = mask;
            self.rebuild(mask);
        }
        for d in &mut self.devices {
            let now = calls::disk_counters(&d.handle);
            d.rates = match (d.last, now) {
                (Some(a), Some(b)) => rates(a, b),
                _ => (None, None, None),
            };
            d.last = now;
        }
        for (drive, sample) in self.drives.iter().zip(self.list.iter_mut()) {
            match calls::space(&drive.root) {
                Some((total, free)) => {
                    sample.total = total;
                    sample.used = total.saturating_sub(free);
                }
                // Gone or without its medium: the list is made again next time.
                None => self.mask = u32::MAX,
            }
            let r = drive
                .device
                .and_then(|i| self.devices.get(i))
                .map_or((None, None, None), |d| d.rates);
            (sample.read_bps, sample.write_bps, sample.busy_pct) = r;
        }
        &self.list
    }

    /// Makes the list from the drive letters: fixed and removable drives that answer with
    /// their space, each tied to its disk's counters.
    fn rebuild(&mut self, mask: u32) {
        self.list.clear();
        self.drives.clear();
        let mut disks: Vec<(String, Option<usize>)> = Vec::new();
        let mut devices: Vec<Device> = Vec::new();
        for i in 0..26u8 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = char::from(b'A' + i);
            let mount = format!("{letter}:\\");
            let root = calls::wide(&mount);
            let kind = calls::drive_type(&root);
            if kind != FIXED && kind != REMOVABLE {
                continue;
            }
            let Some((total, free)) = calls::space(&root) else {
                continue;
            };
            let volume = calls::open_device(&calls::wide(&format!("\\\\.\\{letter}:")));
            let number = volume.as_ref().and_then(calls::disk_number);
            let dev = match number {
                Some(n) => format!("PhysicalDrive{n}"),
                None => format!("{letter}:"),
            };
            let device = match disks.iter().find(|(name, _)| *name == dev) {
                Some((_, known)) => *known,
                None => {
                    // The whole disk's counters; the volume's own if the disk does not answer.
                    let disk = number.and_then(|n| {
                        calls::open_device(&calls::wide(&format!("\\\\.\\PhysicalDrive{n}")))
                    });
                    let handle = [disk, volume]
                        .into_iter()
                        .flatten()
                        .find(|h| calls::disk_counters(h).is_some());
                    let index = handle.map(|handle| {
                        devices.push(Device {
                            handle,
                            last: None,
                            rates: (None, None, None),
                        });
                        devices.len() - 1
                    });
                    disks.push((dev.clone(), index));
                    index
                }
            };
            self.drives.push(Drive { root, device });
            self.list.push(DiskSample {
                mount,
                dev,
                total,
                used: total.saturating_sub(free),
                read_bps: None,
                write_bps: None,
                busy_pct: None,
            });
        }
        // Rates start again from the next pair of readings.
        self.devices = devices;
    }
}

/// Read and write bytes per second and the busy share between two readings of a disk's
/// counters (read, written, idle time, query time; times in 100 ns).
fn rates(a: [u64; 4], b: [u64; 4]) -> (Option<u64>, Option<u64>, Option<f32>) {
    let span = b[3].saturating_sub(a[3]);
    if span == 0 {
        return (None, None, None);
    }
    let seconds = span as f64 / 1e7;
    let per_second = |x: u64, y: u64| Some((y.saturating_sub(x) as f64 / seconds) as u64);
    let idle = b[2].saturating_sub(a[2]).min(span);
    let busy = ((span - idle) as f64 / span as f64 * 100.0).clamp(0.0, 100.0) as f32;
    (per_second(a[0], b[0]), per_second(a[1], b[1]), Some(busy))
}

/// Whether the disk counters answer for the drive Windows runs from (for `aether check`).
pub fn counters_answer() -> bool {
    let mut disks = Disks::open(&Root::system());
    disks.read();
    !disks.devices.is_empty()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_busy_from_two_readings() {
        // Half a second; 1 MB read, 2 MB written, idle a quarter of the time.
        let a = [0, 0, 0, 0];
        let b = [1_000_000, 2_000_000, 1_250_000, 5_000_000];
        assert_eq!(rates(a, b), (Some(2_000_000), Some(4_000_000), Some(75.0)));
        assert_eq!(rates(b, b), (None, None, None));
    }

    #[test]
    fn at_least_one_disk_has_space() {
        let mut disks = Disks::open(&Root::system());
        disks.read();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let list = disks.read();
        assert!(list.iter().any(|d| d.total > 0), "{list:?}");
        for d in list {
            assert!(d.used <= d.total);
            if let Some(b) = d.busy_pct {
                assert!((0.0..=100.0).contains(&b));
            }
        }
    }
}
