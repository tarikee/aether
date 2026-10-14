//! The keys of the live view. Pure: bytes in, a key out; a key and the state in, the change out.
//! Nothing here reads the terminal, sends a signal or reads a process: the live loop does that.

use crate::cli::{Sort, View};
use crate::out::screen::{Hits, Overlay};
use std::time::{Duration, Instant};

/// One key the view acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    Backspace,
    Interrupt,
    Char(char),
}

/// The next key at the start of `buf`, and the bytes it took. `None` for a byte that is no
/// key (a control code the view ignores, or a sequence it does not know) with the bytes it took.
pub(crate) fn next(buf: &[u8]) -> Option<(Option<Key>, usize)> {
    let &b = buf.first()?;
    Some(match b {
        0x1b => escape(buf),
        b'\r' | b'\n' => (Some(Key::Enter), 1),
        0x7f | 0x08 => (Some(Key::Backspace), 1),
        0x03 => (Some(Key::Interrupt), 1),
        0x00..=0x1f => (None, 1),
        _ => {
            let len = utf8_len(b).min(buf.len());
            match std::str::from_utf8(&buf[..len])
                .ok()
                .and_then(|s| s.chars().next())
            {
                Some(c) => (Some(Key::Char(c)), len),
                None => (None, 1),
            }
        }
    })
}

/// What the mouse did: a press of a button, or the wheel. Cells count from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Click {
    Left,
    Other,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mouse {
    pub click: Click,
    pub x: usize,
    pub y: usize,
}

/// One thing the user did: a key or the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    Key(Key),
    Mouse(Mouse),
}

/// The next input at the start of `buf`: an SGR mouse report (`ESC [ < b ; x ; y M`) or a key.
/// A release, a drag and a report that is cut short give `None` with the bytes they took.
pub(crate) fn next_input(buf: &[u8]) -> Option<(Option<Input>, usize)> {
    if buf.starts_with(b"\x1b[<") {
        let end = buf.iter().position(|c| matches!(c, b'M' | b'm'));
        let Some(end) = end else {
            return Some((None, buf.len()));
        };
        let text = std::str::from_utf8(&buf[3..end]).unwrap_or("");
        let mut nums = text.split(';').map(|n| n.parse::<usize>().ok());
        let input = match (
            nums.next().flatten(),
            nums.next().flatten(),
            nums.next().flatten(),
        ) {
            (Some(b), Some(x), Some(y)) if buf[end] == b'M' && b & 32 == 0 && x > 0 && y > 0 => {
                let click = match b {
                    0 => Some(Click::Left),
                    1 | 2 => Some(Click::Other),
                    64 => Some(Click::WheelUp),
                    65 => Some(Click::WheelDown),
                    _ => None,
                };
                click.map(|click| {
                    Input::Mouse(Mouse {
                        click,
                        x: x - 1,
                        y: y - 1,
                    })
                })
            }
            _ => None,
        };
        return Some((input, end + 1));
    }
    next(buf).map(|(k, n)| (k.map(Input::Key), n))
}

