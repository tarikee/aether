//! Network: the hardware interfaces (filter layers and loopback left out) with their byte
//! counters, read one interface at a time. The list and the addresses are read on the first
//! reading and again on every 30th.

use super::calls;
use crate::sys::{NetSample, Root};
use std::net::IpAddr;
use std::time::Instant;

/// Network interfaces: rates and addresses.
pub struct Nets {
    list: Vec<NetSample>,
    /// (interface index, name), in list order.
    ifaces: Vec<(u32, String)>,
    found: Vec<(u32, String)>,
    last: Vec<Option<(u64, u64)>>,
    at: Option<Instant>,
    ticks: u64,
    addr_buf: Vec<u64>,
    addrs: Vec<(u32, IpAddr)>,
}

impl Nets {
    pub fn open(_root: &Root) -> Nets {
        Nets {
            list: Vec::new(),
            ifaces: Vec::new(),
            found: Vec::new(),
            last: Vec::new(),
            at: None,
            ticks: 0,
            addr_buf: Vec::new(),
            addrs: Vec::new(),
        }
    }

    pub fn read(&mut self) -> &[NetSample] {
        if self.ticks.is_multiple_of(30) {
            self.refresh();
        }
        self.ticks += 1;
        let now = Instant::now();
        let seconds = self.at.map(|a| now.duration_since(a).as_secs_f64());
        self.at = Some(now);
        for ((index, _), (sample, last)) in self
            .ifaces
            .iter()
            .zip(self.list.iter_mut().zip(self.last.iter_mut()))
        {
            let counts = calls::octets(*index);
            (sample.rx_bps, sample.tx_bps) = match (*last, counts, seconds) {
                (Some(a), Some(b), Some(s)) if s > 0.0 => (
                    Some((b.0.saturating_sub(a.0) as f64 / s) as u64),
                    Some((b.1.saturating_sub(a.1) as f64 / s) as u64),
                ),
                _ => (None, None),
            };
            *last = counts;
        }
        &self.list
    }

    /// Reads the interface list and the addresses again.
    fn refresh(&mut self) {
        if calls::interfaces(&mut self.found) && self.found != self.ifaces {
            std::mem::swap(&mut self.found, &mut self.ifaces);
            self.list.clear();
            self.list
                .extend(self.ifaces.iter().map(|(_, name)| NetSample {
                    name: name.clone(),
                    rx_bps: None,
                    tx_bps: None,
                    addrs: Vec::new(),
                }));
            self.last.clear();
            self.last.resize(self.ifaces.len(), None);
        }
        calls::addresses(&mut self.addr_buf, &mut self.addrs);
        attach(&mut self.list, &self.ifaces, &self.addrs);
    }
}

/// Gives each interface its addresses, link-local IPv6 ones left out.
fn attach(list: &mut [NetSample], ifaces: &[(u32, String)], addrs: &[(u32, IpAddr)]) {
    for (sample, (index, _)) in list.iter_mut().zip(ifaces) {
        sample.addrs.clear();
        sample.addrs.extend(
            addrs
                .iter()
                .filter(|(i, ip)| i == index && !link_local(ip))
                .map(|(_, ip)| ip.to_string()),
        );
    }
}

fn link_local(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn addresses_go_to_their_interface_without_link_local_ones() {
        let mut list = vec![NetSample {
            name: "Ethernet".into(),
            rx_bps: None,
            tx_bps: None,
            addrs: Vec::new(),
        }];
        let ifaces = [(7, "Ethernet".to_string())];
        let addrs = [
            (7, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))),
            (7, IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))),
            (
                7,
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x10)),
            ),
            (9, IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1))),
        ];
        attach(&mut list, &ifaces, &addrs);
        assert_eq!(list[0].addrs, ["192.0.2.10", "2001:db8::10"]);
    }

    #[test]
    fn rates_come_on_the_second_reading() {
        let mut nets = Nets::open(&Root::system());
        for s in nets.read() {
            assert!(s.rx_bps.is_none() && s.tx_bps.is_none());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        for s in nets.read() {
            assert!(!s.name.is_empty());
            assert!(s.rx_bps.is_some(), "{s:?}");
        }
    }
}
