//! One network socket, the same on every OS: what each OS folder's reader fills in.

use std::net::IpAddr;

/// One socket of the table: TCP or UDP, v4 or v6.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Socket {
    /// `tcp` or `udp`.
    pub proto: &'static str,
    pub v6: bool,
    pub local: IpAddr,
    pub port: u16,
    pub remote: IpAddr,
    pub remote_port: u16,
    /// `LISTEN`, `ESTABLISHED`, `TIME_WAIT`...; for UDP `UNCONN` or `ESTABLISHED`.
    pub state: &'static str,
    /// Waiting for connections: a TCP listener or an unconnected UDP socket.
    pub listening: bool,
    pub pid: Option<u32>,
    pub uid: Option<u32>,
    /// The kernel's socket number (0 when the OS gives none or no process holds it).
    pub inode: u64,
    /// No process holds it (a connection waiting to close), so none can be found.
    pub orphan: bool,
}

impl Socket {
    /// Whether the other end is unset.
    pub fn remote_unset(&self) -> bool {
        self.remote.is_unspecified() && self.remote_port == 0
    }
}

/// The name of a TCP state by its Linux number.
pub fn tcp_state(code: u8) -> &'static str {
    match code {
        1 => "ESTABLISHED",
        2 => "SYN_SENT",
        3 => "SYN_RECV",
        4 => "FIN_WAIT1",
        5 => "FIN_WAIT2",
        6 => "TIME_WAIT",
        7 => "CLOSE",
        8 => "CLOSE_WAIT",
        9 => "LAST_ACK",
        10 => "LISTEN",
        11 => "CLOSING",
        _ => "UNKNOWN",
    }
}