fn utf8_len(b: u8) -> usize {
    match b {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// An escape: alone it is Esc; `ESC [` or `ESC O` starts a sequence that ends at its final byte.
fn escape(buf: &[u8]) -> (Option<Key>, usize) {
    match buf.get(1) {
        None => (Some(Key::Esc), 1),
        Some(b'[') | Some(b'O') => {
            let end = buf
                .iter()
                .skip(2)
                .position(|c| (0x40..=0x7e).contains(c))
                .map_or(buf.len(), |p| p + 3);
            if end > buf.len() {
                return (None, buf.len());
            }
            let fin = buf[end - 1];
            let params = &buf[2..end - 1];
            let key = match (fin, params) {
                (b'A', _) => Some(Key::Up),
                (b'B', _) => Some(Key::Down),
                (b'C', _) => Some(Key::Right),
                (b'D', _) => Some(Key::Left),
                (b'H', _) => Some(Key::Home),
                (b'F', _) => Some(Key::End),
                (b'~', b"1") | (b'~', b"7") => Some(Key::Home),
                (b'~', b"4") | (b'~', b"8") => Some(Key::End),
                (b'~', b"5") => Some(Key::PageUp),
                (b'~', b"6") => Some(Key::PageDown),
                _ => None,
            };
            (key, end)
        }
        Some(_) => (Some(Key::Esc), 1),
    }
}

/// The key a hint in the bottom border names.
fn hint_key(name: &str) -> Option<Key> {
    Some(match name {
        "↑" => Key::Up,
        "↓" => Key::Down,
        "←" => Key::Left,
        "→" => Key::Right,
        "enter" => Key::Enter,
        "space" => Key::Char(' '),
        other => {
            let mut c = other.chars();
            match (c.next(), c.next()) {
                (Some(ch), None) => Key::Char(ch),
                _ => return None,
            }
        }
    })
}

/// The sort keys the left and right arrows step through, in order.
const SORTS: [Sort; 9] = [
    Sort::Cpu,
    Sort::Mem,
    Sort::Gpu,
    Sort::Disk,
    Sort::Pid,
    Sort::User,
    Sort::Threads,
    Sort::Name,
    Sort::Age,
];

/// The shortest and longest interval the keys reach, in seconds.
const INTERVAL_MIN: f64 = 0.5;
const INTERVAL_MAX: f64 = 10.0;
const INTERVAL_STEP: f64 = 0.5;

/// Which row a process is, across readings: its name and, for a single process, its pid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ident {
    pub name: String,
    pub pid: Option<u32>,
}

/// What the view is doing with the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    /// Typing the filter; the draft is kept in `Ui::draft`.
    Filter,
    Help,
    Sensors,
    Detail,
    /// Asking whether to end the selected row; `force` is SIGKILL.
    Confirm {
        force: bool,
    },
}

/// What the live loop must do after a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Quit,
    /// End the process named by the row: the pid of one process, or every process of the name.
    End {
        pid: Option<u32>,
        name: String,
        force: bool,
    },
}

/// The view's state that the keys change. `view` is the options as the view shows them now.
#[derive(Clone, Debug)]
pub(crate) struct Ui {
    pub mode: Mode,
    pub view: View,
    /// The selected row by index, and the row's identity so the selection follows it.
    pub index: usize,
    pub ident: Option<Ident>,
    pub paused: bool,
    pub interval_s: f64,
    /// The filter text while it is typed.
    pub draft: String,
    /// The last left click on a row: when and which, for the double click.
    pub clicked: Option<(Instant, usize)>,
}

/// Two clicks on the same row within this time are a double click.
const DOUBLE: Duration = Duration::from_millis(400);
/// The rows the wheel moves the selection by.
const WHEEL_ROWS: usize = 3;

impl Ui {
    pub(crate) fn new(view: View, interval_s: f64) -> Ui {
        Ui {
            mode: Mode::Normal,
            view,
            index: 0,
            ident: None,
            paused: false,
            interval_s,
            draft: String::new(),
            clicked: None,
        }
    }

