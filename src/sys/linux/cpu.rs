//! CPU: total and per thread, clock, temperature and load (proc/stat and friends).

use std::path::{Path, PathBuf};
use std::time::Instant;

use super::reader::{fields, lines, num, PFile};
use crate::sys::{CpuSample, Root};

/// Room for `proc/stat`: a line of about 80 bytes per thread, so this holds several hundred.
const STAT_CAP: usize = 64 * 1024;
const LOAD_CAP: usize = 128;
const SMALL_CAP: usize = 32;
/// The package energy counter and its wrap point (root only on most machines).
const RAPL_ENERGY: &str = "sys/class/powercap/intel-rapl:0/energy_uj";
const RAPL_RANGE: &str = "sys/class/powercap/intel-rapl:0/max_energy_range_uj";

/// The hwmon chips that carry the CPU temperature.
const CPU_CHIPS: [&str; 4] = ["k10temp", "coretemp", "zenpower", "cpu_thermal"];

/// The CPU source, with its files kept open.
pub struct Cpu {
    stat: Option<PFile>,
    loadavg: Option<PFile>,
    freqs: Vec<PFile>,
    thread_freqs: Vec<PFile>,
    power: Option<PFile>,
    range: Option<u64>,
    energy: Option<(u64, Instant)>,
    temp: Option<PFile>,
    /// Total and busy time at the last reading: index 0 is the whole CPU, then each thread.
    last: Vec<(u64, u64)>,
    primed: bool,
    sample: CpuSample,
}

impl Cpu {
    pub fn open(root: &Root) -> Cpu {
        let stat_path = root.path("proc/stat");
        let threads = std::fs::read(&stat_path).map_or(0, |b| count_threads(&b));
        let thread_freqs = thread_freq_files(root, threads);
        // The policy clocks are read only when there is no clock per thread.
        let freqs = if thread_freqs.is_empty() {
            freq_files(root)
        } else {
            Vec::new()
        };
        Cpu {
            stat: PFile::open(&stat_path, STAT_CAP),
            loadavg: PFile::open(&root.path("proc/loadavg"), LOAD_CAP),
            freqs,
            thread_freqs,
            power: PFile::open(&root.path(RAPL_ENERGY), SMALL_CAP),
            range: PFile::open(&root.path(RAPL_RANGE), SMALL_CAP)
                .and_then(|mut f| first_number(&mut f)),
            energy: None,
            temp: cpu_temp_input(root).and_then(|p| PFile::open(&p, SMALL_CAP)),
            last: vec![(0, 0); threads + 1],
            primed: false,
            sample: CpuSample {
                threads: Vec::with_capacity(threads),
                threads_mhz: Vec::with_capacity(threads),
                ..CpuSample::default()
            },
        }
    }

    pub fn read(&mut self) -> &CpuSample {
        self.read_clocks();
        self.sample.power_w = self.read_power(Instant::now());
        self.sample.temp_c = self.temp.as_mut().and_then(read_celsius);
        self.sample.load = self.loadavg.as_mut().and_then(read_load);
        self.read_stat();
        &self.sample
    }

    /// The clock of each thread, and `mhz` as their average (the policy clocks only when
    /// there are no per-thread clocks).
    fn read_clocks(&mut self) {
        self.sample.threads_mhz.clear();
        for file in self.thread_freqs.iter_mut() {
            if let Some(mhz) = first_number(file).and_then(|khz| u32::try_from(khz / 1000).ok()) {
                self.sample.threads_mhz.push(mhz);
            }
        }
        let count = self.sample.threads_mhz.len() as u64;
        let sum: u64 = self.sample.threads_mhz.iter().map(|m| u64::from(*m)).sum();
        self.sample.mhz = match count {
            0 => read_mhz(&mut self.freqs),
            n => u32::try_from(sum / n).ok(),
        };
    }

    /// Watts since the last reading, from the package energy counter. `None` the first time
    /// and when the counter cannot be read.
    fn read_power(&mut self, now: Instant) -> Option<f32> {
        let file = self.power.as_mut()?;
        let energy = first_number(file)?;
        let (prev, before) = self.energy.replace((energy, now))?;
        watts(
            prev,
            energy,
            self.range,
            now.duration_since(before).as_secs_f64(),
        )
    }

