//! Mounted disks: space by statvfs and read/write rates from proc/diskstats.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::calls;
use super::reader::{fields, lines, num, PFile};
use crate::sys::{DiskSample, Root};

const MOUNTS_CAP: usize = 64 * 1024;
const STATS_CAP: usize = 64 * 1024;
/// Block devices that are not disks: loop, ram, compressed ram, device mapper, optical.
const NOT_DISKS: [&str; 5] = ["loop", "ram", "zram", "dm-", "sr"];

/// A whole block device. Counters are bytes (sectors × 512).
struct Whole {
    name: String,
    prev: Option<(u64, u64)>,
    cur: Option<(u64, u64)>,
    rate: Option<(u64, u64)>,
    /// The I/O time in ms at the last reading and now, and the busy percent between them.
    busy_prev: Option<u64>,
    busy_cur: Option<u64>,
    busy: Option<f32>,
}

/// The disk source, with its files kept open.
pub struct Disks {
    mounts: Option<PFile>,
    stats: Option<PFile>,
    block: PathBuf,
    wholes: Vec<Whole>,
    seen: Vec<u8>,
    list: Vec<DiskSample>,
    /// `paths[i]` is the mount of `list[i]`, as a C string made once.
    paths: Vec<CString>,
    last: Option<Instant>,
}

impl Disks {
    pub fn open(root: &Root) -> Disks {
        let block = root.path("sys/block");
        let mut wholes = Vec::new();
        scan_block(&block, &mut wholes);
        let mut disks = Disks {
            mounts: PFile::open(&root.path("proc/mounts"), MOUNTS_CAP),
            stats: PFile::open(&root.path("proc/diskstats"), STATS_CAP),
            block,
            wholes,
            seen: Vec::new(),
            list: Vec::new(),
            paths: Vec::new(),
            last: None,
        };
        // The mounts are learned here; a tick only updates numbers.
        disks.sync_mounts();
        disks
    }

    pub fn read(&mut self) -> &[DiskSample] {
        let now = Instant::now();
        let secs = self.last.map(|t| now.duration_since(t).as_secs_f64());
        self.last = Some(now);
        self.sync_mounts();

        for w in &mut self.wholes {
            w.cur = None;
            w.busy_cur = None;
        }
        if let Some(f) = self.stats.as_mut() {
            for line in lines(f.read()) {
                let mut it = fields(line);
                let Some(name) = it.nth(2) else { continue };
                // Fields 6 and 10 (sectors read and written), counted from 1.
                let (Some(read), Some(written)) = (it.nth(2), it.nth(3)) else {
                    continue;
                };
                // Field 13 (ms spent doing I/O), two fields after the one just taken.
                let busy = it.nth(2).and_then(num);
                let (Some(read), Some(written)) = (num(read), num(written)) else {
                    continue;
                };
                if let Some(w) = self.wholes.iter_mut().find(|w| w.name.as_bytes() == name) {
                    w.cur = Some((read.saturating_mul(512), written.saturating_mul(512)));
                    w.busy_cur = busy;
                }
            }
        }
        for w in &mut self.wholes {
            w.rate = rate(w.prev, w.cur, secs);
            w.prev = w.cur;
            w.busy = busy_pct(w.busy_prev, w.busy_cur, secs);
            w.busy_prev = w.busy_cur;
        }

        // A mount whose space cannot be read is left out, never shown as 0.
        let mut i = 0;
        while i < self.list.len() {
            match calls::space(&self.paths[i]) {
                Some((total, used)) => {
                    self.list[i].total = total;
                    self.list[i].used = used;
                    i += 1;
                }
                None => {
                    self.list.remove(i);
                    self.paths.remove(i);
                }
            }
        }
        for d in &mut self.list {
            let r = whole_for(&d.dev, &self.wholes).and_then(|j| self.wholes[j].rate);
            d.read_bps = r.map(|p| p.0);
            d.write_bps = r.map(|p| p.1);
            d.busy_pct = whole_for(&d.dev, &self.wholes).and_then(|j| self.wholes[j].busy);
        }
        &self.list
    }