    /// Applies one mouse press. `hits` is where the last frame drew the table and the hints,
    /// `rows` how many rows the table has. Only the table's own view takes the mouse.
    pub(crate) fn mouse(&mut self, m: Mouse, hits: &Hits, rows: usize, now: Instant) -> Action {
        if self.mode != Mode::Normal {
            return Action::None;
        }
        let inside = hits
            .table
            .is_some_and(|(y0, y1, x0, x1)| (y0..=y1).contains(&m.y) && (x0..=x1).contains(&m.x));
        let last = rows.saturating_sub(1);
        match m.click {
            Click::WheelUp if inside => self.index = self.index.saturating_sub(WHEEL_ROWS),
            Click::WheelDown if inside => self.index = (self.index + WHEEL_ROWS).min(last),
            Click::Left if inside => {
                if m.y == hits.keys_y {
                    let hint = hits.keys.iter().find(|(a, b, _)| (*a..=*b).contains(&m.x));
                    return match hint.and_then(|(_, _, k)| hint_key(k)) {
                        Some(key) => self.apply(key, rows),
                        None => Action::None,
                    };
                }
                if m.y == hits.header_y {
                    let cell = hits
                        .headers
                        .iter()
                        .find(|(a, b, _)| (*a..=*b).contains(&m.x));
                    if let Some(&(_, _, sort)) = cell {
                        if self.view.by == sort {
                            self.view.reverse = !self.view.reverse;
                        } else {
                            self.view.by = sort;
                            self.view.reverse = false;
                        }
                    }
                } else if m.y >= hits.first_y && m.y < hits.first_y + hits.shown {
                    let at = hits.scroll + (m.y - hits.first_y);
                    let twice = self
                        .clicked
                        .is_some_and(|(t, i)| i == at && now.duration_since(t) <= DOUBLE);
                    self.index = at;
                    if twice {
                        self.clicked = None;
                        self.mode = Mode::Detail;
                    } else {
                        self.clicked = Some((now, at));
                    }
                }
            }
            _ => {}
        }
        Action::None
    }

    /// What the screen shows over the process table.
    pub(crate) fn overlay(&self) -> Overlay {
        match self.mode {
            Mode::Help => Overlay::Help,
            Mode::Sensors => Overlay::Sensors,
            Mode::Detail => Overlay::Detail,
            Mode::Confirm { .. } => Overlay::Table,
            Mode::Normal | Mode::Filter => Overlay::Table,
        }
    }

    /// Applies one key. `rows` is how many rows the table has now.
    pub(crate) fn apply(&mut self, key: Key, rows: usize) -> Action {
        if key == Key::Interrupt {
            return Action::Quit;
        }
        match self.mode {
            Mode::Normal => self.normal(key, rows),
            Mode::Filter => {
                self.typing(key);
                Action::None
            }
            Mode::Help | Mode::Sensors | Mode::Detail => {
                let back = match self.mode {
                    Mode::Help => key == Key::Char('?') || key == Key::Esc,
                    Mode::Sensors => key == Key::Char('s') || key == Key::Esc,
                    _ => key == Key::Esc || key == Key::Enter,
                };
                if back {
                    self.mode = Mode::Normal;
                }
                Action::None
            }
            Mode::Confirm { force } => match key {
                Key::Char('y') | Key::Char('Y') => {
                    self.mode = Mode::Normal;
                    match self.ident.clone() {
                        Some(id) => Action::End {
                            pid: id.pid,
                            name: id.name,
                            force,
                        },
                        None => Action::None,
                    }
                }
                Key::Char('n') | Key::Char('N') | Key::Esc => {
                    self.mode = Mode::Normal;
                    Action::None
                }
                _ => Action::None,
            },
        }
    }

