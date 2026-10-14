//! Graphics cards and the processes that use them. Each vendor is one file; the list of
//! vendors is `VENDORS` below.

pub mod amd;
pub mod intel;
pub mod procs;

pub use procs::GpuProcs;

use std::fs;
use std::path::Path;

use crate::sys::{GpuSample, Root};

/// THE list of vendors, by the names their files use.
pub const VENDORS: &[&str] = &["amd", "intel"];

/// One process's GPU use with its card's PCI address: (address, percent, VRAM bytes).
pub type Share = (String, f32, u64);

/// One graphics card, of a vendor's kind.
enum Card {
    Amd(amd::Card),
    Intel(intel::Card),
}

impl Card {
    fn blank(&self) -> GpuSample {
        match self {
            Card::Amd(c) => c.blank(),
            Card::Intel(c) => c.blank(),
        }
    }

    fn read(&mut self, out: &mut GpuSample) {
        match self {
            Card::Amd(c) => c.read(out),
            Card::Intel(c) => c.read(out),
        }
    }
}

/// Opens the card whose `device` folder is `dev`, by the first vendor that knows it.
fn open_card(root: &Root, dev: &Path) -> Option<Card> {
    amd::open(root, dev)
        .map(Card::Amd)
        .or_else(|| intel::open(dev).map(Card::Intel))
}

/// The graphics card source, with its files kept open.
pub struct Gpus {
    cards: Vec<Card>,
    /// Each card's PCI address, in the order of `cards`.
    pcis: Vec<Option<String>>,
    list: Vec<GpuSample>,
}

impl Gpus {
    pub fn open(root: &Root) -> Gpus {
        let mut cards = Vec::new();
        let mut pcis = Vec::new();
        for name in drm_cards(&root.path("sys/class/drm")) {
            let dev = root.path("sys/class/drm").join(name).join("device");
            if let Some(card) = open_card(root, &dev) {
                cards.push(card);
                pcis.push(pci_address(&dev));
            }
        }
        let list = cards.iter().map(Card::blank).collect();
        Gpus { cards, pcis, list }
    }

    /// Reads every card. With `shares` (the processes' GPU use from the last scan), the Intel
    /// cards whose use is unknown take their processes' sums; `None` leaves them unknown.
    pub fn read(&mut self, shares: Option<&[Share]>) -> &[GpuSample] {
        for (card, out) in self.cards.iter_mut().zip(self.list.iter_mut()) {
            card.read(out);
        }
        if let Some(shares) = shares {
            add_shares(&mut self.list, &self.pcis, shares);
        }
        &self.list
    }
}

/// The PCI address of the card whose `device` folder is `dev`: the basename of its symlink
/// target, such as `0000:03:00.0`.
fn pci_address(dev: &Path) -> Option<String> {
    let target = fs::read_link(dev).ok()?;
    Some(target.file_name()?.to_str()?.to_string())
}

/// Gives each Intel card whose use is unknown the sum of the GPU % of the processes on its PCI
/// address, capped at 100. A card with its own VRAM (`vram_total`) gets the sum of their VRAM
/// as its used VRAM. Other vendors' cards, and processes on no listed address, are left out.
pub fn add_shares(list: &mut [GpuSample], pcis: &[Option<String>], shares: &[Share]) {
    for (card, pci) in list.iter_mut().zip(pcis) {
        if card.vendor != "intel" || card.pct.is_some() {
            continue;
        }
        let Some(pci) = pci else {
            continue;
        };
        let (mut pct, mut vram) = (0f32, 0u64);
        for (_, p, v) in shares.iter().filter(|s| s.0 == *pci) {
            pct += p;
            vram = vram.saturating_add(*v);
        }
        card.pct = Some(pct.min(100.0));
        if card.vram_total.is_some() {
            card.vram_used = Some(vram);
        }
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
        let list = g.read(None).to_vec();
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
        assert!(Gpus::open(&root).read(None).is_empty());
    }

    fn sample(vendor: &'static str, pct: Option<f32>, vram_total: Option<u64>) -> GpuSample {
        GpuSample {
            name: String::from("made up"),
            vendor,
            pct,
            temp_c: None,
            power_w: None,
            vram_used: None,
            vram_total,
            fan_rpm: None,
        }
    }

    #[test]
    fn intel_cards_take_their_processes_sums_capped_and_others_stay() {
        let mut list = vec![
            sample("intel", None, Some(4_294_967_296)),
            sample("intel", None, None),
            sample("amd", None, Some(8_589_934_592)),
        ];
        let pcis = [
            Some(String::from("0000:00:02.0")),
            Some(String::from("0000:03:00.0")),
            Some(String::from("0000:04:00.0")),
        ];
        let shares: Vec<Share> = vec![
            (String::from("0000:00:02.0"), 30.0, 100),
            (String::from("0000:00:02.0"), 80.0, 50),
            (String::from("0000:03:00.0"), 5.0, 7),
            (String::from("0000:04:00.0"), 10.0, 9),
            (String::from("0000:09:00.0"), 50.0, 1),
        ];
        add_shares(&mut list, &pcis, &shares);

        // 30 + 80 is capped at 100; the discrete card gets the VRAM of its processes.
        assert_eq!((list[0].pct, list[0].vram_used), (Some(100.0), Some(150)));
        // An integrated card has no VRAM of its own to fill in.
        assert_eq!((list[1].pct, list[1].vram_used), (Some(5.0), None));
        // The AMD card is untouched, though a process is on its address.
        assert_eq!((list[2].pct, list[2].vram_used), (None, None));
    }
}
