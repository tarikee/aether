//! Sockets: `/proc/net/{tcp,tcp6,udp,udp6}` for the table, then `/proc/PID/fd` for the owners,
//! read only when asked and only until every wanted socket has one.

use super::super::socket::{tcp_state, Socket};
use super::super::Root;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The hex address of a `/proc/net` line: one native-endian word for v4, four for v6.
fn address(hex: &str) -> Option<IpAddr> {
    let word = |s: &str| u32::from_str_radix(s, 16).ok().map(u32::to_ne_bytes);
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(word(hex)?))),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                b[i * 4..i * 4 + 4].copy_from_slice(&word(hex.get(i * 8..i * 8 + 8)?)?);
            }
            Some(IpAddr::V6(Ipv6Addr::from(b)))
        }
        _ => None,
    }
}

/// `ADDR:PORT` of a `/proc/net` line.
fn endpoint(text: &str) -> Option<(IpAddr, u16)> {
    let (a, p) = text.split_once(':')?;
    Some((address(a)?, u16::from_str_radix(p, 16).ok()?))
}

/// The sockets of one `/proc/net` table.
pub fn parse_table(text: &str, proto: &'static str, v6: bool) -> Vec<Socket> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 {
            continue;
        }
        let (Some((local, port)), Some((remote, remote_port))) = (endpoint(f[1]), endpoint(f[2]))
        else {
            continue;
        };
        let code = u8::from_str_radix(f[3], 16).unwrap_or(0);
        let inode = f[9].parse::<u64>().unwrap_or(0);
        let (state, listening) = if proto == "tcp" {
            (tcp_state(code), code == 10)
        } else if code == 7 {
            ("UNCONN", true)
        } else {
            ("ESTABLISHED", false)
        };
        out.push(Socket {
            proto,
            v6,
            local,
            port,
            remote,
            remote_port,
            state,
            listening,
            pid: None,
            uid: f[7].parse().ok(),
            inode,
            orphan: inode == 0,
        });
    }
    out
}

/// Every TCP and UDP socket of the machine, without owners.
pub fn read_sockets(root: &Root) -> Option<Vec<Socket>> {
    let mut out = Vec::new();
    let mut any = false;
    for (file, proto, v6) in [
        ("proc/net/tcp", "tcp", false),
        ("proc/net/tcp6", "tcp", true),
        ("proc/net/udp", "udp", false),
        ("proc/net/udp6", "udp", true),
    ] {
        if let Ok(text) = std::fs::read_to_string(root.path(file)) {
            any = true;
            out.extend(parse_table(&text, proto, v6));
        }
    }
    any.then_some(out)
}

/// Sets the pid of every socket whose inode a readable `/proc/PID/fd` holds, and stops as soon
/// as all are found. A process that cannot be read is skipped.
pub fn attach_owners(root: &Root, sockets: &mut [Socket]) {
    let mut wanted: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, s) in sockets.iter().enumerate() {
        if s.inode != 0 {
            wanted.entry(s.inode).or_default().push(i);
        }
    }
    let Ok(dir) = std::fs::read_dir(root.path("proc")) else {
        return;
    };
    for entry in dir.flatten() {
        if wanted.is_empty() {
            return;
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let inode = target
                .to_str()
                .and_then(|t| t.strip_prefix("socket:["))
                .and_then(|t| t.strip_suffix(']'))
                .and_then(|t| t.parse::<u64>().ok());
            if let Some(list) = inode.and_then(|n| wanted.remove(&n)) {
                for i in list {
                    sockets[i].pid = Some(pid);
                }
                if wanted.is_empty() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0000000000000000 100 0 0 10 0\n   1: 0100007F:C350 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 0 1 0\n";

    #[test]
    fn a_tcp_table_is_read() {
        let s = parse_table(TCP, "tcp", false);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].port, 8080);
        assert_eq!(s[0].local, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(s[0].listening);
        assert_eq!((s[0].inode, s[0].uid), (4242, Some(1000)));
        assert_eq!(s[1].state, "ESTABLISHED");
        assert_eq!(s[1].remote_port, 8080);
        assert!(s[1].orphan);
    }

    #[test]
    fn a_v6_address_is_read() {
        let a = address("00000000000000000000000001000000").unwrap();
        assert_eq!(a, IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn own_listener_is_found_with_own_pid() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let root = Root::system();
        let mut all: Vec<Socket> = read_sockets(&root)
            .unwrap()
            .into_iter()
            .filter(|s| s.port == port && s.listening)
            .collect();
        attach_owners(&root, &mut all);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].pid, Some(std::process::id()));
    }
}