    fn normal(&mut self, key: Key, rows: usize) -> Action {
        let last = rows.saturating_sub(1);
        match key {
            Key::Char('q') => return Action::Quit,
            Key::Up => self.index = self.index.saturating_sub(1),
            Key::Down => self.index = (self.index + 1).min(last),
            Key::PageUp => self.index = self.index.saturating_sub(10),
            Key::PageDown => self.index = (self.index + 10).min(last),
            Key::Home => self.index = 0,
            Key::End => self.index = last,
            Key::Left | Key::Right => {
                let at = SORTS.iter().position(|s| *s == self.view.by).unwrap_or(0);
                let next = if key == Key::Right {
                    (at + 1) % SORTS.len()
                } else {
                    (at + SORTS.len() - 1) % SORTS.len()
                };
                self.view.by = SORTS[next];
            }
            Key::Char('r') => self.view.reverse = !self.view.reverse,
            Key::Char('t') => {
                self.view.tree = !self.view.tree;
                if self.view.tree {
                    self.view.flat = false;
                }
            }
            Key::Char('f') => {
                self.view.flat = !self.view.flat;
                if self.view.flat {
                    self.view.tree = false;
                }
            }
            Key::Char('/') => {
                self.draft = self.view.filter.clone().unwrap_or_default();
                self.mode = Mode::Filter;
            }
            Key::Char(' ') => self.paused = !self.paused,
            Key::Char('?') => self.mode = Mode::Help,
            Key::Char('s') => self.mode = Mode::Sensors,
            Key::Enter => {
                if self.ident.is_some() {
                    self.mode = Mode::Detail;
                }
            }
            Key::Char('k') | Key::Char('K') if self.ident.is_some() => {
                self.mode = Mode::Confirm {
                    force: key == Key::Char('K'),
                };
            }
            Key::Char('+') => {
                self.interval_s = (self.interval_s + INTERVAL_STEP).min(INTERVAL_MAX);
            }
            Key::Char('-') => {
                self.interval_s = (self.interval_s - INTERVAL_STEP).max(INTERVAL_MIN);
            }
            _ => {}
        }
        Action::None
    }

