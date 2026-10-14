//! Users: a process's account is a SID, read once per process from its token. The uid shown is
//! the SID's relative id for accounts (S-1-5-21-...-1001 is 1001) and the well-known ones
//! (SYSTEM 18, LOCAL SERVICE 19, NETWORK SERVICE 20); any other SID gets a stable number with
//! the top bit set. A token that cannot be read (another account's process, without
//! administrator rights) is `UNKNOWN`.

use super::calls;
use std::collections::HashMap;

/// The uid of a process whose account could not be read.
pub const UNKNOWN: u32 = u32::MAX;

/// The uid of a process's account.
pub fn uid_of(pid: u32) -> u32 {
    calls::process_sid(pid).map_or(UNKNOWN, |sid| uid_of_sid(&sid))
}

/// The uid for a SID in its binary form: revision, count, a 6-byte authority (big-endian),
/// then the sub-authorities (4 bytes each, little-endian).
pub fn uid_of_sid(sid: &[u8]) -> u32 {
    let count = usize::from(sid.get(1).copied().unwrap_or(0));
    let sub = |i: usize| {
        sid.get(8 + 4 * i..12 + 4 * i)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_le_bytes)
    };
    let nt = sid.get(2..8) == Some(&[0, 0, 0, 0, 0, 5][..]);
    if nt && sid.len() == 8 + 4 * count {
        if count == 1 {
            if let Some(n) = sub(0) {
                return n;
            }
        }
        if count >= 2 && sub(0) == Some(21) {
            if let Some(rid) = sub(count - 1) {
                return rid;
            }
        }
    }
    // FNV-1a over the bytes, with the top bit set so it never meets a relative id.
    let hash = sid.iter().fold(0x811c_9dc5u32, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    (hash | 0x8000_0000).min(UNKNOWN - 1)
}

/// The names of the accounts the running processes belong to, by uid; read once at the start
/// of the view. An account that could not be read is "?".
pub fn names() -> HashMap<u32, String> {
    let mut pids = Vec::new();
    let mut buf = Vec::new();
    calls::processes(&mut buf, |p| pids.push(p.pid));
    let mut names = HashMap::new();
    for pid in pids.into_iter().filter(|&p| p != 0) {
        let Some(sid) = calls::process_sid(pid) else {
            continue;
        };
        let uid = uid_of_sid(&sid);
        if let std::collections::hash_map::Entry::Vacant(e) = names.entry(uid) {
            if let Some(name) = calls::account_name(&sid) {
                e.insert(name);
            }
        }
    }
    names.insert(UNKNOWN, "?".to_string());
    names
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn sid(authority: u8, subs: &[u32]) -> Vec<u8> {
        let mut b = vec![1, subs.len() as u8, 0, 0, 0, 0, 0, authority];
        for s in subs {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    #[test]
    fn accounts_give_their_relative_id() {
        assert_eq!(uid_of_sid(&sid(5, &[18])), 18);
        assert_eq!(uid_of_sid(&sid(5, &[20])), 20);
        assert_eq!(uid_of_sid(&sid(5, &[21, 1, 2, 3, 1001])), 1001);
        let dwm = uid_of_sid(&sid(5, &[90, 0, 1]));
        let font = uid_of_sid(&sid(5, &[96, 0, 1]));
        assert!(dwm >= 0x8000_0000 && font >= 0x8000_0000 && dwm != font);
        assert_eq!(dwm, uid_of_sid(&sid(5, &[90, 0, 1])), "stable");
        assert!(uid_of_sid(&[1]) >= 0x8000_0000);
    }

    #[test]
    fn this_program_has_a_named_account() {
        let uid = uid_of(std::process::id());
        assert_ne!(uid, UNKNOWN);
        let names = names();
        assert!(names.get(&uid).is_some_and(|n| !n.is_empty()), "{names:?}");
    }
}
