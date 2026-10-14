//! Network interfaces: rates from the counters `getifaddrs` gives with each link (32 bits wide,
//! so each reading adds the change to a 64-bit total), the link speed from `ifi_baudrate`
//! (0 means unknown). Listed are the Ethernet-like interfaces named `en*` that are up: Wi-Fi,
//! Ethernet, Thunderbolt and USB adapters.

use super::calls::{self, Link};
use crate::sys::{NetSample, Root};
use std::time::Instant;

/// What is kept of an interface between readings.
struct State {
    raw: (u32, u32),
    prev: Option<(u64, u64)>,
    total: (u64, u64),
}

/// The network source.
pub struct Nets {
    links: Vec<Link>,
    state: Vec<State>,
    list: Vec<NetSample>,
    last: Option<Instant>,
    ticks: u32,
    addrs: Vec<(String, String)>,
}

impl Nets {
    pub fn open(_root: &Root) -> Nets {
        let mut nets = Nets {
            links: Vec::new(),
            state: Vec::new(),
            list: Vec::new(),
            last: None,
            ticks: 0,
            addrs: Vec::new(),
        };
        calls::links(&mut nets.links);
        nets.sync();
        nets.refresh_addrs();
        nets
    }

    pub fn read(&mut self) -> &[NetSample] {
        let now = Instant::now();
        let secs = self.last.map(|t| now.duration_since(t).as_secs_f64());
        self.last = Some(now);
        if !calls::links(&mut self.links) {
            return &self.list;
        }
        if self.sync() {
            self.refresh_addrs();
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(30) {
            self.refresh_addrs();
        }
        let wanted = self.links.iter().filter(|l| listed(l));
        for ((link, st), s) in wanted.zip(&mut self.state).zip(&mut self.list) {
            st.total.0 += u64::from(link.rx.wrapping_sub(st.raw.0));
            st.total.1 += u64::from(link.tx.wrapping_sub(st.raw.1));
            st.raw = (link.rx, link.tx);
            let r = rate(st.prev, st.total, secs);
            s.rx_bps = r.map(|p| p.0);
            s.tx_bps = r.map(|p| p.1);
            s.link_bps = (link.baud > 0).then_some(u64::from(link.baud));
            st.prev = Some(st.total);
        }
        &self.list
    }

    /// Makes the list again when the listed interfaces differ from the last reading; true then.
    fn sync(&mut self) -> bool {
        let same = self
            .links
            .iter()
            .filter(|l| listed(l))
            .map(Link::name)
            .eq(self.list.iter().map(|s| s.name.as_str()));
        if same {
            return false;
        }
        self.list.clear();
        self.state.clear();
        for l in self.links.iter().filter(|l| listed(l)) {
            self.list.push(NetSample {
                name: l.name().to_string(),
                rx_bps: None,
                tx_bps: None,
                link_bps: (l.baud > 0).then_some(u64::from(l.baud)),
                addrs: Vec::new(),
            });
            self.state.push(State {
                raw: (l.rx, l.tx),
                prev: None,
                total: (0, 0),
            });
        }
        true
    }

    fn refresh_addrs(&mut self) {
        calls::addresses(&mut self.addrs);
        attach(&mut self.list, &self.addrs);
    }
}

/// Whether an interface is one to list.
fn listed(l: &Link) -> bool {
    l.ether && l.up && l.name().starts_with("en")
}

/// Bytes per second between two totals, `None` without an earlier one or a time.
fn rate(prev: Option<(u64, u64)>, now: (u64, u64), secs: Option<f64>) -> Option<(u64, u64)> {
    let (p, s) = (prev?, secs.filter(|s| *s > 0.0)?);
    Some((
        (now.0.saturating_sub(p.0) as f64 / s) as u64,
        (now.1.saturating_sub(p.1) as f64 / s) as u64,
    ))
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn rates_are_the_change_over_the_seconds() {
        assert_eq!(rate(None, (10, 10), Some(1.0)), None);
        assert_eq!(rate(Some((0, 0)), (10, 10), None), None);
        assert_eq!(rate(Some((100, 0)), (300, 50), Some(2.0)), Some((100, 25)));
    }

    #[test]
    fn a_wrapped_counter_adds_the_distance_it_travelled() {
        let before = u32::MAX - 9;
        let after = 90u32;
        assert_eq!(u64::from(after.wrapping_sub(before)), 100);
    }

    #[test]
    fn addresses_go_to_their_interface() {
        let mut list = vec![NetSample {
            name: "en0".into(),
            rx_bps: None,
            tx_bps: None,
            link_bps: None,
            addrs: vec!["old".into()],
        }];
        let addrs = [
            ("lo0".to_string(), "127.0.0.1".to_string()),
            ("en0".to_string(), "10.0.0.2".to_string()),
        ];
        attach(&mut list, &addrs);
        assert_eq!(list[0].addrs, ["10.0.0.2"]);
    }

    #[test]
    fn the_first_reading_has_no_rates_and_the_second_does() {
        let mut n = Nets::open(&Root::system());
        for s in n.read() {
            assert!(s.rx_bps.is_none() && s.tx_bps.is_none());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        for s in n.read() {
            assert!(s.rx_bps.is_some() && s.tx_bps.is_some(), "{}", s.name);
        }
    }
}