    /// The filter's draft: typed text, Backspace, Enter keeps it, Esc clears it. The table
    /// follows the draft as it is typed.
    fn typing(&mut self, key: Key) {
        match key {
            Key::Char(c) if !c.is_control() => self.draft.push(c),
            Key::Backspace => {
                self.draft.pop();
            }
            Key::Enter => self.mode = Mode::Normal,
            Key::Esc => {
                self.draft.clear();
                self.mode = Mode::Normal;
            }
            _ => return,
        }
        self.view.filter = if self.draft.is_empty() {
            None
        } else {
            Some(self.draft.clone())
        };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn ui() -> Ui {
        Ui::new(crate::cli::parse(&[]).unwrap().view, 1.0)
    }

    fn id(name: &str, pid: u32) -> Option<Ident> {
        Some(Ident {
            name: name.to_string(),
            pid: Some(pid),
        })
    }

    #[test]
    fn plain_keys_and_the_utf8_letters() {
        assert_eq!(next(b"q"), Some((Some(Key::Char('q')), 1)));
        assert_eq!(next("é".as_bytes()), Some((Some(Key::Char('é')), 2)));
        assert_eq!(next(b"\r"), Some((Some(Key::Enter), 1)));
        assert_eq!(next(b"\n"), Some((Some(Key::Enter), 1)));
        assert_eq!(next(&[0x7f]), Some((Some(Key::Backspace), 1)));
        assert_eq!(next(&[0x03]), Some((Some(Key::Interrupt), 1)));
        assert_eq!(next(&[0x01]), Some((None, 1)));
        assert_eq!(next(b""), None);
    }

    #[test]
    fn arrows_and_the_page_home_end_keys() {
        assert_eq!(next(b"\x1b[A"), Some((Some(Key::Up), 3)));
        assert_eq!(next(b"\x1b[B"), Some((Some(Key::Down), 3)));
        assert_eq!(next(b"\x1b[C"), Some((Some(Key::Right), 3)));
        assert_eq!(next(b"\x1b[D"), Some((Some(Key::Left), 3)));
        assert_eq!(next(b"\x1bOA"), Some((Some(Key::Up), 3)));
        assert_eq!(next(b"\x1b[5~"), Some((Some(Key::PageUp), 4)));
        assert_eq!(next(b"\x1b[6~"), Some((Some(Key::PageDown), 4)));
        assert_eq!(next(b"\x1b[H"), Some((Some(Key::Home), 3)));
        assert_eq!(next(b"\x1b[1~"), Some((Some(Key::Home), 4)));
        assert_eq!(next(b"\x1b[F"), Some((Some(Key::End), 3)));
        assert_eq!(next(b"\x1b[4~"), Some((Some(Key::End), 4)));
        // A sequence the view does not know is taken whole and gives no key.
        assert_eq!(next(b"\x1b[2~"), Some((None, 4)));
        // An unfinished sequence is taken whole too.
        assert_eq!(next(b"\x1b[1"), Some((None, 3)));
    }

    #[test]
    fn a_lone_escape_is_esc_and_alt_keys_are_esc_then_the_key() {
        assert_eq!(next(b"\x1b"), Some((Some(Key::Esc), 1)));
        assert_eq!(next(b"\x1bq"), Some((Some(Key::Esc), 1)));
    }

    #[test]
    fn arrows_move_the_index_within_the_rows() {
        let mut u = ui();
        assert_eq!(u.apply(Key::Up, 5), Action::None);
        assert_eq!(u.index, 0);
        u.apply(Key::Down, 5);
        assert_eq!(u.index, 1);
        u.apply(Key::End, 5);
        assert_eq!(u.index, 4);
        u.apply(Key::Down, 5);
        assert_eq!(u.index, 4);
        u.apply(Key::PageUp, 5);
        assert_eq!(u.index, 0);
        u.apply(Key::PageDown, 25);
        assert_eq!(u.index, 10);
        u.apply(Key::Home, 25);
        assert_eq!(u.index, 0);
    }

    #[test]
    fn left_and_right_step_the_sort_key_and_wrap() {
        let mut u = ui();
        assert_eq!(u.view.by, Sort::Cpu);
        u.apply(Key::Right, 1);
        assert_eq!(u.view.by, Sort::Mem);
        u.apply(Key::Left, 1);
        u.apply(Key::Left, 1);
        assert_eq!(u.view.by, Sort::Age);
        u.apply(Key::Right, 1);
        assert_eq!(u.view.by, Sort::Cpu);
    }

    #[test]
    fn r_t_f_space_and_the_interval_change_one_thing_each() {
        let mut u = ui();
        u.apply(Key::Char('r'), 1);
        assert!(u.view.reverse);
        u.apply(Key::Char('t'), 1);
        assert!(u.view.tree && !u.view.flat);
        u.apply(Key::Char('f'), 1);
        assert!(u.view.flat && !u.view.tree);
        u.apply(Key::Char(' '), 1);
        assert!(u.paused);
        u.apply(Key::Char('+'), 1);
        assert!((u.interval_s - 1.5).abs() < 1e-9);
        for _ in 0..30 {
            u.apply(Key::Char('-'), 1);
        }
        assert!((u.interval_s - INTERVAL_MIN).abs() < 1e-9);
    }

    #[test]
    fn the_filter_is_typed_kept_and_cleared() {
        let mut u = ui();
        u.apply(Key::Char('/'), 1);
        assert_eq!(u.mode, Mode::Filter);
        for c in "sle".chars() {
            u.apply(Key::Char(c), 1);
        }
        assert_eq!(u.view.filter.as_deref(), Some("sle"));
        u.apply(Key::Backspace, 1);
        assert_eq!(u.draft, "sl");
        u.apply(Key::Enter, 1);
        assert_eq!(u.mode, Mode::Normal);
        assert_eq!(u.view.filter.as_deref(), Some("sl"));
        u.apply(Key::Char('/'), 1);
        u.apply(Key::Esc, 1);
        assert_eq!(u.mode, Mode::Normal);
        assert_eq!(u.view.filter, None);
    }

    #[test]
    fn overlays_open_and_close_with_their_own_key() {
        let mut u = ui();
        u.apply(Key::Char('s'), 1);
        assert_eq!(u.overlay(), Overlay::Sensors);
        u.apply(Key::Char('x'), 1);
        assert_eq!(u.mode, Mode::Sensors);
        u.apply(Key::Esc, 1);
        assert_eq!(u.overlay(), Overlay::Table);
        u.apply(Key::Char('?'), 1);
        assert_eq!(u.overlay(), Overlay::Help);
        u.apply(Key::Char('?'), 1);
        assert_eq!(u.mode, Mode::Normal);
    }

    #[test]
    fn enter_shows_the_detail_only_with_a_row() {
        let mut u = ui();
        u.apply(Key::Enter, 1);
        assert_eq!(u.mode, Mode::Normal);
        u.ident = id("sleep", 7);
        u.apply(Key::Enter, 1);
        assert_eq!(u.overlay(), Overlay::Detail);
        u.apply(Key::Esc, 1);
        assert_eq!(u.mode, Mode::Normal);
    }

    #[test]
    fn k_asks_then_y_ends_and_n_keeps() {
        let mut u = ui();
        u.ident = id("sleep", 7);
        u.apply(Key::Char('k'), 1);
        assert_eq!(u.mode, Mode::Confirm { force: false });
        assert_eq!(u.apply(Key::Char('n'), 1), Action::None);
        assert_eq!(u.mode, Mode::Normal);
        u.apply(Key::Char('k'), 1);
        assert_eq!(
            u.apply(Key::Char('y'), 1),
            Action::End {
                pid: Some(7),
                name: "sleep".to_string(),
                force: false
            }
        );
        u.apply(Key::Char('K'), 1);
        assert_eq!(u.mode, Mode::Confirm { force: true });
        assert_eq!(
            u.apply(Key::Char('y'), 1),
            Action::End {
                pid: Some(7),
                name: "sleep".to_string(),
                force: true
            }
        );
    }

    #[test]
    fn k_does_nothing_without_a_row_and_q_or_ctrl_c_quit() {
        let mut u = ui();
        u.apply(Key::Char('k'), 1);
        assert_eq!(u.mode, Mode::Normal);
        assert_eq!(u.apply(Key::Char('q'), 1), Action::Quit);
        assert_eq!(u.apply(Key::Interrupt, 1), Action::Quit);
    }

    fn hits() -> Hits {
        Hits {
            table: Some((10, 30, 0, 99)),
            header_y: 11,
            headers: vec![(2, 9, Sort::Cpu), (10, 20, Sort::Mem), (21, 40, Sort::Name)],
            first_y: 12,
            shown: 10,
            scroll: 4,
            keys_y: 30,
            keys: vec![(3, 3, "q"), (7, 7, "↓"), (9, 13, "enter")],
        }
    }

    fn click(x: usize, y: usize) -> Mouse {
        Mouse {
            click: Click::Left,
            x,
            y,
        }
    }

    #[test]
    fn sgr_mouse_reports_are_decoded() {
        let m = |click, x, y| Some((Some(Input::Mouse(Mouse { click, x, y })), 0));
        let got = next_input(b"\x1b[<0;12;5M").unwrap();
        assert_eq!((got.0, got.1), (m(Click::Left, 11, 4).unwrap().0, 10));
        assert_eq!(
            next_input(b"\x1b[<64;1;1M").unwrap().0,
            m(Click::WheelUp, 0, 0).unwrap().0
        );
        assert_eq!(
            next_input(b"\x1b[<65;3;2M").unwrap().0,
            m(Click::WheelDown, 2, 1).unwrap().0
        );
        assert_eq!(
            next_input(b"\x1b[<2;3;2M").unwrap().0,
            m(Click::Other, 2, 1).unwrap().0
        );
        // A release, a drag, and a report cut short give no input and are taken whole.
        assert_eq!(next_input(b"\x1b[<0;12;5m"), Some((None, 10)));
        assert_eq!(next_input(b"\x1b[<32;12;5M"), Some((None, 11)));
        assert_eq!(next_input(b"\x1b[<0;12"), Some((None, 7)));
        // Keys after a report are still read.
        let both = b"\x1b[<0;1;1Mq";
        let (_, n) = next_input(both).unwrap();
        assert_eq!(
            next_input(&both[n..]),
            Some((Some(Input::Key(Key::Char('q'))), 1))
        );
        // Keys decode as before.
        assert_eq!(next_input(b"\x1b[A"), Some((Some(Input::Key(Key::Up)), 3)));
        assert_eq!(next_input(b"\x1b"), Some((Some(Input::Key(Key::Esc)), 1)));
    }

    #[test]
    fn a_click_selects_a_row_by_the_scroll_and_two_open_its_detail() {
        let mut u = ui();
        let t = Instant::now();
        u.mouse(click(5, 14), &hits(), 50, t);
        assert_eq!(u.index, 6);
        assert_eq!(u.mode, Mode::Normal);
        u.mouse(click(5, 14), &hits(), 50, t + Duration::from_millis(200));
        assert_eq!(u.mode, Mode::Detail);
        // A click on another row is no double click, nor is a slow one.
        let mut u = ui();
        u.mouse(click(5, 14), &hits(), 50, t);
        u.mouse(click(5, 15), &hits(), 50, t + Duration::from_millis(100));
        u.mouse(click(5, 15), &hits(), 50, t + Duration::from_millis(900));
        assert_eq!((u.index, u.mode), (7, Mode::Normal));
        // Below the last drawn row, and outside the table: nothing.
        u.mouse(click(5, 22), &hits(), 50, t);
        u.mouse(click(5, 40), &hits(), 50, t);
        assert_eq!(u.index, 7);
    }

    #[test]
    fn a_header_sorts_and_again_reverses() {
        let mut u = ui();
        let t = Instant::now();
        u.mouse(click(12, 11), &hits(), 5, t);
        assert_eq!((u.view.by, u.view.reverse), (Sort::Mem, false));
        u.mouse(click(12, 11), &hits(), 5, t);
        assert_eq!((u.view.by, u.view.reverse), (Sort::Mem, true));
        u.mouse(click(30, 11), &hits(), 5, t);
        assert_eq!((u.view.by, u.view.reverse), (Sort::Name, false));
        // Between headers' cells and beyond them: nothing.
        u.mouse(click(60, 11), &hits(), 5, t);
        assert_eq!(u.view.by, Sort::Name);
    }

    #[test]
    fn the_wheel_moves_three_rows_over_the_table_only() {
        let mut u = ui();
        let t = Instant::now();
        let wheel = |click, x, y| Mouse { click, x, y };
        u.mouse(wheel(Click::WheelDown, 5, 20), &hits(), 20, t);
        assert_eq!(u.index, 3);
        u.mouse(wheel(Click::WheelDown, 5, 20), &hits(), 20, t);
        u.mouse(wheel(Click::WheelUp, 5, 20), &hits(), 20, t);
        assert_eq!(u.index, 3);
        u.mouse(wheel(Click::WheelUp, 5, 20), &hits(), 20, t);
        u.mouse(wheel(Click::WheelUp, 5, 20), &hits(), 20, t);
        assert_eq!(u.index, 0);
        for _ in 0..10 {
            u.mouse(wheel(Click::WheelDown, 5, 20), &hits(), 20, t);
        }
        assert_eq!(u.index, 19);
        // Over the overview above the table: nothing.
        u.mouse(wheel(Click::WheelUp, 5, 3), &hits(), 20, t);
        assert_eq!(u.index, 19);
    }

    #[test]
    fn a_hint_does_what_its_key_does_and_overlays_take_no_mouse() {
        let mut u = ui();
        let t = Instant::now();
        assert_eq!(u.mouse(click(3, 30), &hits(), 5, t), Action::Quit);
        u.mouse(click(7, 30), &hits(), 5, t);
        assert_eq!(u.index, 1);
        u.ident = id("sleep", 7);
        u.mouse(click(10, 30), &hits(), 5, t);
        assert_eq!(u.mode, Mode::Detail);
        u.mouse(click(5, 14), &hits(), 5, t);
        u.mouse(click(12, 11), &hits(), 5, t);
        assert_eq!((u.mode, u.view.by), (Mode::Detail, Sort::Cpu));
    }
}
