//! Graphics cards, from the kernel's graphics statistics (the numbers Task Manager shows): the
//! name and dedicated memory once, then each reading the busiest engine's share of the time
//! and the memory resident in the card's own segments. Temperature, power and fan need the
//! vendor's driver: `None`. Use per process is not read on Windows.

use super::calls::{self, Adapter};
use crate::sys::{GpuSample, Root};
use std::time::Instant;
use windows_sys::Win32::Foundation::LUID;

/// One card being read.
struct Card {
    luid: LUID,
    nodes: u32,
    segments: u32,
    last: Vec<u64>,
    at: Option<Instant>,
}

/// Graphics cards.
pub struct Gpus {
    list: Vec<GpuSample>,
    cards: Vec<Card>,
    found: bool,
}

impl Gpus {
    pub fn open(_root: &Root) -> Gpus {
        Gpus {
            list: Vec::new(),
            cards: Vec::new(),
            found: false,
        }
    }

    pub fn read(&mut self, _shares: Option<&[(String, f32, u64)]>) -> &[GpuSample] {
        if !self.found {
            self.found = true;
            let mut all = Vec::new();
            calls::adapters(&mut all);
            // Real cards only: not the processor drawing in software, not display-only ones.
            for a in all.into_iter().filter(|a| a.render && !a.software) {
                self.add(a);
            }
        }
        for (card, sample) in self.cards.iter_mut().zip(self.list.iter_mut()) {
            read_card(card, sample);
        }
        &self.list
    }

    fn add(&mut self, a: Adapter) {
        let Some((nodes, segments)) = calls::gpu_layout(a.luid) else {
            return;
        };
        self.cards.push(Card {
            luid: a.luid,
            nodes,
            segments,
            last: Vec::new(),
            at: None,
        });
        self.list.push(GpuSample {
            name: if a.name.is_empty() {
                "GPU".to_string()
            } else {
                a.name
            },
            vendor: vendor(a.vendor_id),
            pct: None,
            temp_c: None,
            power_w: None,
            vram_used: None,
            vram_total: (a.vram_total > 0).then_some(a.vram_total),
            fan_rpm: None,
        });
    }
}

/// One reading of a card: the busiest engine since the last reading, and the memory in use.
fn read_card(card: &mut Card, sample: &mut GpuSample) {
    let now = Instant::now();
    let span = card
        .at
        .map(|a| now.duration_since(a).as_nanos() as f64 / 100.0);
    card.at = Some(now);
    let fresh = card.last.len() != card.nodes as usize;
    card.last.resize(card.nodes as usize, 0);
    let mut busiest: Option<f32> = None;
    for (node, last) in (0..card.nodes).zip(card.last.iter_mut()) {
        let Some(t) = calls::gpu_node_time(card.luid, node) else {
            continue;
        };
        if let (false, Some(span)) = (fresh, span) {
            let pct = share(*last, t, span);
            busiest = Some(busiest.map_or(pct, |b| b.max(pct)));
        }
        *last = t;
    }
    sample.pct = busiest;
    if sample.vram_total.is_some() {
        sample.vram_used = (0..card.segments)
            .filter_map(|s| calls::gpu_segment(card.luid, s))
            .filter(|(_, aperture)| !aperture)
            .map(|(resident, _)| resident)
            .reduce(|a, b| a + b);
    }
}

/// An engine's share of `span` (in 100 ns), at most 100.
fn share(last: u64, now: u64, span: f64) -> f32 {
    if span <= 0.0 {
        return 0.0;
    }
    (now.saturating_sub(last) as f64 / span * 100.0).clamp(0.0, 100.0) as f32
}

/// The vendor by its PCI id.
fn vendor(id: u32) -> &'static str {
    match id {
        0x1002 => "amd",
        0x10de => "nvidia",
        0x8086 => "intel",
        0x1414 => "microsoft",
        0x5143 => "qualcomm",
        _ => "other",
    }
}

/// Per-process GPU use: not read on Windows (it would cost a query per process and engine).
pub struct GpuProcs {
    out: Vec<(u32, f32, u64)>,
}

impl GpuProcs {
    pub fn open(_root: &Root) -> GpuProcs {
        GpuProcs { out: Vec::new() }
    }

    pub fn read(&mut self, _pids: &[u32]) -> &[(u32, f32, u64)] {
        &self.out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn shares_and_vendors() {
        assert_eq!(share(0, 500, 1000.0), 50.0);
        assert_eq!(share(0, 5000, 1000.0), 100.0);
        assert_eq!(share(10, 5, 1000.0), 0.0);
        assert_eq!(vendor(0x10de), "nvidia");
        assert_eq!(vendor(0x1234), "other");
    }

    /// Every adapter, the software one included, answers its queries without a fault; a card
    /// that is listed has a name and a percent within range on its second reading.
    #[test]
    fn adapters_and_cards_read_cleanly() {
        let mut all = Vec::new();
        calls::adapters(&mut all);
        for a in &all {
            if let Some((nodes, segments)) = calls::gpu_layout(a.luid) {
                for n in 0..nodes {
                    let _ = calls::gpu_node_time(a.luid, n);
                }
                for s in 0..segments {
                    let _ = calls::gpu_segment(a.luid, s);
                }
            }
        }
        let mut gpus = Gpus::open(&Root::system());
        gpus.read(None);
        std::thread::sleep(std::time::Duration::from_millis(200));
        for g in gpus.read(None) {
            assert!(!g.name.is_empty());
            if let Some(p) = g.pct {
                assert!((0.0..=100.0).contains(&p));
            }
        }
    }
}
