//! The memory modules' type and speed from the SMBIOS table (type 17 structures). Pure: bytes
//! in, values out; only Windows reads the table, but the parser is built everywhere for tests.

/// Reads `N` bytes at `at`, or `None` past the end.
fn bytes<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    bytes::<2>(b, at).map(u16::from_le_bytes)
}

fn type_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => return None,
    })
}

/// A speed field: 0 (unknown) and 0xFFFF (see the extended field) are not values.
fn speed(v: Option<u16>) -> Option<u32> {
    v.filter(|&s| s != 0 && s != 0xFFFF).map(u32::from)
}

/// The memory type and speed (MT/s) from a raw SMBIOS buffer (the 8-byte header, then the
/// table). The speed is the largest configured one, else the largest rated one. Anything
/// malformed or cut short gives what was read before it, or `None`.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse(raw: &[u8]) -> (Option<String>, Option<u32>) {
    let mut kind: Option<&'static str> = None;
    let mut configured: Option<u32> = None;
    let mut rated: Option<u32> = None;
    let len = bytes::<4>(raw, 4).map_or(0, |l| u32::from_le_bytes(l) as usize);
    let table = raw.get(8..).unwrap_or(&[]);
    let table = table.get(..len).unwrap_or(table);
    let mut at = 0usize;
    while let (Some(&ty), Some(&flen)) = (table.get(at), table.get(at + 1)) {
        let flen = flen as usize;
        if ty == 127 || flen < 4 {
            break;
        }
        let Some(area) = table.get(at..at + flen) else {
            break;
        };
        if ty == 17 && u16_at(area, 0x0C).is_some_and(|s| s != 0) {
            if let Some(name) = area.get(0x12).and_then(|&c| type_name(c)) {
                kind = kind.or(Some(name));
            }
            rated = rated.max(speed(u16_at(area, 0x15)));
            if flen > 0x21 {
                configured = configured.max(speed(u16_at(area, 0x20)));
            }
        }
        // The strings: end at a double NUL.
        let mut end = at + flen;
        loop {
            match (table.get(end), table.get(end + 1)) {
                (Some(0), Some(0)) => {
                    end += 2;
                    break;
                }
                (Some(_), Some(_)) => end += 1,
                _ => return finish(kind, configured, rated),
            }
        }
        at = end;
    }
    finish(kind, configured, rated)
}

fn finish(
    kind: Option<&'static str>,
    configured: Option<u32>,
    rated: Option<u32>,
) -> (Option<String>, Option<u32>) {
    (kind.map(str::to_string), configured.or(rated))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn stick(size: u16, ty: u8, rated: u16, configured: u16) -> Vec<u8> {
        let mut s = vec![0u8; 0x22];
        s[0] = 17;
        s[1] = 0x22;
        s[0x0C..0x0E].copy_from_slice(&size.to_le_bytes());
        s[0x12] = ty;
        s[0x15..0x17].copy_from_slice(&rated.to_le_bytes());
        s[0x20..0x22].copy_from_slice(&configured.to_le_bytes());
        s.extend_from_slice(b"DIMM A\0\0");
        s
    }

    fn table() -> Vec<u8> {
        let mut t = Vec::new();
        t.extend(stick(0, 0x1A, 0, 0));
        t.extend(stick(8192, 0x1A, 3200, 3200));
        t.extend(stick(8192, 0x1A, 3200, 3200));
        t.extend_from_slice(&[127, 4, 0, 0, 0, 0]);
        let mut raw = vec![0, 3, 3, 0];
        raw.extend_from_slice(&(t.len() as u32).to_le_bytes());
        raw.extend(t);
        raw
    }

    #[test]
    fn two_ddr4_sticks_and_an_empty_slot() {
        assert_eq!(parse(&table()), (Some("DDR4".to_string()), Some(3200)));
    }

    #[test]
    fn configured_beats_rated_and_unknown_is_ignored() {
        let mut t = stick(8192, 0x22, 5600, 4800);
        t.extend(stick(8192, 0x22, 0xFFFF, 0xFFFF));
        let mut raw = vec![0, 3, 3, 0];
        raw.extend_from_slice(&(t.len() as u32).to_le_bytes());
        raw.extend(t);
        assert_eq!(parse(&raw), (Some("DDR5".to_string()), Some(4800)));
    }

    #[test]
    fn truncated_buffers_never_panic() {
        let full = table();
        for n in 0..full.len() {
            let _ = parse(&full[..n]);
        }
        assert_eq!(parse(&[]), (None, None));
        assert_eq!(parse(&full[..20]), (None, None));
    }
}