    /// Parses the mount table again only when its bytes change. A changed table rebuilds the
    /// list, and reads the block devices again so a new disk is known with its mount.
    fn sync_mounts(&mut self) {
        let Some(f) = self.mounts.as_mut() else {
            return;
        };
        let buf = f.read();
        if buf == self.seen.as_slice() {
            return;
        }
        self.seen.clear();
        self.seen.extend_from_slice(buf);
        scan_block(&self.block, &mut self.wholes);
        let mut list = Vec::new();
        let mut paths = Vec::new();
        for (dev, mount) in mounts_from(&self.seen) {
            let Ok(path) = CString::new(mount.as_bytes()) else {
                continue;
            };
            paths.push(path);
            list.push(DiskSample {
                mount,
                dev,
                total: 0,
                used: 0,
                read_bps: None,
                write_bps: None,
                busy_pct: None,
            });
        }
        self.list = list;
        self.paths = paths;
    }
}

/// Adds every block device in `dir` that is a disk and not yet known, sorted by name.
fn scan_block(dir: &Path, wholes: &mut Vec<Whole>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut added = false;
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if NOT_DISKS.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        if !wholes.iter().any(|w| w.name == name) {
            wholes.push(Whole {
                name,
                prev: None,
                cur: None,
                rate: None,
                busy_prev: None,
                busy_cur: None,
                busy: None,
            });
            added = true;
        }
    }
    if added {
        wholes.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

/// The disks in a mount table, as (device, mount): only `/dev/` devices other than loop
/// devices, never under `/boot`, one line per device (its shortest mount).
pub(crate) fn mounts_from(buf: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in lines(buf) {
        let mut it = fields(line);
        let (Some(dev), Some(mnt)) = (it.next(), it.next()) else {
            continue;
        };
        // ZFS datasets are named without `/dev/`; their type says they are kept too.
        let zfs = it.next() == Some(&b"zfs"[..]);
        let dev = unescape(dev);
        if !(zfs || (dev.starts_with("/dev/") && !dev.starts_with("/dev/loop"))) {
            continue;
        }
        let mount = unescape(mnt);
        if mount.starts_with("/boot") {
            continue;
        }
        match out.iter_mut().find(|(d, _)| *d == dev) {
            Some(entry) => {
                if mount.len() < entry.1.len() {
                    entry.1 = mount;
                }
            }
            None => out.push((dev, mount)),
        }
    }
    out
}

/// The kernel writes spaces and other odd bytes as `\ooo` octal escapes.
fn unescape(field: &[u8]) -> String {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' && i + 3 < field.len() {
            let value = field[i + 1..i + 4]
                .iter()
                .try_fold(0u32, |acc, &b| match b {
                    b'0'..=b'7' => Some(acc * 8 + u32::from(b - b'0')),
                    _ => None,
                });
            if let Some(byte) = value.and_then(|v| u8::try_from(v).ok()) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(field[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The whole device a partition belongs to: the longest device name that starts the
/// partition's name (`nvme0n1` for `/dev/nvme0n1p2`). `None` for other paths.
fn whole_for(dev: &str, wholes: &[Whole]) -> Option<usize> {
    let name = dev.strip_prefix("/dev/")?;
    wholes
        .iter()
        .enumerate()
        .filter(|(_, w)| name.starts_with(w.name.as_str()))
        .max_by_key(|(_, w)| w.name.len())
        .map(|(i, _)| i)
}

/// Percent of the time a device was busy between two readings of its I/O time in ms, at
/// most 100. `None` on the first reading, when the counter went back, or when no time passed.
pub(crate) fn busy_pct(prev: Option<u64>, cur: Option<u64>, secs: Option<f64>) -> Option<f32> {
    let (p, c, s) = (prev?, cur?, secs?);
    if c < p || s <= 0.0 {
        return None;
    }
    let pct = (c - p) as f64 / (s * 1000.0) * 100.0;
    Some(pct.min(100.0) as f32)
}

/// Bytes per second between two readings of a running counter. `None` if the counter went
/// back (the device was reset) or no time has passed.
pub(crate) fn per_second(prev: u64, cur: u64, secs: f64) -> Option<u64> {
    if cur < prev || secs <= 0.0 {
        return None;
    }
    Some(((cur - prev) as f64 / secs).round() as u64)
}

/// The two rates (read, write) from two readings of the two counters; `None` on the first
/// reading.
pub(crate) fn rate(
    prev: Option<(u64, u64)>,
    cur: Option<(u64, u64)>,
    secs: Option<f64>,
) -> Option<(u64, u64)> {
    let ((pr, pw), (cr, cw), s) = (prev?, cur?, secs?);
    Some((per_second(pr, cr, s)?, per_second(pw, cw, s)?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn root() -> Root {
        Root::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/disks"))
    }

    fn whole(name: &str) -> Whole {
        Whole {
            name: name.to_string(),
            prev: None,
            cur: None,
            rate: None,
            busy_prev: None,
            busy_cur: None,
            busy: None,
        }
    }

    #[test]
    fn the_mount_filter_keeps_disks_and_drops_the_rest() {
        let buf = std::fs::read(root().path("proc/mounts")).expect("fixture mounts");
        let got = mounts_from(&buf);
        let want = [
            ("/dev/sda1", "/"),
            ("/dev/nvme0n1p2", "/data disk"),
            ("/dev/mapper/vg-home", "/home"),
            ("tank/data", "/tank"),
        ];
        let want: Vec<(String, String)> = want
            .iter()
            .map(|(d, m)| (d.to_string(), m.to_string()))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn octal_escapes_are_unescaped() {
        assert_eq!(unescape(b"/data\\040disk"), "/data disk");
        assert_eq!(unescape(b"a\\134b"), "a\\b");
        assert_eq!(unescape(b"a\\9"), "a\\9");
    }

    #[test]
    fn a_partition_belongs_to_its_longest_whole_device() {
        let wholes = vec![whole("sda"), whole("sdaa"), whole("nvme0n1")];
        assert_eq!(whole_for("/dev/sdaa1", &wholes), Some(1));
        assert_eq!(whole_for("/dev/sda1", &wholes), Some(0));
        assert_eq!(whole_for("/dev/nvme0n1p2", &wholes), Some(2));
        assert_eq!(whole_for("/dev/mapper/sda-crypt", &wholes), None);
    }

    #[test]
    fn rates_come_from_two_readings() {
        assert_eq!(per_second(0, 1000, 2.0), Some(500));
        assert_eq!(per_second(1000, 1000, 2.0), Some(0));
        assert_eq!(per_second(1000, 999, 2.0), None);
        assert_eq!(per_second(0, 1000, 0.0), None);
        assert_eq!(rate(None, Some((0, 0)), Some(1.0)), None);
        assert_eq!(
            rate(Some((0, 0)), Some((4000, 800)), Some(1.0)),
            Some((4000, 800))
        );
        assert_eq!(rate(Some((0, 0)), Some((512, 0)), None), None);
        assert_eq!(busy_pct(Some(0), Some(500), Some(1.0)), Some(50.0));
        assert_eq!(busy_pct(Some(0), Some(5000), Some(1.0)), Some(100.0));
        assert_eq!(busy_pct(Some(900), Some(100), Some(1.0)), None);
        assert_eq!(busy_pct(None, Some(100), Some(1.0)), None);
    }

    #[test]
    fn disks_from_the_fixture_tree() {
        let mut disks = Disks::open(&root());
        let names: Vec<&str> = disks.wholes.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["nvme0n1", "sda"]);
        let first = disks.read().to_vec();
        // Space is read from the machine itself, so only mounts that exist here are listed:
        // "/" always is, and none is shown with 0 bytes.
        assert!(first.iter().any(|d| d.mount == "/"));
        assert!(first.iter().all(|d| d.total > 0));
        assert!(first.iter().all(|d| d.read_bps.is_none()));
        assert!(first.iter().all(|d| d.busy_pct.is_none()));
        std::thread::sleep(Duration::from_millis(5));
        let second = disks.read().to_vec();
        let sda1 = second.iter().find(|d| d.dev == "/dev/sda1").expect("sda1");
        assert_eq!(sda1.read_bps, Some(0));
        assert_eq!(sda1.write_bps, Some(0));
        assert_eq!(sda1.busy_pct, Some(0.0));
        if let Some(home) = second.iter().find(|d| d.dev == "/dev/mapper/vg-home") {
            assert_eq!(home.read_bps, None);
        }
    }
}
