//! `aether` (live, or once with `--once`): the readings, written out. Live on a terminal that
//! can show it, the full-screen view; otherwise the plain text view. `--json` and `--once` never
//! take the screen.

use crate::cli::{compare, Keys, Parsed, Sort, View};
use crate::cmd::keys::{self, Action, Ident, Input, Mode as Pane, Ui};
#[cfg(windows)]
use crate::cmd::wininput;
use crate::out::history::History;
use crate::out::screen::{self, Overlay, Screen};
use crate::out::tree::Tree;
use crate::out::{json, text};
use aether::sys::{
    command_line, Battery, Cpu, Disks, Gpus, Host, Mem, Nets, Proc, Procs, Root, Sensors,
};
use aether::Reading;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Moves the cursor home and clears to the end of the screen: one frame replaces the last.
const CLEAR: &[u8] = b"\x1b[H\x1b[J";

/// The wait between the two readings of `--once`, so its rates are numbers.
const ONCE_WAIT: Duration = Duration::from_millis(250);

/// The alternate screen, with the cursor hidden: entering the full-screen view.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l";

/// Mouse reports on (presses and the wheel, in the SGR form), and off again.
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1006h";
const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1000l";

/// What the terminal had: colours reset, the alternate screen left, the cursor shown.
const LEAVE: &[u8] = b"\x1b[0m\x1b[?1049l\x1b[?25h";

/// The rows and columns a terminal needs for the full-screen view; a smaller one gets plain text.
const SCREEN_MIN: (usize, usize) = (15, 60);

/// How the frames are written: to a terminal or not, in the full-screen view or the plain one,
/// and with colour codes or without.
#[derive(Debug)]
pub(crate) struct Mode {
    pub tty: bool,
    pub screen: bool,
    pub colour: bool,
    /// The keys are on: stdin is a terminal in raw mode.
    pub keys: bool,
    /// The mouse is on: reports are asked of the terminal while the view runs.
    pub mouse: bool,
    /// The user names by uid.
    pub users: Rc<HashMap<u32, String>>,
}

/// Runs the live view, or one reading with `--once`.
pub fn run(parsed: &Parsed) -> ExitCode {
    let tty = io::stdout().is_terminal();
    // On Windows the console takes escape codes and UTF-8 for the run and is put back after.
    #[cfg(windows)]
    let console = if tty {
        aether::sys::windows::calls::console_open()
    } else {
        None
    };
    #[cfg(windows)]
    let utf8 = console.is_some();
    #[cfg(not(windows))]
    let utf8 = utf8_locale();
    let root = Root::system();
    let started = Instant::now();
    let mut sources = Sources::open(&root);
    let live = !parsed.once;
    let fits =
        screen_size().is_some_and(|(rows, cols)| rows >= SCREEN_MIN.0 && cols >= SCREEN_MIN.1);
    let full = tty && live && !parsed.json && !parsed.plain && utf8 && term_not_dumb() && fits;
    if full {
        catch_stops();
    }
    // The keys take stdin in raw mode; `Restore` puts it back however the run ends.
    let saved = if full && !parsed.no_keys && io::stdin().is_terminal() {
        raw_input(!parsed.no_mouse)
    } else {
        None
    };
    let _restore = Restore { saved };
    let mode = Mode {
        tty,
        screen: full,
        colour: screen_colour(parsed),
        keys: _restore.saved.is_some(),
        mouse: _restore.saved.is_some() && !parsed.no_mouse,
        users: Rc::new(user_names(&root)),
    };
    let keys_on = mode.keys;
    let mut out = io::stdout().lock();
    let result = frames(
        |r| sources.fill(r, started, &parsed.view),
        &mut out,
        parsed,
        mode,
        move |d| {
            if keys_on {
                wait_keys(d)
            } else {
                wait(d);
                Vec::new()
            }
        },
        screen_size,
        stop_requested,
    );
    // A full-screen write that fails means the terminal has gone: there is no one left to tell.
    let result = if full { result.or(Ok(())) } else { result };
    finish(result)
}

