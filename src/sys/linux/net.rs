//! Network interfaces: rates from proc/net/dev. Only physical interfaces are listed.

use std::path::{Path, PathBuf};
use std::time::Instant;

use super::calls;
use super::disk::rate;
use super::reader::{fields, lines, num, PFile};
use crate::sys::{NetSample, Root};

const NET_CAP: usize = 16 * 1024;

/// An interface seen in proc/net/dev, with its counters (received, sent bytes).
struct Iface {
    name: String,
    physical: bool,
    prev: Option<(u64, u64)>,
    cur: Option<(u64, u64)>,
}

/// The network source, with its files kept open.
pub struct Nets {
    file: Option<PFile>,
    net_dir: PathBuf,
    seen: Vec<Iface>,
    list: Vec<NetSample>,
    last: Option<Instant>,
    /// Addresses are read only on a live system, and again on every 30th reading.
    live: bool,
    ticks: u32,
    addrs: Vec<(String, String)>,
}

impl Nets {
    pub fn open(root: &Root) -> Nets {
        let mut nets = Nets {
            file: PFile::open(&root.path("proc/net/dev"), NET_CAP),
            net_dir: root.path("sys/class/net"),
            seen: Vec::new(),
            list: Vec::new(),
            last: None,
            live: root.base() == Path::new("/"),
            ticks: 0,
            addrs: Vec::new(),
        };
        // The names are learned and the list built here; a tick only updates numbers.
        nets.update();
        nets.sync_list();
        nets.refresh_addrs();
        nets
    }

    pub fn read(&mut self) -> &[NetSample] {
        let now = Instant::now();
        let secs = self.last.map(|t| now.duration_since(t).as_secs_f64());
        self.last = Some(now);

        self.update();
        self.sync_list();
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(30) {
            self.refresh_addrs();
            self.refresh_links();
        }
        for (s, i) in self
            .list
            .iter_mut()
            .zip(self.seen.iter().filter(|i| is_present(i)))
        {
            let r = rate(i.prev, i.cur, secs);
            s.rx_bps = r.map(|p| p.0);
            s.tx_bps = r.map(|p| p.1);
        }
        for i in &mut self.seen {
            i.prev = i.cur;
        }
        &self.list
    }

    /// Reads the counters of proc/net/dev. An interface seen for the first time is kept, and
    /// its name is built only then.
    fn update(&mut self) {
        for i in &mut self.seen {
            i.cur = None;
        }
        if let Some(f) = self.file.as_mut() {
            // Two header lines, then one line per interface: `name: rx_bytes … tx_bytes …`.
            for line in lines(f.read()).skip(2) {
                let Some(colon) = line.iter().position(|&b| b == b':') else {
                    continue;
                };
                let Some(name) = fields(&line[..colon]).next() else {
                    continue;
                };
                let mut it = fields(&line[colon + 1..]);
                let (Some(rx), Some(tx)) = (it.next().and_then(num), it.nth(7).and_then(num))
                else {
                    continue;
                };
                let pos = self.seen.iter().position(|i| i.name.as_bytes() == name);
                match pos {
                    Some(p) => self.seen[p].cur = Some((rx, tx)),
                    None => {
                        let name = String::from_utf8_lossy(name).into_owned();
                        let physical = self.net_dir.join(&name).join("device").exists();
                        self.seen.push(Iface {
                            name,
                            physical,
                            prev: None,
                            cur: Some((rx, tx)),
                        });
                    }
                }
            }
        }
        // Virtual interfaces that have gone are forgotten; physical ones keep their place.
        self.seen.retain(|i| i.physical || i.cur.is_some());
    }

    /// Reads the system's addresses again. Never under a fixture root.
    fn refresh_addrs(&mut self) {
        if !self.live {
            return;
        }
        calls::addresses(&mut self.addrs);
        attach(&mut self.list, &self.addrs);
    }

    /// Reads each listed interface's link speed again (every 30th reading).
    fn refresh_links(&mut self) {
        for s in &mut self.list {
            let path = self.net_dir.join(&s.name).join("speed");
            s.link_bps = std::fs::read_to_string(path)
                .ok()
                .and_then(|t| link_bps(&t));
        }
    }

