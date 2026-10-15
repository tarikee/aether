//! Text by the cells it takes on a terminal: measuring and cutting names to fit.

/// The cells a character takes: two for the wide East Asian ranges and emoji, none for the
/// combining marks, one for the rest.
pub(crate) fn char_cells(c: char) -> usize {
    let u = u32::from(c);
    if matches!(u, 0x0300..=0x036F | 0x200B..=0x200F | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F)
    {
        return 0;
    }
    if matches!(u, 0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD)
    {
        return 2;
    }
    1
}

/// The cells a text takes on the screen.
pub(crate) fn cells(s: &str) -> usize {
    s.chars().map(char_cells).sum()
}

/// The longest start of `s` within `room` cells, and its cells.
pub(crate) fn fit(s: &str, room: usize) -> (&str, usize) {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        let w = char_cells(c);
        if used + w > room {
            return (&s[..i], used);
        }
        used += w;
    }
    (s, used)
}

/// Cuts `s` to `n` cells, the last one an ellipsis when it is cut.
pub(crate) fn cut(s: &str, n: usize) -> String {
    if cells(s) <= n {
        return s.to_string();
    }
    if n == 0 {
        return String::new();
    }
    let mut t = fit(s, n - 1).0.to_string();
    t.push('…');
    t
}

/// Cuts a name to `n` cells around an ellipsis: a head of about a third, the rest the tail.
pub(crate) fn cut_middle(s: &str, n: usize) -> String {
    if cells(s) <= n {
        return s.to_string();
    }
    if n < 3 {
        return cut(s, n);
    }
    let head_w = n / 3;
    let tail_w = n - 1 - head_w;
    let head = fit(s, head_w).0;
    let mut start = s.len();
    let mut used = 0;
    for (i, c) in s.char_indices().rev() {
        if used + char_cells(c) > tail_w {
            break;
        }
        used += char_cells(c);
        start = i;
    }
    format!("{head}…{}", &s[start..])
}