/// Puts stdin back as it was when dropped: the keys' raw mode ends however the run ends.
struct Restore {
    saved: Option<Saved>,
}

impl Drop for Restore {
    fn drop(&mut self) {
        if let Some(s) = &self.saved {
            restore_input(s);
        }
    }
}

#[cfg(target_os = "linux")]
type Saved = aether::sys::linux::calls::Settings;
#[cfg(not(target_os = "linux"))]
type Saved = ();

#[cfg(target_os = "linux")]
fn raw_input(_mouse: bool) -> Option<Saved> {
    aether::sys::linux::calls::raw_input()
}
#[cfg(windows)]
fn raw_input(mouse: bool) -> Option<Saved> {
    aether::sys::windows::calls::input_open(mouse).then_some(())
}
#[cfg(not(any(target_os = "linux", windows)))]
fn raw_input(_mouse: bool) -> Option<Saved> {
    None
}

#[cfg(target_os = "linux")]
fn restore_input(saved: &Saved) {
    aether::sys::linux::calls::restore_input(saved);
}
#[cfg(windows)]
fn restore_input(_saved: &Saved) {
    aether::sys::windows::calls::input_restore();
}
#[cfg(not(any(target_os = "linux", windows)))]
fn restore_input(_saved: &Saved) {}

/// Waits up to `d` for keys on stdin and returns the bytes that came; none at the timeout or
/// when a signal ends the wait. Input that has closed reads as Ctrl-C, so the view leaves.
#[cfg(target_os = "linux")]
fn wait_keys(d: Duration) -> Vec<u8> {
    let ms = i32::try_from(d.as_millis()).unwrap_or(i32::MAX);
    if !aether::sys::linux::calls::input_ready(ms) {
        return Vec::new();
    }
    let mut buf = [0u8; 256];
    match aether::sys::linux::calls::read_input(&mut buf) {
        Some(0) => vec![0x03],
        Some(n) => buf[..n].to_vec(),
        None => Vec::new(),
    }
}
/// The console's key and mouse events, as the bytes a terminal would send.
#[cfg(windows)]
fn wait_keys(d: Duration) -> Vec<u8> {
    use aether::sys::windows::calls::{input_events, InputEvent};
    let ms = u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
    let mut out = Vec::new();
    static HELD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let mut held = HELD.load(std::sync::atomic::Ordering::Relaxed);
    let events = input_events(ms);
    for e in events {
        match e {
            InputEvent::Key { down, vk, ch } => wininput::key_bytes(down, vk, ch, &mut out),
            InputEvent::Mouse {
                x,
                y,
                buttons,
                flags,
            } => wininput::mouse_bytes(&mut held, x, y, buttons, flags, &mut out),
        }
    }
    HELD.store(held, std::sync::atomic::Ordering::Relaxed);
    out
}
#[cfg(not(any(target_os = "linux", windows)))]
fn wait_keys(d: Duration) -> Vec<u8> {
    wait(d);
    Vec::new()
}

/// The account names by uid, read once from the running processes' accounts.
#[cfg(windows)]
pub(crate) fn user_names(_root: &Root) -> HashMap<u32, String> {
    aether::sys::windows::users::names()
}

/// The user names by uid, read once from the password file. A missing file gives none, and a
/// uid with no name is shown as the number.
#[cfg(not(windows))]
pub(crate) fn user_names(root: &Root) -> HashMap<u32, String> {
    let mut names = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(root.path("etc/passwd")) {
        for line in text.lines() {
            let mut fields = line.split(':');
            if let (Some(name), Some(_), Some(uid)) = (fields.next(), fields.next(), fields.next())
            {
                if let Ok(uid) = uid.parse::<u32>() {
                    names.entry(uid).or_insert_with(|| name.to_string());
                }
            }
        }
    }
    names
}

/// Colour codes unless `--no-color` or `NO_COLOR` is set.
fn screen_colour(parsed: &Parsed) -> bool {
    !parsed.no_color && std::env::var_os("NO_COLOR").is_none()
}

/// The first of LC_ALL, LC_CTYPE and LANG that is set names UTF-8.
#[cfg(not(windows))]
fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
}