    /// Total and per-thread percentages, from the busy time since the last reading.
    fn read_stat(&mut self) {
        self.sample.pct = None;
        self.sample.threads.clear();
        let Some(file) = self.stat.as_mut() else {
            return;
        };
        let buf = file.read();
        let primed = self.primed;
        let mut whole = None;
        let mut index = 0usize;
        for line in lines(buf) {
            let mut parts = fields(line);
            let Some(name) = parts.next() else {
                continue;
            };
            if !name.starts_with(b"cpu") {
                break;
            }
            let Some(now) = times(parts) else {
                continue;
            };
            if name == &b"cpu"[..] {
                whole = Some(now);
                continue;
            }
            index += 1;
            if let Some(slot) = self.last.get_mut(index) {
                if primed {
                    self.sample.threads.push(pct(*slot, now).unwrap_or(0.0));
                }
                *slot = now;
            }
        }
        let Some(now) = whole else {
            return;
        };
        if let Some(first) = self.last.first_mut() {
            if primed {
                self.sample.pct = pct(*first, now);
            }
            *first = now;
        }
        self.primed = true;
    }
}

/// The first number of a one-number file.
fn first_number(file: &mut PFile) -> Option<u64> {
    fields(file.read()).next().and_then(num)
}

/// The current clock file of each thread (`cpuN`), for the threads counted in proc/stat.
fn thread_freq_files(root: &Root, threads: usize) -> Vec<PFile> {
    (0..threads)
        .filter_map(|n| {
            let path = root.path(&format!(
                "sys/devices/system/cpu/cpu{n}/cpufreq/scaling_cur_freq"
            ));
            PFile::open(&path, SMALL_CAP)
        })
        .collect()
}

/// Watts between two energy readings in microjoules; the counter wraps at `range`.
pub(crate) fn watts(prev: u64, cur: u64, range: Option<u64>, secs: f64) -> Option<f32> {
    if secs <= 0.0 {
        return None;
    }
    let used = if cur >= prev {
        cur - prev
    } else {
        range?.saturating_sub(prev).saturating_add(cur)
    };
    Some((used as f64 / 1e6 / secs) as f32)
}

/// How many per-thread lines (`cpu0`, `cpu1`, …) a `proc/stat` buffer holds.
pub fn count_threads(buf: &[u8]) -> usize {
    lines(buf)
        .take_while(|l| l.starts_with(b"cpu"))
        .filter(|l| l.get(3).is_some_and(|c| c.is_ascii_digit()))
        .count()
}

/// The total and busy time of a `cpu` line: the first eight counters, busy = total - idle - iowait.
fn times<'a>(mut parts: impl Iterator<Item = &'a [u8]>) -> Option<(u64, u64)> {
    let mut c = [0u64; 8];
    for slot in c.iter_mut() {
        *slot = num(parts.next()?)?;
    }
    let total = c.iter().fold(0u64, |a, b| a.saturating_add(*b));
    let busy = total.saturating_sub(c[3].saturating_add(c[4]));
    Some((total, busy))
}

/// Percent busy between two readings. `None` when no time has passed.
fn pct(prev: (u64, u64), now: (u64, u64)) -> Option<f32> {
    let dt = now.0.saturating_sub(prev.0);
    if dt == 0 {
        return None;
    }
    let db = now.1.saturating_sub(prev.1);
    Some(db as f32 / dt as f32 * 100.0)
}

fn read_load(file: &mut PFile) -> Option<[f32; 3]> {
    let buf = file.read();
    let mut parts = fields(buf);
    let mut out = [0f32; 3];
    for slot in out.iter_mut() {
        *slot = std::str::from_utf8(parts.next()?).ok()?.parse().ok()?;
    }
    Some(out)
}

