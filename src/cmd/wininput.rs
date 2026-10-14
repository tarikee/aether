//! The Windows console's input events as the bytes a terminal would send, so the view reads
//! keys and mouse the same way on every system. Pure: numbers in, bytes out. The calls that read
//! the console are in `sys/windows/calls.rs`; this file is built everywhere so its tests run on
//! every system, but only Windows uses it.

#![cfg_attr(not(windows), allow(dead_code))]

const VK_BACK: u16 = 0x08;
const VK_RETURN: u16 = 0x0d;
const VK_ESCAPE: u16 = 0x1b;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;

/// The mouse button bits of a console mouse event, and its wheel flag.
const LEFT: u32 = 0x1;
const RIGHT: u32 = 0x2;
const MIDDLE: u32 = 0x4;
const WHEELED: u32 = 0x4;

/// A key event: the arrows and the page, home and end keys as their escape sequences, Esc as a
/// lone escape, Enter and Backspace as their bytes, a character as its UTF-8. Key-up events and
/// keys the view has no use for give nothing.
pub(crate) fn key_bytes(down: bool, vk: u16, ch: u16, out: &mut Vec<u8>) {
    if !down {
        return;
    }
    let seq: Option<&[u8]> = match vk {
        VK_UP => Some(b"\x1b[A"),
        VK_DOWN => Some(b"\x1b[B"),
        VK_RIGHT => Some(b"\x1b[C"),
        VK_LEFT => Some(b"\x1b[D"),
        VK_PRIOR => Some(b"\x1b[5~"),
        VK_NEXT => Some(b"\x1b[6~"),
        VK_HOME => Some(b"\x1b[H"),
        VK_END => Some(b"\x1b[F"),
        VK_ESCAPE => Some(b"\x1b"),
        VK_RETURN => Some(b"\r"),
        VK_BACK => Some(&[0x7f]),
        _ => None,
    };
    if let Some(s) = seq {
        out.extend_from_slice(s);
        return;
    }
    if ch == 0 {
        return;
    }
    if let Some(c) = char::from_u32(u32::from(ch)) {
        let mut b = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
    }
}

/// A mouse event as an SGR mouse report. `prev` is the buttons held at the last event: a press
/// is a button newly down. `x` and `y` count cells from 0; the wheel's direction is the sign
/// of the high word of `buttons`.
pub(crate) fn mouse_bytes(
    prev: &mut u32,
    x: i16,
    y: i16,
    buttons: u32,
    flags: u32,
    out: &mut Vec<u8>,
) {
    let (x, y) = (i32::from(x.max(0)) + 1, i32::from(y.max(0)) + 1);
    if flags & WHEELED != 0 {
        let delta = (buttons >> 16) as u16 as i16;
        let code = if delta > 0 { 64 } else { 65 };
        out.extend_from_slice(format!("\x1b[<{code};{x};{y}M").as_bytes());
        return;
    }
    let held = buttons & (LEFT | RIGHT | MIDDLE);
    let new = held & !*prev;
    *prev = held;
    for (bit, code) in [(LEFT, 0), (MIDDLE, 1), (RIGHT, 2)] {
        if new & bit != 0 {
            out.extend_from_slice(format!("\x1b[<{code};{x};{y}M").as_bytes());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cmd::keys::{next_input, Click, Input, Key, Mouse};

    fn key(vk: u16, ch: u16) -> Vec<u8> {
        let mut out = Vec::new();
        key_bytes(true, vk, ch, &mut out);
        out
    }

    #[test]
    fn keys_become_the_bytes_a_terminal_sends() {
        assert_eq!(key(VK_UP, 0), b"\x1b[A");
        assert_eq!(key(VK_NEXT, 0), b"\x1b[6~");
        assert_eq!(key(VK_ESCAPE, 0x1b), b"\x1b");
        assert_eq!(key(0x51, u16::from(b'q')), b"q");
        assert_eq!(key(0x41, 0xe9), "é".as_bytes());
        assert_eq!(key(VK_RETURN, 13), b"\r");
        assert_eq!(key(VK_BACK, 8), [0x7f]);
        // A modifier alone, or a key with no character, gives nothing.
        assert_eq!(key(0x10, 0), b"");
        let mut out = Vec::new();
        key_bytes(false, VK_UP, 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn the_bytes_decode_to_the_same_keys() {
        let (k, n) = next_input(&key(VK_HOME, 0)).unwrap();
        assert_eq!((k, n), (Some(Input::Key(Key::Home)), 3));
        let (k, _) = next_input(&key(VK_ESCAPE, 0x1b)).unwrap();
        assert_eq!(k, Some(Input::Key(Key::Esc)));
    }

    #[test]
    fn a_press_is_a_report_once_and_the_wheel_has_a_direction() {
        let mut prev = 0;
        let mut out = Vec::new();
        mouse_bytes(&mut prev, 4, 2, LEFT, 0, &mut out);
        assert_eq!(out, b"\x1b[<0;5;3M");
        // The same button still held (a move with it down) is no new press.
        out.clear();
        mouse_bytes(&mut prev, 5, 2, LEFT, 1, &mut out);
        assert!(out.is_empty());
        // A double click arrives as a second press, after the release.
        mouse_bytes(&mut prev, 5, 2, 0, 0, &mut out);
        assert!(out.is_empty());
        mouse_bytes(&mut prev, 5, 2, LEFT, 2, &mut out);
        assert_eq!(out, b"\x1b[<0;6;3M");
        out.clear();
        mouse_bytes(&mut prev, 0, 0, 120 << 16, WHEELED, &mut out);
        assert_eq!(out, b"\x1b[<64;1;1M");
        out.clear();
        mouse_bytes(
            &mut prev,
            0,
            0,
            (-120i16 as u16 as u32) << 16,
            WHEELED,
            &mut out,
        );
        let (m, _) = next_input(&out).unwrap();
        assert_eq!(
            m,
            Some(Input::Mouse(Mouse {
                click: Click::WheelDown,
                x: 0,
                y: 0
            }))
        );
    }
}
