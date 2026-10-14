//! Sockets: the TCP and UDP sockets of every process this program may look into, with the
//! owner. A process of another user cannot be looked into without root, so its sockets are
//! not in the table.

use super::super::socket::{tcp_state, Socket};
use super::super::Root;
use super::calls;

/// The Linux number of a macOS TCP state (0 closed, 1 listen, 2 syn sent, ...).
fn linux_code(state: u32) -> u8 {
    match state {
        1 => 10,
        2 => 2,
        3 => 3,
        4 => 1,
        5 => 8,
        6 => 4,
        7 => 11,
        8 => 9,
        9 => 5,
        10 => 6,
        _ => 7,
    }
}

/// Every TCP and UDP socket that can be seen, with the owning pid.
pub fn read_sockets(_root: &Root) -> Option<Vec<Socket>> {
    let mut pids = Vec::new();
    let count = calls::list_pids(&mut pids);
    if count == 0 {
        return None;
    }
    let mut fds = Vec::new();
    let mut out = Vec::new();
    for &raw in pids.iter().take(count) {
        let Ok(pid) = u32::try_from(raw) else {
            continue;
        };
        if pid == 0 {
            continue;
        }
        let uid = calls::bsd_info(pid).map(|b| b.pbi_uid);
        calls::process_sockets(pid, &mut fds, |r| {
            let (state, listening) = if r.udp {
                ("UNCONN", true)
            } else {
                let code = linux_code(r.state);
                (tcp_state(code), code == 10)
            };
            out.push(Socket {
                proto: if r.udp { "udp" } else { "tcp" },
                v6: r.v6,
                local: r.local,
                port: r.port,
                remote: r.remote,
                remote_port: r.remote_port,
                state,
                listening,
                pid: Some(pid),
                uid,
                inode: 0,
                orphan: false,
            });
        });
    }
    Some(out)
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
        assert_eq!(found[0].proto, "tcp");
        assert_eq!(found[0].state, "LISTEN");
    }

    #[test]
    fn own_udp_socket_is_found_with_own_pid() {
        let u = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = u.local_addr().unwrap().port();
        let found: Vec<Socket> = read_sockets(&Root::system())
            .unwrap()
            .into_iter()
            .filter(|s| s.port == port && s.proto == "udp")
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, Some(std::process::id()));
    }

    #[test]
    fn tcp_states_are_named() {
        assert_eq!(tcp_state(linux_code(1)), "LISTEN");
        assert_eq!(tcp_state(linux_code(4)), "ESTABLISHED");
        assert_eq!(tcp_state(linux_code(10)), "TIME_WAIT");
    }
}
