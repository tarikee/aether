//! The rows of `aether port`: the same rows for the text table and for `--json`.

use std::io::{self, Write};

/// One row: a socket, or a v4 and a v6 socket of the same process shown as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortRow {
    /// `tcp` or `udp`.
    pub proto: &'static str,
    /// `v4`, `v6` or `both`.
    pub ip: &'static str,
    /// The address, or `*` for every address of both kinds.
    pub local: String,
    pub port: u16,
    /// The other end, for a connection.
    pub remote: Option<String>,
    pub remote_port: Option<u16>,
    pub state: &'static str,
    pub pid: Option<u32>,
    pub name: Option<String>,
    pub user: Option<String>,
}

/// An address with its port; a v6 address is in brackets.
pub fn endpoint(addr: &str, port: u16) -> String {
    if addr.contains(':') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    }
}

/// The line that ends the table when some process could not be seen.
pub const HIDDEN: &str = "some processes cannot be seen: running as root shows them";

/// The table. `detailed` adds the remote end and the state.
pub fn text(out: &mut dyn Write, rows: &[PortRow], detailed: bool, hidden: bool) -> io::Result<()> {
    let dash = || "\u{2014}".to_string();
    let mut table: Vec<Vec<String>> = vec![["PROTO", "LOCAL", "PID", "PROCESS", "USER"]
        .iter()
        .chain(if detailed {
            &["REMOTE", "STATE"][..]
        } else {
            &[][..]
        })
        .map(|s| (*s).to_string())
        .collect()];
    for r in rows {
        let mut line = vec![
            r.proto.to_string(),
            endpoint(&r.local, r.port),
            r.pid.map_or_else(dash, |p| p.to_string()),
            r.name.clone().unwrap_or_else(dash),
            r.user.clone().unwrap_or_else(dash),
        ];
        if detailed {
            line.push(match (&r.remote, r.remote_port) {
                (Some(a), Some(p)) => endpoint(a, p),
                _ => "-".to_string(),
            });
            line.push(r.state.to_string());
        }
        table.push(line);
    }
    let cols = table[0].len();
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            table
                .iter()
                .map(|l| l[c].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for line in &table {
        let mut text = String::new();
        for (c, cell) in line.iter().enumerate() {
            if c > 0 {
                text.push_str("  ");
            }
            text.push_str(cell);
            if c + 1 < cols {
                text.push_str(&" ".repeat(widths[c] - cell.chars().count()));
            }
        }
        writeln!(out, "{text}")?;
    }
    if hidden {
        writeln!(out, "{HIDDEN}")?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn row(port: u16, pid: Option<u32>) -> PortRow {
        PortRow {
            proto: "tcp",
            ip: "v6",
            local: "::1".to_string(),
            port,
            remote: None,
            remote_port: None,
            state: "LISTEN",
            pid,
            name: pid.map(|_| "web".to_string()),
            user: None,
        }
    }

    #[test]
    fn the_table_has_dashes_for_what_is_unseen_and_ends_with_the_root_line() {
        let mut out = Vec::new();
        text(&mut out, &[row(80, Some(7)), row(81, None)], false, true).unwrap();
        let t = String::from_utf8(out).unwrap();
        assert!(t.starts_with("PROTO  LOCAL"));
        assert!(t.contains("tcp    [::1]:80  7    web      \u{2014}\n"));
        assert!(t.contains("[::1]:81  \u{2014}    \u{2014}"));
        assert!(t.ends_with(&format!("{HIDDEN}\n")));
    }
}
