//! Graphics cards and the processes that use them. Each vendor is one file; the list of
//! vendors is `VENDORS` below.

pub mod amd;
pub mod nvidia;
pub mod procs;

pub use procs::GpuProcs;

use std::fs;
use std::path::Path;

use crate::sys::{GpuSample, Root};

/// THE list of vendors, by the names their files use.
pub const VENDORS: &[&str] = &["amd", "nvidia"];

/// One graphics card, of a vendor's kind.
enum Card {
    Amd(Box<amd::Card>),
    Nvidia(nvidia::Card),
}

impl Card {
    fn blank(&self) -> GpuSample {
        match self {
            Card::Amd(c) => c.blank(),
            Card::Nvidia(c) => c.blank(),
        }
    }

    fn read(&mut self, out: &mut GpuSample) {
        match self {
            Card::Amd(c) => c.read(out),
            Card::Nvidia(c) => c.read(out),
        }
    }
}

/// Opens the card whose `device` folder is `dev`, by the first vendor that knows it.
fn open_card(root: &Root, dev: &Path) -> Option<Card> {
    amd::open(root, dev).map(|c| Card::Amd(Box::new(c)))
}

/// The graphics card source, with its files kept open.
pub struct Gpus {
    cards: Vec<Card>,
    list: Vec<GpuSample>,
}

impl Gpus {
    pub fn open(root: &Root) -> Gpus {
        let mut cards = Vec::new();
        for name in drm_cards(&root.path("sys/class/drm")) {
            if let Some(card) =
                open_card(root, &root.path("sys/class/drm").join(name).join("device"))
            {
                cards.push(card);
            }
        }
        // NVIDIA cards come from NVML, after the DRM cards.
        cards.extend(
            nvidia::cards(nvidia::session(root))
                .into_iter()
                .map(Card::Nvidia),
        );
        let list = cards.iter().map(Card::blank).collect();
        Gpus { cards, list }
    }

    pub fn read(&mut self) -> &[GpuSample] {
        for (card, out) in self.cards.iter_mut().zip(self.list.iter_mut()) {
            card.read(out);
        }
        &self.list
    }
}

/// The `cardN` names in `drm`, in order. Connectors (`card0-DP-1`) and render nodes are skipped.
fn drm_cards(drm: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(drm) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            n.strip_prefix("card")
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_amd_card_is_read_and_other_vendors_and_connectors_are_skipped() {
        let root = Root::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sensors"));
        let mut g = Gpus::open(&root);
        let list = g.read().to_vec();
        assert_eq!(list.len(), 1);
        let card = &list[0];
        assert_eq!(card.name, "Example GPU 73bf");
        assert_eq!(card.vendor, "amd");
        assert_eq!(card.pct, Some(37.0));
        assert_eq!(card.temp_c, Some(61.0));
        assert_eq!(card.fan_rpm, Some(1450));
        assert_eq!(card.power_w, Some(98.0));
        assert_eq!(card.vram_used, Some(2_147_483_648));
        assert_eq!(card.vram_total, Some(8_589_934_592));
    }

    #[test]
    fn a_tree_without_cards_gives_an_empty_list() {
        let root = Root::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/none"));
        assert!(Gpus::open(&root).read().is_empty());
    }
}
