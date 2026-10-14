//! The graphs of the full-screen view: fixed rings of the last readings (CPU, memory used, the
//! first GPU's use, and the first interface's down and up rates). A push overwrites the oldest
//! value, so nothing is allocated per tick.

use aether::Reading;

/// The readings a ring keeps.
pub const SAMPLES: usize = 300;

/// The last `SAMPLES` values of one number; `None` where a reading had none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    vals: [Option<f32>; SAMPLES],
    next: usize,
    len: usize,
}

impl Default for Ring {
    fn default() -> Ring {
        Ring {
            vals: [None; SAMPLES],
            next: 0,
            len: 0,
        }
    }
}

impl Ring {
    pub fn push(&mut self, v: Option<f32>) {
        self.vals[self.next] = v;
        self.next = (self.next + 1) % SAMPLES;
        self.len = (self.len + 1).min(SAMPLES);
    }

    /// How many values the ring holds.
    pub fn len(&self) -> usize {
        self.len
    }

    /// The last `n` values, oldest first (fewer when the ring holds fewer).
    pub fn newest(&self, n: usize) -> impl Iterator<Item = Option<f32>> + Clone + '_ {
        let n = n.min(self.len);
        let first = self.next + SAMPLES - self.len;
        (self.len - n..self.len).map(move |i| self.vals[(first + i) % SAMPLES])
    }
}

/// The rings the view draws from. Pushed once per reading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    pub cpu: Ring,
    /// Memory used, in percent of the total.
    pub mem: Ring,
    /// The first GPU's use, in percent.
    pub gpu: Ring,
    pub down: Ring,
    pub up: Ring,
}

impl History {
    pub fn push(&mut self, r: &Reading) {
        self.cpu.push(r.cpu.pct);
        let m = &r.mem;
        self.mem
            .push((m.total > 0).then(|| (m.used as f64 / m.total as f64 * 100.0) as f32));
        self.gpu.push(r.gpus.first().and_then(|g| g.pct));
        let first = r.nets.first();
        self.down
            .push(first.and_then(|n| n.rx_bps).map(|b| b as f32));
        self.up.push(first.and_then(|n| n.tx_bps).map(|b| b as f32));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_keeps_the_newest_values_in_order() {
        let mut ring = Ring::default();
        assert_eq!(ring.len(), 0);
        for i in 0..(SAMPLES + 5) {
            ring.push(Some(i as f32));
        }
        assert_eq!(ring.len(), SAMPLES);
        let last: Vec<Option<f32>> = ring.newest(3).collect();
        let want = [
            Some((SAMPLES + 2) as f32),
            Some((SAMPLES + 3) as f32),
            Some((SAMPLES + 4) as f32),
        ];
        assert_eq!(last, want);
        assert_eq!(ring.newest(1000).count(), SAMPLES);
        assert_eq!(ring.newest(1000).next(), Some(Some(5.0)));
    }

    #[test]
    fn a_missing_value_stays_missing() {
        let mut ring = Ring::default();
        ring.push(None);
        ring.push(Some(2.0));
        let all: Vec<Option<f32>> = ring.newest(10).collect();
        assert_eq!(all, [None, Some(2.0)]);
    }

    #[test]
    fn memory_is_kept_as_a_share_and_a_missing_gpu_as_none() {
        let mut r = Reading::default();
        r.mem.total = 1000;
        r.mem.used = 250;
        let mut h = History::default();
        h.push(&r);
        assert_eq!(h.mem.newest(1).next(), Some(Some(25.0)));
        assert_eq!(h.gpu.newest(1).next(), Some(None));
        r.mem.total = 0;
        h.push(&r);
        assert_eq!(h.mem.newest(1).next(), Some(None));
    }
}
