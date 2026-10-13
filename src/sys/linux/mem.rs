//! Memory and swap (proc/meminfo and the memory pressure file).

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
        Mem {
            meminfo: PFile::open(&root.path("proc/meminfo"), MEMINFO_CAP),
            pressure: PFile::open(&root.path("proc/pressure/memory"), PRESSURE_CAP),
            arc: PFile::open(&root.path("proc/spl/kstat/zfs/arcstats"), ARC_CAP),
            sample: MemSample::default(),
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
    fn a_missing_tree_reads_as_nothing() {
        let mut mem = Mem::open(&tree("none"));
        let s = mem.read();
        assert_eq!(*s, MemSample::default());
    }
}
