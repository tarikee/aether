//! NVIDIA graphics cards, through NVML: the library is loaded at run time (`calls::nvml`), so
//! the program does not need it. Written from NVML's documented interface; not yet seen on a
//! real card.

use std::path::Path;

use super::procs::ProcUse;
use crate::sys::linux::calls::{self, NvDevice, NvProcInfo, NvSample, Nvml};
use crate::sys::{GpuSample, Root};

/// The NVML session for `root`: only the running system has NVIDIA cards to ask about.
pub fn session(root: &Root) -> Option<&'static Nvml> {
    if root.base() != Path::new("/") {
        return None;
    }
    calls::nvml()
}

/// The devices of the session, in NVML's order. Empty without a session.
fn devices(nvml: &Nvml) -> Vec<NvDevice> {
    let Some(n) = nvml.device_count() else {
        return Vec::new();
    };
    (0..n).filter_map(|i| nvml.device(i)).collect()
}

/// One NVIDIA card.
pub struct Card {
    nvml: &'static Nvml,
    dev: NvDevice,
    name: String,
}

/// The NVIDIA cards of the session, in NVML's order.
pub fn cards(nvml: Option<&'static Nvml>) -> Vec<Card> {
    let Some(nvml) = nvml else {
        return Vec::new();
    };
    devices(nvml)
        .into_iter()
        .map(|dev| Card {
            nvml,
            dev,
            name: nvml.name(dev).unwrap_or_else(|| "NVIDIA GPU".to_string()),
        })
        .collect()
}

impl Card {
    /// The card before its first reading: name and vendor, every value unknown.
    pub fn blank(&self) -> GpuSample {
        GpuSample {
            name: self.name.clone(),
            vendor: "nvidia",
            pct: None,
            temp_c: None,
            power_w: None,
            vram_used: None,
            vram_total: None,
            fan_rpm: None,
        }
    }

    /// Fills the card's values in place. NVML reports fan speed in percent, not rpm, so
    /// `fan_rpm` stays unknown.
    pub fn read(&mut self, out: &mut GpuSample) {
        out.pct = self.nvml.utilization(self.dev).map(|u| u.gpu as f32);
        let memory = self.nvml.memory(self.dev);
        out.vram_used = memory.map(|m| m.used);
        out.vram_total = memory.map(|m| m.total);
        out.temp_c = self.nvml.temperature(self.dev).map(|t| t as f32);
        out.power_w = self.nvml.power_mw(self.dev).map(|mw| mw as f32 / 1000.0);
        out.fan_rpm = None;
    }
}

/// Per-process GPU use on the NVIDIA cards, with the buffers reused from tick to tick.
pub struct Procs {
    nvml: Option<&'static Nvml>,
    /// Each device and the newest sample time stamp seen on it.
    devices: Vec<(NvDevice, u64)>,
    infos: Vec<NvProcInfo>,
    samples: Vec<NvSample>,
    dev_rows: Vec<ProcUse>,
    rows: Vec<ProcUse>,
}

impl Procs {
    pub fn open(nvml: Option<&'static Nvml>) -> Procs {
        let devices = nvml
            .map(|n| devices(n).into_iter().map(|d| (d, 0)).collect())
            .unwrap_or_default();
        Procs {
            nvml,
            devices,
            infos: Vec::new(),
            samples: Vec::new(),
            dev_rows: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// This tick's rows, one per process NVML lists on any card, sorted by pid. The percent is
    /// the process's SM use, capped at 100 over the cards; VRAM is `None` when NVML does not know
    /// it.
    pub fn read(&mut self) -> &[ProcUse] {
        self.rows.clear();
        let Some(nvml) = self.nvml else {
            return &self.rows;
        };
        for (dev, since) in self.devices.iter_mut() {
            self.dev_rows.clear();
            let util_ok = nvml.samples(*dev, *since, &mut self.samples);
            for s in &self.samples {
                *since = (*since).max(s.time_stamp);
                self.dev_rows.push((s.pid, Some(s.sm_util as f32), None));
            }
            // A process NVML lists but gives no sample is idle: 0% when the samples worked.
            let idle = util_ok.then_some(0.0);
            for graphics in [false, true] {
                if nvml.processes(*dev, graphics, &mut self.infos) {
                    for info in &self.infos {
                        let vram =
                            (info.used_gpu_memory != u64::MAX).then_some(info.used_gpu_memory);
                        self.dev_rows.push((info.pid, idle, vram));
                    }
                }
            }
            // One process on one card is counted once: the largest value of each.
            join(&mut self.dev_rows, false);
            self.rows.extend_from_slice(&self.dev_rows);
        }
        join(&mut self.rows, true);
        &self.rows
    }
}

/// Sorts the rows by pid, and joins the rows of one pid. Across cards (`across`) the values add
/// up, with the percent capped at 100; on one card the largest value is kept.
pub fn join(rows: &mut Vec<ProcUse>, across: bool) {
    rows.sort_by_key(|r| r.0);
    let mut kept = 0;
    for i in 0..rows.len() {
        let cur = rows[i];
        if kept > 0 && rows[kept - 1].0 == cur.0 {
            let prev = &mut rows[kept - 1];
            prev.1 = pct_of(prev.1, cur.1, across);
            prev.2 = vram_of(prev.2, cur.2, across);
        } else {
            rows[kept] = cur;
            kept += 1;
        }
    }
    rows.truncate(kept);
}

fn pct_of(a: Option<f32>, b: Option<f32>, across: bool) -> Option<f32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if across { (x + y).min(100.0) } else { x.max(y) }),
        (x, None) | (None, x) => x,
    }
}

fn vram_of(a: Option<u64>, b: Option<u64>, across: bool) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if across {
            x.saturating_add(y)
        } else {
            x.max(y)
        }),
        (x, None) | (None, x) => x,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn without_the_library_there_are_no_cards_and_no_process_rows() {
        assert!(cards(None).is_empty());
        let mut p = Procs::open(None);
        assert!(p.read().is_empty());
    }

    #[test]
    fn one_process_on_two_cards_adds_up_and_one_on_one_card_takes_the_largest() {
        let mut rows = vec![
            (30, Some(10.0), Some(100)),
            (20, Some(5.0), None),
            (30, Some(70.0), Some(300)),
            (30, None, Some(200)),
            (20, None, Some(7)),
        ];
        join(&mut rows, false);
        assert_eq!(
            rows,
            vec![(20, Some(5.0), Some(7)), (30, Some(70.0), Some(300))]
        );

        let mut rows = vec![
            (30, Some(70.0), Some(300)),
            (30, Some(60.0), None),
            (30, None, Some(5)),
        ];
        join(&mut rows, true);
        assert_eq!(rows, vec![(30, Some(100.0), Some(305))]);
    }
}
