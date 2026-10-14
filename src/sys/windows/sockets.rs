//! Sockets: `GetExtendedTcpTable` and `GetExtendedUdpTable`, which give the owning pid.

use super::super::socket::{tcp_state, Socket};
use super::super::Root;
use super::calls;

/// The Linux number of a Windows TCP state (1 closed, 2 listen, 3 syn sent, ...).
fn linux_code(state: u32) -> u8 {
    match state {
        2 => 10,
        3 => 2,
        4 => 3,
        5 => 1,
        6 => 4,
        7 => 5,
        8 => 8,
        9 => 11,
        10 => 9,
        11 => 6,
        _ => 7,
    }
}

/// Every TCP and UDP socket of the machine, with the owning pid.
pub fn read_sockets(_root: &Root) -> Option<Vec<Socket>> {
    let mut raw = Vec::new();
    if !calls::sockets(&mut raw) {
        return None;
    }
    Some(
        raw.into_iter()
            .map(|r| {
                let (state, listening) = if r.udp {
                    ("UNCONN", true)
                } else {
                    let code = linux_code(r.state);
                    (tcp_state(code), code == 10)
                };
                Socket {
                    proto: if r.udp { "udp" } else { "tcp" },
                    v6: r.local.is_ipv6(),
                    local: r.local,
                    port: r.port,
                    remote: r.remote,
                    remote_port: r.remote_port,
                    state,
                    listening,
                    pid: (r.pid != 0).then_some(r.pid),
                    uid: None,
                    inode: 0,
                    orphan: r.pid == 0,
                }
            })
            .collect(),
    )
}

/// The owners came with the table.
pub fn attach_owners(_root: &Root, _sockets: &mut [Socket]) {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn own_listener_is_found_with_own_pid() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let found: Vec<Socket> = read_sockets(&Root::system())
            .unwrap()
            .into_iter()
            .filter(|s| s.port == port && s.listening)
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, Some(std::process::id()));
    }
}
