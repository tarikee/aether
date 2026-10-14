//! `aether port [PORT] [--all] [--json]`: the listening sockets, or who holds one port.

use crate::cli::Parsed;
use crate::cmd::live::{finish, user_names};
use crate::out::json;
use crate::out::ports::{self, PortRow};
use aether::sys::{attach_owners, read_sockets, Proc, Procs, Root, Socket};
use std::collections::HashMap;
use std::io::{self, Write};
use std::process::ExitCode;

/// The port a command line word names: `8080` or `:8080`.
pub fn parse_port(text: &str) -> Option<u16> {
    text.strip_prefix(':').unwrap_or(text).parse().ok()
}

/// Whether a socket is asked for: the port's sockets, every socket, or the listening ones.
fn keep(s: &Socket, all: bool, port: Option<u16>) -> bool {
    match port {
        Some(p) => s.port == p,
        None => all || s.listening,
    }
}

/// The sockets asked for, with their owners found; `None` on a system not built yet.
fn read(root: &Root, all: bool, port: Option<u16>) -> Option<Vec<Socket>> {
    let mut sockets: Vec<Socket> = read_sockets(root)?
        .into_iter()
        .filter(|s| keep(s, all, port))
        .collect();
    attach_owners(root, &mut sockets);
    Some(sockets)
}

/// The address as shown: `*` when both kinds are meant.
fn shown(s: &Socket) -> String {
    s.local.to_string()
}

/// The rows: one per socket, a v4 and a v6 listener of one process on one port as one row,
/// sorted by port. The flag says some socket's process could not be seen.
pub fn rows(
    sockets: &[Socket],
    procs: &[Proc],
    users: &HashMap<u32, String>,
) -> (Vec<PortRow>, bool) {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut hidden = false;
    let mut merged = vec![false; sockets.len()];
    let mut out: Vec<PortRow> = Vec::new();
    for (i, s) in sockets.iter().enumerate() {
        if merged[i] {
            continue;
        }
        let mut ip = if s.v6 { "v6" } else { "v4" };
        let mut local = shown(s);
        if s.v6 && s.listening && s.local.is_unspecified() {
            let twin = sockets.iter().enumerate().find(|(j, t)| {
                *j != i
                    && !merged[*j]
                    && !t.v6
                    && t.listening
                    && t.local.is_unspecified()
                    && t.proto == s.proto
                    && t.port == s.port
                    && t.pid == s.pid
            });
            if let Some((j, _)) = twin {
                merged[j] = true;
                ip = "both";
                local = "*".to_string();
            }
        } else if !s.v6 && s.listening && s.local.is_unspecified() {
            let twin = sockets.iter().enumerate().find(|(j, t)| {
                *j > i
                    && t.v6
                    && t.listening
                    && t.local.is_unspecified()
                    && t.proto == s.proto
                    && t.port == s.port
                    && t.pid == s.pid
            });
            if let Some((j, _)) = twin {
                merged[j] = true;
                ip = "both";
                local = "*".to_string();
            }
        }
        let process = s.pid.and_then(|p| by_pid.get(&p).copied());
        if s.pid.is_none() && !s.orphan {
            hidden = true;
        }
        let uid = s
            .uid
            .or_else(|| process.map(|p| p.uid))
            .filter(|u| *u != u32::MAX);
        let remote = !s.remote_unset();
        out.push(PortRow {
            proto: s.proto,
            ip,
            local,
            port: s.port,
            remote: remote.then(|| s.remote.to_string()),
            remote_port: remote.then_some(s.remote_port),
            state: s.state,
            pid: s.pid,
            name: process.map(|p| p.name.clone()),
            user: uid.map(|u| users.get(&u).cloned().unwrap_or_else(|| u.to_string())),
        });
    }
    out.sort_by(|a, b| {
        (a.port, a.proto, a.state, &a.local).cmp(&(b.port, b.proto, b.state, &b.local))
    });
    (out, hidden)
}