    /// Rebuilds the list only when the physical interfaces present have changed.
    fn sync_list(&mut self) {
        let same = self
            .seen
            .iter()
            .filter(|i| is_present(i))
            .map(|i| i.name.as_str())
            .eq(self.list.iter().map(|s| s.name.as_str()));
        if !same {
            self.list = self
                .seen
                .iter()
                .filter(|i| is_present(i))
                .map(|i| NetSample {
                    name: i.name.clone(),
                    rx_bps: None,
                    tx_bps: None,
                    link_bps: None,
                    addrs: Vec::new(),
                })
                .collect();
            attach(&mut self.list, &self.addrs);
            self.refresh_links();
        }
    }
}

/// The text of sys/class/net/<if>/speed (Mbit/s; -1 when unknown) as bits per second.
fn link_bps(text: &str) -> Option<u64> {
    let mbit: i64 = text.trim().parse().ok()?;
    u64::try_from(mbit)
        .ok()
        .filter(|&m| m > 0)
        .map(|m| m * 1_000_000)
}

/// Gives each listed interface the addresses the system reported for it.
fn attach(list: &mut [NetSample], addrs: &[(String, String)]) {
    for s in list.iter_mut() {
        s.addrs.clear();
        s.addrs.extend(
            addrs
                .iter()
                .filter(|(n, _)| *n == s.name)
                .map(|(_, a)| a.clone()),
        );
    }
}

/// A physical interface that is in this reading.
fn is_present(i: &Iface) -> bool {
    i.physical && i.cur.is_some()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn root() -> Root {
        Root::at(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/disks"))
    }

    #[test]
    fn the_first_reading_has_no_rates_and_lists_physical_only() {
        let mut nets = Nets::open(&root());
        let list = nets.read().to_vec();
        let names: Vec<&str> = list.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["enp3s0", "wlan0"]);
        assert!(list
            .iter()
            .all(|n| n.rx_bps.is_none() && n.tx_bps.is_none()));
    }

    #[test]
    fn the_second_reading_of_an_unchanged_file_has_zero_rates() {
        let mut nets = Nets::open(&root());
        nets.read();
        std::thread::sleep(Duration::from_millis(5));
        let list = nets.read().to_vec();
        let eth = list.iter().find(|n| n.name == "enp3s0").expect("enp3s0");
        assert_eq!(eth.rx_bps, Some(0));
        assert_eq!(eth.tx_bps, Some(0));
    }

    #[test]
    fn the_link_speed_is_megabits_and_an_error_is_none() {
        assert_eq!(link_bps("1000\n"), Some(1_000_000_000));
        assert_eq!(link_bps("-1\n"), None);
        assert_eq!(link_bps("0"), None);
        assert_eq!(link_bps("x"), None);
    }

    #[test]
    fn addresses_are_attached_by_name_and_not_read_under_a_fixture() {
        let mut list = vec![
            NetSample {
                name: "enp3s0".into(),
                rx_bps: None,
                tx_bps: None,
                link_bps: None,
                addrs: Vec::new(),
            },
            NetSample {
                name: "wlan0".into(),
                rx_bps: None,
                tx_bps: None,
                link_bps: None,
                addrs: Vec::new(),
            },
        ];
        let addrs = vec![
            ("enp3s0".to_string(), "192.0.2.10".to_string()),
            ("enp3s0".to_string(), "2001:db8::10".to_string()),
            ("lo".to_string(), "127.0.0.1".to_string()),
        ];
        attach(&mut list, &addrs);
        assert_eq!(list[0].addrs, ["192.0.2.10", "2001:db8::10"]);
        assert!(list[1].addrs.is_empty());
        assert!(Nets::open(&root())
            .read()
            .iter()
            .all(|n| n.addrs.is_empty()));
    }

    #[test]
    fn a_counter_that_moves_gives_a_rate() {
        // The rate itself is the shared `rate` of disk.rs, tested there; here the bytes
        // per second of one interface from the fixture's own numbers.
        let prev = Some((5_000_000u64, 2_000_000u64));
        let cur = Some((7_000_000u64, 2_500_000u64));
        assert_eq!(rate(prev, cur, Some(2.0)), Some((1_000_000, 250_000)));
    }
}