/// The average clock over every policy, in MHz.
fn read_mhz(files: &mut [PFile]) -> Option<u32> {
    let (mut sum, mut count) = (0u64, 0u64);
    for file in files.iter_mut() {
        if let Some(khz) = fields(file.read()).next().and_then(num) {
            sum = sum.saturating_add(khz);
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    u32::try_from(sum / count / 1000).ok()
}

/// A millidegree file, in degrees.
fn read_celsius(file: &mut PFile) -> Option<f32> {
    let milli = fields(file.read()).next().and_then(num)?;
    Some(milli as f32 / 1000.0)
}

/// One `policy*` folder per frequency domain, with its current clock file.
fn freq_files(root: &Root) -> Vec<PFile> {
    let Ok(dir) = std::fs::read_dir(root.path("sys/devices/system/cpu/cpufreq")) else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("policy"))
        .filter_map(|e| PFile::open(&e.path().join("scaling_cur_freq"), SMALL_CAP))
        .collect()
}

fn is_temp_input(path: &Path) -> bool {
    path.file_name().is_some_and(|f| {
        let f = f.to_string_lossy();
        f.starts_with("temp") && f.ends_with("_input")
    })
}

/// The label file that goes with a `tempN_input` file.
fn label_of(input: &Path) -> String {
    let file = input
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::fs::read_to_string(input.with_file_name(file.replacen("_input", "_label", 1)))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The input file of the CPU temperature. Among the CPU chips, a label `Tdie` or `Tctl` wins,
/// then one starting `Package`, then any temperature of the chip.
fn cpu_temp_input(root: &Root) -> Option<PathBuf> {
    let mut chips: Vec<PathBuf> = std::fs::read_dir(root.path("sys/class/hwmon"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    chips.sort();
    let mut best: Option<(u8, PathBuf)> = None;
    for chip in chips {
        let Ok(name) = std::fs::read_to_string(chip.join("name")) else {
            continue;
        };
        if !CPU_CHIPS.contains(&name.trim()) {
            continue;
        }
        let Ok(dir) = std::fs::read_dir(&chip) else {
            continue;
        };
        let mut inputs: Vec<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_temp_input(p))
            .collect();
        inputs.sort();
        for input in inputs {
            let label = label_of(&input);
            let rank = match label.as_str() {
                "Tdie" | "Tctl" => 0,
                l if l.starts_with("Package") => 1,
                _ => 2,
            };
            match &best {
                Some((r, _)) if *r <= rank => {}
                _ => best = Some((rank, input)),
            }
        }
    }
    best.map(|(_, p)| p)
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

    fn near(got: Option<f32>, want: f32) -> bool {
        got.is_some_and(|g| (g - want).abs() < 1e-3)
    }

    #[test]
    fn percentages_from_two_snapshots() {
        let mut cpu = Cpu::open(&tree(""));
        let first = cpu.read().clone();
        assert_eq!(first.pct, None);
        assert!(first.threads.is_empty());
        assert_eq!(first.mhz, Some(3000));
        assert_eq!(first.threads_mhz, vec![2800, 3200]);
        assert_eq!(first.temp_c, Some(63.5));
        assert_eq!(first.load, Some([0.52, 0.58, 0.59]));

        cpu.stat = PFile::open(&tree("later").path("proc/stat"), STAT_CAP);
        let second = cpu.read();
        assert!(near(second.pct, 40.0), "total {:?}", second.pct);
        assert_eq!(second.threads.len(), 2);
        assert!(near(Some(second.threads[0]), 50.0));
        assert!(near(Some(second.threads[1]), 30.0));
    }

    #[test]
    fn counts_the_thread_lines_only() {
        let stat = std::fs::read(tree("").path("proc/stat")).expect("fixture");
        assert_eq!(count_threads(&stat), 2);
    }

    #[test]
    fn watts_from_two_energy_readings_and_a_wrap() {
        assert_eq!(watts(1_000_000, 3_000_000, None, 2.0), Some(1.0));
        let max = 10_000_000;
        assert_eq!(watts(max - 1_000_000, 500_000, Some(max), 1.0), Some(1.5));
        assert_eq!(watts(5, 4, None, 1.0), None);
        assert_eq!(watts(0, 1, None, 0.0), None);
    }

    #[test]
    fn a_missing_tree_reads_as_nothing() {
        let mut cpu = Cpu::open(&tree("none"));
        let s = cpu.read();
        assert_eq!(s.pct, None);
        assert!(s.threads.is_empty());
        assert_eq!(s.mhz, None);
        assert_eq!(s.temp_c, None);
        assert_eq!(s.load, None);
    }
}