fn term_not_dumb() -> bool {
    !matches!(std::env::var("TERM"), Ok(t) if t == "dumb")
}

/// The terminal's (rows, columns) on standard output, where the OS has that built.
#[cfg(target_os = "linux")]
fn screen_size() -> Option<(usize, usize)> {
    aether::sys::linux::calls::term_size()
        .map(|(rows, cols)| (usize::from(rows), usize::from(cols)))
}

#[cfg(windows)]
fn screen_size() -> Option<(usize, usize)> {
    aether::sys::windows::calls::term_size()
        .map(|(rows, cols)| (usize::from(rows), usize::from(cols)))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn screen_size() -> Option<(usize, usize)> {
    None
}

#[cfg(target_os = "linux")]
pub(crate) fn catch_stops() {
    aether::sys::linux::calls::catch_stops();
}

#[cfg(windows)]
pub(crate) fn catch_stops() {
    aether::sys::windows::calls::catch_stops();
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(crate) fn catch_stops() {}

#[cfg(target_os = "linux")]
pub(crate) fn stop_requested() -> bool {
    aether::sys::linux::calls::stop_requested()
}

#[cfg(windows)]
pub(crate) fn stop_requested() -> bool {
    aether::sys::windows::calls::stop_requested()
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(crate) fn stop_requested() -> bool {
    false
}

/// Sleeps for `d` in slices of 50 ms, and returns early when a stop has come.
pub(crate) fn wait(d: Duration) {
    let slice = Duration::from_millis(50);
    let mut left = d;
    while !stop_requested() && !left.is_zero() {
        let step = left.min(slice);
        std::thread::sleep(step);
        left -= step;
    }
}

/// Enters the full-screen view: the alternate screen, the cursor hidden.
fn enter(out: &mut dyn Write, mouse: bool) -> io::Result<()> {
    out.write_all(ENTER)?;
    if mouse {
        out.write_all(MOUSE_ON)?;
    }
    out.flush()
}

/// Leaves it: the terminal is put back as it was.
fn leave(out: &mut dyn Write, mouse: bool) -> io::Result<()> {
    if mouse {
        out.write_all(MOUSE_OFF)?;
    }
    out.write_all(LEAVE)?;
    out.flush()
}

/// The exit code for the end of a run. A closed pipe (`aether --json | head`) is a normal end.
pub(crate) fn finish(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("aether: could not write the reading");
            ExitCode::FAILURE
        }
    }
}

/// Every source, opened once; each tick reads them in place.
pub(crate) struct Sources {
    cpu: Cpu,
    mem: Mem,
    gpus: Gpus,
    sensors: Sensors,
    disks: Disks,
    nets: Nets,
    battery: Battery,
    host: Host,
    procs: Procs,
}

impl Sources {
    pub(crate) fn open(root: &Root) -> Sources {
        Sources {
            cpu: Cpu::open(root),
            mem: Mem::open(root),
            gpus: Gpus::open(root),
            sensors: Sensors::open(root),
            disks: Disks::open(root),
            nets: Nets::open(root),
            battery: Battery::open(root),
            host: Host::open(root),
            procs: Procs::open(root),
        }
    }

    /// Fills `r` from one read of every source. The buffers of `r` are reused.
    /// Only the parts `--only` names are read: a part that is not shown costs nothing.
    pub(crate) fn fill(&mut self, r: &mut Reading, started: Instant, view: &View) {
        r.t = started.elapsed().as_secs_f64();
        if view.shows("cpu") {
            r.cpu.clone_from(self.cpu.read());
        }
        if view.shows("mem") {
            r.mem.clone_from(self.mem.read());
        }
        if view.shows("gpu") {
            fill_vec(&mut r.gpus, self.gpus.read());
        }
        if view.shows("sensors") {
            fill_vec(&mut r.sensors, self.sensors.read());
        }
        if view.shows("disk") {
            fill_vec(&mut r.disks, self.disks.read());
        }
        if view.shows("net") {
            fill_vec(&mut r.nets, self.nets.read());
        }
        if view.shows("battery") {
            r.battery = self.battery.read().cloned();
        }
        if view.shows("host") {
            r.host.clone_from(self.host.read());
        }
        if view.shows("procs") {
            fill_vec(&mut r.procs, self.procs.read());
        }
    }
}

/// Copies `src` into `dst`, reusing the elements (and their strings) when the lengths match.
fn fill_vec<T: Clone>(dst: &mut Vec<T>, src: &[T]) {
    if dst.len() == src.len() {
        dst.clone_from_slice(src);
    } else {
        dst.clear();
        dst.extend_from_slice(src);
    }
}

/// The number a numeric key sorts by. A process without the number sorts last.
fn key(p: &Proc, by: Sort) -> f64 {
    let value = match by {
        Sort::Cpu => p.cpu_pct.map(f64::from),
        Sort::Mem => Some(p.mem as f64),
        Sort::Gpu => p.gpu_pct.map(f64::from),
        Sort::Disk => match (p.read_bps, p.write_bps) {
            (None, None) => None,
            (r, w) => Some(r.unwrap_or(0).saturating_add(w.unwrap_or(0)) as f64),
        },
        // The other keys compare their own fields; there is no number to sort by.
        Sort::Pid | Sort::User | Sort::Threads | Sort::Name | Sort::Age => None,
    };
    value.unwrap_or(-1.0)
}

/// Puts the processes in the order the view asks for. `rank` is the number the numeric keys
/// sort by (the raw key, or the smoothed one under `--steady`).
pub(crate) fn order(procs: &mut [Proc], view: &View, rank: &dyn Fn(&Proc) -> f64) {
    procs.sort_unstable_by(|a, b| compare(view, &keys(a, rank), &keys(b, rank)));
}

fn keys<'a>(p: &'a Proc, rank: &dyn Fn(&Proc) -> f64) -> Keys<'a> {
    Keys {
        pid: p.pid,
        uid: p.uid,
        threads: p.threads,
        started: p.started_s,
        name: &p.name,
        rank: rank(p),
    }
}

