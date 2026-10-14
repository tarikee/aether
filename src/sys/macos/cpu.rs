//! CPU: the total and each thread's use from the kernel's tick counters (`host_processor_info`),
//! and the load average. The clock, the temperature and the power are not read: macOS gives
//! them only to a program that runs as root, so they stay `None`. The temperature is the mean of
//! the die sensors (`tdie`), which need no root; it is read on every fourth reading (each sensor
//! costs an exchange with the system), so it is never more than four seconds old.

use super::calls::{self, Thermometers};
use crate::sys::{CpuSample, Root};

/// Per-thread and total CPU, and the load.
pub struct Cpu {
    sample: CpuSample,
    now: Vec<[u32; 4]>,
    last: Vec<[u32; 4]>,
    thermo: Option<Thermometers>,
    die: Vec<usize>,
    ticks: u32,
}

impl Cpu {
    pub fn open(_root: &Root) -> Cpu {
        let thermo = Thermometers::open();
        let die = thermo.as_ref().map_or_else(Vec::new, |t| {
            (0..t.names.len())
                .filter(|&i| t.names[i].contains("tdie") && t.celsius(i).is_some())
                .collect()
        });
        Cpu {
            sample: CpuSample::default(),
            now: Vec::new(),
            last: Vec::new(),
            thermo,
            die,
            ticks: 0,
        }
    }

    pub fn read(&mut self) -> &CpuSample {
        self.sample.threads.clear();
        self.sample.pct = None;
        if calls::cpu_ticks(&mut self.now) {
            if self.last.len() == self.now.len() {
                let (mut busy, mut total) = (0u64, 0u64);
                for (a, b) in self.last.iter().zip(&self.now) {
                    let (bz, tz) = delta(*a, *b);
                    busy += bz;
                    total += tz;
                    self.sample.threads.push(pct_of(bz, tz).unwrap_or(0.0));
                }
                self.sample.pct = pct_of(busy, total);
            }
            std::mem::swap(&mut self.last, &mut self.now);
        }
        self.sample.load = calls::load_average();
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % 4 == 1 {
            self.sample.temp_c = self.thermo.as_ref().and_then(|t| {
                let (sum, n) = self
                    .die
                    .iter()
                    .filter_map(|&i| t.celsius(i))
                    .fold((0.0f32, 0u32), |(s, n), v| (s + v, n + 1));
                (n > 0).then(|| sum / n as f32)
            });
        }
        &self.sample
    }
}

/// (busy, total) ticks between two readings of [user, system, idle, nice]; the counters are
/// 32 bits wide and may wrap.
fn delta(a: [u32; 4], b: [u32; 4]) -> (u64, u64) {
    let d = |i: usize| u64::from(b[i].wrapping_sub(a[i]));
    let (user, system, idle, nice) = (d(0), d(1), d(2), d(3));
    (user + system + nice, user + system + idle + nice)
}

/// Busy share of the total, 0 to 100; `None` when no time passed.
fn pct_of(busy: u64, total: u64) -> Option<f32> {
    (total > 0).then(|| (busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0) as f32)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn busy_is_what_is_not_idle_and_a_wrapped_counter_still_counts() {
        // 100 ticks: 30 user, 10 system, 55 idle, 5 nice.
        let (busy, total) = delta([0; 4], [30, 10, 55, 5]);
        assert_eq!((busy, total), (45, 100));
        assert_eq!(pct_of(busy, total), Some(45.0));
        assert_eq!(pct_of(0, 0), None);
        let (busy, total) = delta([u32::MAX - 9, 0, 0, 0], [10, 0, 80, 0]);
        assert_eq!((busy, total), (20, 100));
    }

    #[test]
    fn two_readings_give_threads_and_percentages_in_range() {
        let mut cpu = Cpu::open(&Root::system());
        let first = cpu.read().clone();
        assert!(first.pct.is_none(), "no percent from one reading");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let s = cpu.read().clone();
        assert!(!s.threads.is_empty(), "threads");
        let pct = s.pct.expect("a total after two readings");
        assert!((0.0..=100.0).contains(&pct), "total {pct}");
        for t in &s.threads {
            assert!((0.0..=100.0).contains(t), "thread {t}");
        }
        assert!(s.load.is_some());
        assert!(s.mhz.is_none() && s.power_w.is_none());
        assert!(s.temp_c.is_none_or(|t| t > 0.0 && t < 150.0));
    }
}