/// Lists the sockets.
pub fn run(parsed: &Parsed) -> ExitCode {
    let port = match parsed.target.as_deref() {
        None => None,
        Some(t) => match parse_port(t) {
            Some(p) => Some(p),
            None => {
                eprintln!("aether: {t} is not a port number (0 to 65535)");
                return ExitCode::from(2);
            }
        },
    };
    let root = Root::system();
    let Some(sockets) = read(&root, parsed.view.all, port) else {
        eprintln!("aether: port: not built for this OS yet");
        return ExitCode::FAILURE;
    };
    let mut procs = Procs::open(&root);
    let (rows, hidden) = rows(&sockets, procs.read(), &user_names(&root));
    let mut out = io::stdout().lock();
    if parsed.json {
        let mut buf = Vec::new();
        json::write_ports(&rows, hidden, &mut buf);
        return finish(out.write_all(&buf));
    }
    if rows.is_empty() {
        let what = match port {
            Some(p) => format!("nothing on port {p}"),
            None => "no listening sockets".to_string(),
        };
        return finish(writeln!(out, "{what}"));
    }
    finish(ports::text(
        &mut out,
        &rows,
        parsed.view.all || port.is_some(),
        hidden,
    ))
}

/// The processes listening on a port as (pid, name), by pid; or why there are none.
pub(crate) fn listeners(root: &Root, port: u16) -> Result<Vec<(u32, String)>, String> {
    let Some(sockets) = read(root, false, Some(port)) else {
        return Err("ports are not built for this OS yet".to_string());
    };
    let sockets: Vec<Socket> = sockets.into_iter().filter(|s| s.listening).collect();
    let mut procs = Procs::open(root);
    let list = procs.read();
    let mut found: Vec<(u32, String)> = Vec::new();
    for pid in sockets.iter().filter_map(|s| s.pid) {
        if found.iter().all(|(p, _)| *p != pid) {
            let name = list
                .iter()
                .find(|p| p.pid == pid)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            found.push((pid, name));
        }
    }
    found.sort_unstable_by_key(|(pid, _)| *pid);
    if found.is_empty() {
        return Err(if sockets.is_empty() {
            format!("nothing is listening on port {port}")
        } else {
            format!(
                "port {port} is held by a process that cannot be seen: running as root shows it"
            )
        });
    }
    Ok(found)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn sock(v6: bool, port: u16, pid: Option<u32>) -> Socket {
        Socket {
            proto: "tcp",
            v6,
            local: if v6 {
                IpAddr::V6(Ipv6Addr::UNSPECIFIED)
            } else {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            },
            port,
            remote: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            remote_port: 0,
            state: "LISTEN",
            listening: true,
            pid,
            uid: Some(0),
            inode: 1,
            orphan: false,
        }
    }

    #[test]
    fn ports_are_numbers_with_or_without_a_colon() {
        assert_eq!(parse_port("80"), Some(80));
        assert_eq!(parse_port(":8080"), Some(8080));
        assert_eq!(parse_port("70000"), None);
        assert_eq!(parse_port("web"), None);
    }

    #[test]
    fn v4_and_v6_of_one_process_are_one_row_sorted_by_port() {
        let list = vec![
            sock(false, 90, Some(5)),
            sock(true, 22, Some(4)),
            sock(true, 90, Some(5)),
            sock(false, 22, Some(9)),
        ];
        let (r, hidden) = rows(&list, &[], &HashMap::new());
        assert!(!hidden);
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].port, r[0].ip), (22, "v4"));
        assert_eq!((r[1].port, r[1].ip), (22, "v6"));
        assert_eq!((r[2].port, r[2].ip, r[2].local.as_str()), (90, "both", "*"));
    }

    #[test]
    fn an_unseen_owner_sets_the_hidden_flag() {
        let (r, hidden) = rows(&[sock(false, 1, None)], &[], &HashMap::new());
        assert!(hidden);
        assert_eq!(r[0].pid, None);
    }
}