/// Whether `hay` contains `needle`, ASCII case ignored (an empty needle is always found).
fn contains_ci(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// A command line read for `--filter`, kept per process while the process lives.
/// The tick it was last seen on is kept with it, so a process that ends drops out.
type Commands = HashMap<(u32, u64), (u64, Option<String>)>;

/// Whether a process stays under `--filter`: its name, or else its command line, contains the
/// text. The command line is read once per process, and only when the name does not match.
fn keeps(p: &Proc, needle: &str, commands: &mut Commands, tick: u64, root: &Root) -> bool {
    if contains_ci(&p.name, needle) {
        return true;
    }
    let entry = commands
        .entry((p.pid, p.started_s))
        .or_insert_with(|| (tick, command_line(root, p.pid)));
    entry.0 = tick;
    entry
        .1
        .as_deref()
        .is_some_and(|line| contains_ci(line, needle))
}

/// The smoothed numbers `--steady` sorts by: half the last value and half the new one, kept per
/// process (pid and start time). A missing value stays missing.
#[derive(Default)]
struct Steady {
    values: HashMap<(u32, u64), f64>,
    scratch: HashMap<(u32, u64), f64>,
}

impl Steady {
    fn update(&mut self, procs: &[Proc], by: Sort) {
        self.scratch.clear();
        for p in procs {
            let raw = key(p, by);
            let id = (p.pid, p.started_s);
            let value = match self.values.get(&id) {
                Some(&old) if old >= 0.0 && raw >= 0.0 => (old + raw) / 2.0,
                _ => raw,
            };
            self.scratch.insert(id, value);
        }
        std::mem::swap(&mut self.values, &mut self.scratch);
    }

    fn of(&self, p: &Proc) -> f64 {
        self.values
            .get(&(p.pid, p.started_s))
            .copied()
            .unwrap_or(-1.0)
    }
}

/// Writes the readings: one when `--once`, else one a tick until the output fails or a stop
/// comes. The full-screen view is entered before the first frame and left after the last, however
/// the run ends. `read` fills a reading, `wait` waits for a while and returns the keys that came
/// in that time, `size` gives the terminal's (rows, columns) and `stop` says a stop has come.
/// Every run reads twice before its first frame, so the rates have two readings to work from.
fn frames(
    mut read: impl FnMut(&mut Reading),
    out: &mut dyn Write,
    parsed: &Parsed,
    mode: Mode,
    mut wait: impl FnMut(Duration) -> Vec<u8>,
    mut size: impl FnMut() -> Option<(usize, usize)>,
    stop: impl Fn() -> bool,
) -> io::Result<()> {
    if !mode.screen {
        return draw(&mut read, out, parsed, mode, &mut wait, &mut size, &stop);
    }
    let mouse = mode.mouse;
    enter(out, mouse)?;
    let result = draw(&mut read, out, parsed, mode, &mut wait, &mut size, &stop);
    // Whatever ended the run, the terminal is put back; a failed write here means it is gone.
    let _ = leave(out, mouse);
    result
}

/// What the last line asks, while a key is waiting for an answer.
fn prompt(ui: &Ui) -> Option<String> {
    match ui.mode {
        Pane::Filter => Some(format!("/ {}", ui.draft)),
        Pane::Confirm { force } => ui.ident.as_ref().map(|id| {
            let what = if force { "force end" } else { "end" };
            match id.pid {
                Some(p) => format!("{what} {} (pid {p})? y or n", id.name),
                None => format!("{what} all {}? y or n", id.name),
            }
        }),
        _ => None,
    }
}

/// Ends the processes a row stands for: the one process, or, for a group or in the grouped
/// view, every process of the name.
fn end_rows(reading: &Reading, grouped: bool, pid: Option<u32>, name: &str, force: bool) {
    match pid {
        Some(p) if !grouped => {
            let _ = aether::sys::end(p, force);
        }
        _ => {
            for p in reading.procs.iter().filter(|p| p.name == name) {
                let _ = aether::sys::end(p.pid, force);
            }
        }
    }
}

/// Puts the processes a reading shows into `reading.procs`: all of them, or those the filter
/// keeps. While a filter is on, the whole list is kept in `full`, so a key that changes the
/// filter needs no new reading. `fresh` says `reading.procs` holds a new reading's list.
#[allow(clippy::too_many_arguments)]
fn sift(
    reading: &mut Reading,
    full: &mut Vec<Proc>,
    filtered: &mut bool,
    needle: Option<&str>,
    fresh: bool,
    commands: &mut Commands,
    tick: u64,
    root: &Root,
) {
    if fresh && *filtered {
        std::mem::swap(&mut reading.procs, full);
    }
    match needle {
        Some(n) => {
            if !*filtered {
                std::mem::swap(&mut reading.procs, full);
                *filtered = true;
            }
            reading.procs.clear();
            for p in full.iter() {
                if keeps(p, n, commands, tick, root) {
                    reading.procs.push(p.clone());
                }
            }
        }
        None => {
            if *filtered {
                std::mem::swap(&mut reading.procs, full);
                *filtered = false;
            }
        }
    }
}

/// The loop: a reading when one is due, a frame after each reading and each key, and a wait
/// between them in which keys are taken. A key redraws from the last reading and reads nothing.
fn draw(
    read: &mut dyn FnMut(&mut Reading),
    out: &mut dyn Write,
    parsed: &Parsed,
    mode: Mode,
    wait: &mut dyn FnMut(Duration) -> Vec<u8>,
    size: &mut dyn FnMut() -> Option<(usize, usize)>,
    stop: &dyn Fn() -> bool,
) -> io::Result<()> {
    let live = !parsed.once;
    let interval = Duration::try_from_secs_f64(parsed.interval_s).unwrap_or(Duration::MAX);
    // `--once` waits as long as `-i` asked, or 250 ms when `-i` was not given.
    let once_wait = if parsed.interval_given {
        interval
    } else {
        ONCE_WAIT
    };
    let root = Root::system();
    let mut reading = Reading::default();
    let mut full: Vec<Proc> = Vec::new();
    let mut filtered = false;
    let mut held: Vec<Proc> = Vec::new();
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut commands = Commands::new();
    let mut steady = Steady::default();
    let mut tick = 0u64;
    let mut tree = Tree::new();
    let mut hist = History::default();
    let mut ui = Ui::new(parsed.view.clone(), parsed.interval_s);
    let (rows, cols) = size().unwrap_or((24, 80));
    let mut state = Screen {
        cols,
        rows,
        colour: mode.colour,
        selected: Some(0),
        scroll: 0,
        interval_s: parsed.interval_s,
        paused: false,
        overlay: Overlay::Table,
        keys: mode.keys,
        prompt: None,
        users: Rc::clone(&mode.users),
        hits: screen::Hits::default(),
    };
    let mut at = Instant::now();
    let mut readings = 0u32;
    let mut last_rows = 0usize;
    loop {
        if stop() {
            return Ok(());
        }
        if Instant::now() >= at {
            let started = Instant::now();
            read(&mut reading);
            if ui.paused {
                reading.procs.clone_from(&held);
            } else {
                tick += 1;
                if ui.view.steady && ui.view.by.smooths() {
                    steady.update(&reading.procs, ui.view.by);
                }
                let needle = ui.view.filter.clone();
                sift(
                    &mut reading,
                    &mut full,
                    &mut filtered,
                    needle.as_deref(),
                    true,
                    &mut commands,
                    tick,
                    &root,
                );
                commands.retain(|_, e| e.0 == tick);
            }
            hist.push(&reading);
            readings += 1;
            at = started + if live { interval } else { once_wait };
        }
        if readings < 2 {
            // A wait that came back with no keys has reached its time (or a signal ended it).
            if wait(at.saturating_duration_since(Instant::now())).is_empty() {
                at = Instant::now();
            }
            continue;
        }
        let smooth = ui.view.steady && ui.view.by.smooths();
        let by = ui.view.by;
        let rank = |p: &Proc| if smooth { steady.of(p) } else { key(p, by) };
        order(&mut reading.procs, &ui.view, &rank);
        buf.clear();
        if mode.screen {
            if let Some((r, c)) = size() {
                state.rows = r;
                state.cols = c;
            }
            let listed = text::process_rows(&reading.procs, &ui.view, &rank, &mut tree).0;
            last_rows = listed.len();
            let pos = ui.ident.as_ref().and_then(|id| {
                listed
                    .iter()
                    .position(|r| r.name == id.name && r.pid == id.pid)
            });
            ui.index = pos.unwrap_or(ui.index.min(listed.len().saturating_sub(1)));
            ui.ident = listed.get(ui.index).map(|r| Ident {
                name: r.name.clone(),
                pid: r.pid,
            });
            state.selected = (!listed.is_empty()).then_some(ui.index);
            state.paused = ui.paused;
            state.overlay = ui.overlay();
            state.interval_s = ui.interval_s;
            state.prompt = prompt(&ui);
            screen::render(
                &mut buf, &reading, &ui.view, &mut state, &hist, &mut tree, &rank,
            );
        } else {
            if live && mode.tty && !parsed.json {
                buf.extend_from_slice(CLEAR);
            }
            if parsed.json {
                json::write(&reading, &ui.view, &mut buf);
            } else {
                // A terminal is filled to its height, unless --all asks for every row.
                let fit = if mode.tty && !parsed.view.all {
                    size().map(|(rows, _)| rows)
                } else {
                    None
                };
                text::write(&reading, &ui.view, &mut buf, fit, &mut tree, &rank);
            }
        }
        out.write_all(&buf)?;
        out.flush()?;
        if !live {
            return Ok(());
        }
        let bytes = wait(at.saturating_duration_since(Instant::now()));
        if bytes.is_empty() {
            at = Instant::now();
        }
        let mut pos = 0;
        while let Some((input, used)) = keys::next_input(&bytes[pos..]) {
            pos += used;
            let Some(input) = input else { continue };
            let (index, was_paused, filter) = (ui.index, ui.paused, ui.view.filter.clone());
            let action = match input {
                Input::Key(k) => ui.apply(k, last_rows),
                Input::Mouse(m) if mode.mouse => {
                    ui.mouse(m, &state.hits, last_rows, Instant::now())
                }
                Input::Mouse(_) => Action::None,
            };
            if ui.index != index {
                ui.ident = None;
            }
            if ui.paused && !was_paused {
                held.clone_from(&reading.procs);
            }
            if ui.view.filter != filter {
                let needle = ui.view.filter.clone();
                sift(
                    &mut reading,
                    &mut full,
                    &mut filtered,
                    needle.as_deref(),
                    false,
                    &mut commands,
                    tick,
                    &root,
                );
            }
            match action {
                Action::None => {}
                Action::Quit => return Ok(()),
                Action::End { pid, name, force } => {
                    let grouped = !ui.view.flat && !ui.view.tree;
                    end_rows(&reading, grouped, pid, &name, force);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cli::parse;
    use std::rc::Rc;

    fn mode(tty: bool) -> Mode {
        Mode {
            tty,
            screen: false,
            colour: false,
            keys: false,
            mouse: false,
            users: Rc::default(),
        }
    }

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    /// Sorts the way the live view does, with the raw key for `by`.
    fn sorted(v: &mut [Proc], by: Sort) {
        let view = View {
            by,
            reverse: false,
            steady: false,
            filter: None,
            flat: false,
            all: false,
            only: None,
            tree: false,
            disks: None,
            nets: None,
        };
        order(v, &view, &|p| key(p, by));
    }

    fn proc(
        pid: u32,
        cpu: Option<f32>,
        mem: u64,
        gpu: Option<f32>,
        io: Option<(u64, u64)>,
    ) -> Proc {
        Proc {
            pid,
            ppid: 1,
            uid: 1000,
            threads: 1,
            name: format!("p{pid}"),
            state: b'S',
            cpu_pct: cpu,
            gpu_pct: gpu,
            mem,
            rss: mem,
            started_s: 0,
            vram: None,
            read_bps: io.map(|(r, _)| r),
            write_bps: io.map(|(_, w)| w),
        }
    }

    fn pids(procs: &[Proc]) -> Vec<u32> {
        procs.iter().map(|p| p.pid).collect()
    }

    /// A writer that accepts a fixed number of frames, then fails with `kind`.
    struct Limited {
        out: Vec<u8>,
        flushes: usize,
        limit: usize,
        kind: io::ErrorKind,
    }

    impl Limited {
        fn new(limit: usize, kind: io::ErrorKind) -> Limited {
            Limited {
                out: Vec::new(),
                flushes: 0,
                limit,
                kind,
            }
        }
    }

    impl Write for Limited {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.out.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes > self.limit {
                Err(io::Error::new(self.kind, "stop"))
            } else {
                Ok(())
            }
        }
    }

    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }

    #[test]
    fn by_cpu_puts_the_biggest_first_and_missing_last() {
        let mut v = vec![
            proc(1, None, 0, None, None),
            proc(2, Some(5.0), 0, None, None),
            proc(3, Some(5.0), 0, None, None),
            proc(4, Some(9.0), 0, None, None),
        ];
        sorted(&mut v, Sort::Cpu);
        assert_eq!(pids(&v), vec![4, 2, 3, 1]);
    }

    #[test]
    fn by_mem_and_by_gpu() {
        let mut v = vec![
            proc(1, None, 10, Some(2.0), None),
            proc(2, None, 30, None, None),
        ];
        sorted(&mut v, Sort::Mem);
        assert_eq!(pids(&v), vec![2, 1]);
        sorted(&mut v, Sort::Gpu);
        assert_eq!(pids(&v), vec![1, 2]);
    }

    #[test]
    fn by_disk_adds_read_and_write() {
        let mut v = vec![
            proc(1, None, 0, None, Some((5, 5))),
            proc(2, None, 0, None, Some((1, 20))),
            proc(3, None, 0, None, None),
        ];
        sorted(&mut v, Sort::Disk);
        assert_eq!(pids(&v), vec![2, 1, 3]);
    }

    #[test]
    fn once_writes_one_frame_and_no_clear_code() {
        let p = parse(&args("--once")).unwrap();
        let mut reads = 0;
        let mut pauses = Vec::new();
        let mut out = Limited::new(9, io::ErrorKind::Other);
        let r = frames(
            |_| reads += 1,
            &mut out,
            &p,
            mode(true),
            |d| {
                pauses.push(d);
                Vec::new()
            },
            || None,
            || false,
        );
        assert!(r.is_ok());
        // A first reading, the wait, then the reading that is printed.
        assert_eq!(reads, 2);
        assert_eq!(pauses.len(), 1);
        assert!(pauses[0] <= ONCE_WAIT);
        assert_eq!(count(&out.out, CLEAR), 0);
    }

    #[test]
    fn live_warms_up_then_paces_each_frame_and_clears_on_a_terminal() {
        let p = parse(&args("-i 2")).unwrap();
        let mut reads = 0;
        let mut pauses = Vec::new();
        let mut out = Limited::new(3, io::ErrorKind::Other);
        let r = frames(
            |_| reads += 1,
            &mut out,
            &p,
            mode(true),
            |d| {
                pauses.push(d);
                Vec::new()
            },
            || None,
            || false,
        );
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::Other);
        // One warm-up read, then four frames: the fourth fails at its flush.
        assert_eq!(reads, 5);
        assert_eq!(count(&out.out, CLEAR), 4);
        // The warm-up pause, then one after each of the three frames that flushed.
        assert_eq!(pauses.len(), 4);
        assert!(pauses.iter().all(|d| *d <= Duration::from_secs(2)));
    }

    #[test]
    fn live_into_a_pipe_has_no_clear_codes() {
        let p = parse(&args("")).unwrap();
        let mut out = Limited::new(2, io::ErrorKind::Other);
        let _ = frames(
            |_| {},
            &mut out,
            &p,
            mode(false),
            |_| Vec::new(),
            || None,
            || false,
        );
        assert_eq!(count(&out.out, CLEAR), 0);
        assert_eq!(out.flushes, 3);
    }

    #[test]
    fn json_is_never_cleared() {
        let p = parse(&args("--json")).unwrap();
        let mut out = Limited::new(1, io::ErrorKind::Other);
        let _ = frames(
            |_| {},
            &mut out,
            &p,
            mode(true),
            |_| Vec::new(),
            || None,
            || false,
        );
        assert_eq!(count(&out.out, CLEAR), 0);
    }

    #[test]
    fn a_closed_pipe_is_a_quiet_end() {
        let p = parse(&args("--json")).unwrap();
        let mut reads = 0;
        let mut out = Limited::new(0, io::ErrorKind::BrokenPipe);
        let r = frames(
            |_| reads += 1,
            &mut out,
            &p,
            mode(false),
            |_| Vec::new(),
            || None,
            || false,
        );
        let e = r.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(reads, 2);
        assert_eq!(finish(Err(e)), ExitCode::SUCCESS);
    }

    #[test]
    fn fill_vec_copies_with_and_without_a_length_change() {
        let mut dst = vec![String::from("old")];
        fill_vec(&mut dst, &[String::from("a")]);
        assert_eq!(dst, vec![String::from("a")]);
        fill_vec(&mut dst, &[String::from("b"), String::from("c")]);
        assert_eq!(dst, vec![String::from("b"), String::from("c")]);
    }

    #[test]
    fn steady_halves_the_old_value_and_a_filter_ignores_case() {
        let mut st = Steady::default();
        let mut v = vec![proc(7, Some(10.0), 0, None, None)];
        st.update(&v, Sort::Cpu);
        assert_eq!(st.of(&v[0]), 10.0);
        v[0].cpu_pct = Some(30.0);
        st.update(&v, Sort::Cpu);
        assert_eq!(st.of(&v[0]), 20.0);
        assert!(contains_ci("Firefox --new-window", "FIREFOX"));
        assert!(!contains_ci("abc", "abcd"));
    }
}
