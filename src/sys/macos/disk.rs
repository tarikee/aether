//! Disks: the mounted volumes on real disks, with their space. Volumes of one APFS container
//! share its space, so a container is listed once, under its shortest mount point. Read and
//! write rates are those of the block storage driver of the whole disk the volume lives on (an
//! APFS container is found below the driver of the drive that holds it). Busy time is not
//! given by macOS: `None`.

use super::calls::{self, BlockDriver, Mount};
use crate::sys::{DiskSample, Root};
use std::time::Instant;

/// Mounted disks: space and rates.
pub struct Disks {
    list: Vec<DiskSample>,
    mounts: Vec<Mount>,
    drivers: Vec<BlockDriver>,
    /// For each listed disk, the driver whose counters it uses.
    driver_of: Vec<Option<usize>>,
    /// Each driver's bytes (read, written) at the last reading.
    last: Vec<Option<(u64, u64)>>,
    at: Option<Instant>,
}

impl Disks {
    pub fn open(_root: &Root) -> Disks {
        Disks {
            list: Vec::new(),
            mounts: Vec::new(),
            drivers: calls::block_drivers(),
            driver_of: Vec::new(),
            last: Vec::new(),
            at: None,
        }
    }

    pub fn read(&mut self) -> &[DiskSample] {
        if !calls::mounts(&mut self.mounts) {
            return &self.list;
        }
        let keep = select(&self.mounts);
        let same = keep.len() == self.list.len()
            && keep
                .iter()
                .zip(&self.list)
                .all(|(&i, s)| self.mounts[i].on == s.mount);
        if !same {
            self.list = keep
                .iter()
                .map(|&i| DiskSample {
                    mount: self.mounts[i].on.clone(),
                    dev: self.mounts[i].from.trim_start_matches("/dev/").to_string(),
                    total: 0,
                    used: 0,
                    read_bps: None,
                    write_bps: None,
                    busy_pct: None,
                })
                .collect();
            self.drivers = calls::block_drivers();
            self.last.clear();
            self.driver_of = self
                .list
                .iter()
                .map(|s| {
                    let w = whole(&s.dev);
                    self.drivers
                        .iter()
                        .position(|d| d.whole.iter().any(|n| n == w))
                })
                .collect();
        }
        for (&i, s) in keep.iter().zip(self.list.iter_mut()) {
            s.total = self.mounts[i].total;
            s.used = self.mounts[i].used;
        }
        let now = Instant::now();
        let secs = self
            .at
            .map(|t| now.duration_since(t).as_secs_f64())
            .filter(|s| *s > 0.0);
        self.at = Some(now);
        let had = self.last.len() == self.drivers.len();
        if !had {
            self.last.clear();
            self.last.resize(self.drivers.len(), None);
        }
        for (s, d) in self.list.iter_mut().zip(&self.driver_of) {
            let r = d.and_then(|d| {
                let now = self.drivers.get(d)?.bytes();
                rates(if had { self.last[d] } else { None }, now, secs)
            });
            s.read_bps = r.map(|r| r.0);
            s.write_bps = r.map(|r| r.1);
        }
        for (l, d) in self.last.iter_mut().zip(&self.drivers) {
            *l = d.bytes();
        }
        &self.list
    }
}

/// Bytes per second read and written between two counters; `None` without both or a time.
fn rates(
    before: Option<(u64, u64)>,
    now: Option<(u64, u64)>,
    secs: Option<f64>,
) -> Option<(u64, u64)> {
    let (a, b, s) = (before?, now?, secs?);
    Some((
        (b.0.saturating_sub(a.0) as f64 / s) as u64,
        (b.1.saturating_sub(a.1) as f64 / s) as u64,
    ))
}

/// The part of a device name that says which physical or synthesized disk it is on: `disk3`
/// for `disk3s1s1`.
fn whole(dev: &str) -> &str {
    let digits = dev
        .strip_prefix("disk")
        .map_or(0, |r| r.bytes().take_while(u8::is_ascii_digit).count());
    if digits == 0 {
        dev
    } else {
        &dev[..4 + digits]
    }
}

/// The mounts to list, as indexes: local, browsable volumes of `/dev/disk*` with some space, one per
/// APFS container or per other device (its shortest mount point), in mount order.
fn select(mounts: &[Mount]) -> Vec<usize> {
    let key = |m: &Mount| -> String {
        let dev = m.from.trim_start_matches("/dev/");
        if m.fstype == "apfs" {
            whole(dev).to_string()
        } else {
            dev.to_string()
        }
    };
    let mut out: Vec<usize> = Vec::new();
    for (i, m) in mounts.iter().enumerate() {
        if !m.local || !m.browse || !m.from.starts_with("/dev/disk") || m.total == 0 {
            continue;
        }
        let k = key(m);
        match out.iter().position(|&j| key(&mounts[j]) == k) {
            Some(p) => {
                if m.on.len() < mounts[out[p]].on.len() {
                    out[p] = i;
                }
            }
            None => out.push(i),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn m(from: &str, on: &str, fstype: &str, local: bool) -> Mount {
        Mount {
            from: from.into(),
            on: on.into(),
            fstype: fstype.into(),
            total: 100,
            used: 40,
            local,
            browse: true,
        }
    }

    #[test]
    fn a_container_is_listed_once_and_pseudo_file_systems_not_at_all() {
        let mounts = [
            m("/dev/disk3s1s1", "/", "apfs", true),
            m("devfs", "/dev", "devfs", false),
            m("/dev/disk3s6", "/System/Volumes/VM", "apfs", true),
            m("/dev/disk3s5", "/System/Volumes/Data", "apfs", true),
            m(
                "map auto_home",
                "/System/Volumes/Data/home",
                "autofs",
                false,
            ),
            m("/dev/disk4s1", "/Volumes/Stick", "msdos", true),
            m("/dev/disk4s2", "/Volumes/Other", "exfat", true),
            m("//u@server/share", "/Volumes/share", "smbfs", false),
        ];
        let keep = select(&mounts);
        let on: Vec<&str> = keep.iter().map(|&i| mounts[i].on.as_str()).collect();
        assert_eq!(on, ["/", "/Volumes/Stick", "/Volumes/Other"]);
    }

    #[test]
    fn rates_need_both_counters_and_a_time() {
        assert_eq!(rates(None, Some((1, 1)), Some(1.0)), None);
        assert_eq!(rates(Some((0, 0)), Some((1, 1)), None), None);
        assert_eq!(
            rates(Some((100, 0)), Some((300, 50)), Some(2.0)),
            Some((100, 25))
        );
    }

    #[test]
    fn whole_names() {
        assert_eq!(whole("disk3s1s1"), "disk3");
        assert_eq!(whole("disk12"), "disk12");
        assert_eq!(whole("other"), "other");
    }

    #[test]
    fn at_least_one_disk_with_space() {
        let mut d = Disks::open(&Root::system());
        let list = d.read();
        assert!(!list.is_empty());
        assert!(list.iter().all(|s| s.total > 0 && s.used <= s.total));
    }
}
