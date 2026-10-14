//! CPU: the total and each thread's use from the kernel's processor times, the clock from the
//! power manager. Windows has no load average, and no CPU temperature or power without a
//! driver: those stay `None`.

use super::calls;
use crate::sys::{CpuSample, Root};

/// Per-thread and total CPU, and the clock.
pub struct Cpu {
    sample: CpuSample,
    /// The kernel's table of processor times, kept between readings.
    buf: Vec<u64>,
    /// The power manager's answer, kept between readings.
    power: Vec<u64>,
    now: Vec<[u64; 3]>,
    last: Vec<[u64; 3]>,
    total_last: Option<[u64; 3]>,
}

impl Cpu {
    pub fn open(_root: &Root) -> Cpu {
        Cpu {
            sample: CpuSample::default(),
            buf: Vec::new(),
            power: Vec::new(),
            now: Vec::new(),
            last: Vec::new(),
            total_last: None,
        }
    }

    pub fn read(&mut self) -> &CpuSample {
        if self.buf.is_empty() {
            // One entry of six 8-byte values per thread of this processor group.
            self.buf.resize(calls::group_threads().max(1) * 6, 0);
        }
        let total = calls::system_times();
        self.sample.pct = match (self.total_last, total) {
            (Some(a), Some(b)) => busy_pct(a, b),
            _ => None,
        };
        self.total_last = total;

        self.sample.threads.clear();
        if calls::processor_times(&mut self.buf, &mut self.now) {
            if self.last.len() == self.now.len() {
                self.sample.threads.extend(
                    self.last
                        .iter()
                        .zip(&self.now)
                        .map(|(a, b)| busy_pct(*a, *b).unwrap_or(0.0)),
                );
            }
            std::mem::swap(&mut self.last, &mut self.now);
        }

        let threads = calls::thread_count() as usize;
        if !calls::processor_mhz(&mut self.power, threads, &mut self.sample.threads_mhz) {
            self.sample.threads_mhz.clear();
        }
        self.sample.mhz = average(&self.sample.threads_mhz);
        &self.sample
    }
}

/// The busy share between two readings of (idle, kernel with idle, user), at most 100.
pub(crate) fn busy_pct(a: [u64; 3], b: [u64; 3]) -> Option<f32> {
    let idle = b[0].saturating_sub(a[0]);
    let total = b[1].saturating_sub(a[1]) + b[2].saturating_sub(a[2]);
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle);
    Some((busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0) as f32)
}

/// The average of the clocks that are not 0, or `None`.
fn average(mhz: &[u32]) -> Option<u32> {
    let (sum, n) = mhz
        .iter()
        .filter(|&&m| m > 0)
        .fold((0u64, 0u64), |(s, n), &m| (s + u64::from(m), n + 1));
    (n > 0).then(|| (sum / n) as u32)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn busy_is_what_is_not_idle() {
        // 1000 ticks: 600 kernel (400 of them idle), 400 user: 600 busy.
        assert_eq!(busy_pct([0, 0, 0], [400, 600, 400]), Some(60.0));
        assert_eq!(busy_pct([5, 5, 5], [5, 5, 5]), None);
        assert_eq!(average(&[0, 3000, 4000]), Some(3500));
        assert_eq!(average(&[]), None);
    }

    #[test]
    fn two_readings_give_threads_and_percentages_in_range() {
        let mut cpu = Cpu::open(&Root::system());
        let first = cpu.read().clone();
        assert!(first.pct.is_none(), "no percent from one reading");
        assert!(first.load.is_none(), "Windows has no load average");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let s = cpu.read().clone();
        assert!(!s.threads.is_empty(), "threads");
        let pct = s.pct.expect("a total after two readings");
        assert!((0.0..=100.0).contains(&pct), "total {pct}");
        for t in &s.threads {
            assert!((0.0..=100.0).contains(t), "thread {t}");
        }
        assert!(s.temp_c.is_none() && s.load.is_none());
    }
}
