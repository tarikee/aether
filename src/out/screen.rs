//! The full-screen view: the machine in rounded boxes that fill the terminal in width and in
//! height. Bars stretch to their line, graphs show the last readings, every thread has its bar,
//! and the process table goes down to the last row. A pure function of one reading and the
//! screen's state: no system call, no clock, no environment. Every line is cut to its width, so
//! nothing wraps.
//!
//! The layout (the tier, the heights, which box goes where) is decided first; then each box
//! draws itself into the rectangle it is given.

use super::history::{History, Ring};
use super::text::{self, Row};
use super::tree::Tree;
use crate::cli::{Sort, View};
use aether::sys::{BatterySample, DiskSample, GpuSample, Proc, Sensor, SensorKind};
use aether::Reading;
use std::collections::HashMap;
use std::rc::Rc;

/// The state the frame is drawn from. The live loop owns it and hands it to `render`.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// The terminal's columns and rows.
    pub cols: usize,
    pub rows: usize,
    /// Whether colour codes are written.
    pub colour: bool,
    /// The process row the marker is on, by index into the rows; `None` shows no marker.
    pub selected: Option<usize>,
    /// The first process row on the screen; `render` moves it to keep the marker in view.
    pub scroll: usize,
    /// Seconds between readings: the title's interval, and what a graph's column spans.
    pub interval_s: f64,
    /// Whether the list is paused (shown in the title).
    pub paused: bool,
    /// What is shown in place of the process table.
    pub overlay: Overlay,
    /// Whether the keys are on: the process box's border names them, else how to leave.
    pub keys: bool,
    /// The filter being typed (`/ text`, in the process box's border), or a question (in a box
    /// of its own under the processes).
    pub prompt: Option<String>,
    /// The user names by uid, read once from the password file.
    pub users: Rc<HashMap<u32, String>>,
}

/// What the screen shows in place of the process table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    Table,
    Help,
    Sensors,
    Detail,
}

/// The colour roles; plain text needs none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Plain,
    Accent,
    Warn,
    Critical,
    Dim,
    /// Bold accent: the name on the first line and the boxes' titles.
    Title,
}

/// The widths from which boxes go side by side: wide (CPU beside memory and the GPUs; disks,
/// network and sensors in thirds) and medium (memory beside the GPUs, disks beside network).
const WIDE: usize = 140;
const MEDIUM: usize = 100;
/// The CPU box's share of a wide screen, in percent.
const CPU_SHARE: usize = 58;
/// The most rows a graph is given on a tall screen: CPU, memory, GPU, and each of the
/// network's two. A box that matches a taller neighbour may go past them.
const GRAPH_MAX: [usize; 4] = [8, 6, 6, 3];
/// The share of the rows the parts above the processes grow into, in percent.
const UPPER_SHARE: usize = 55;
/// The process rows a screen keeps: under 30 rows, and from 30.
const PROC_ROWS_SHORT: usize = 5;
const PROC_ROWS: usize = 8;
/// The rows a box asks for before the rest is "+N more": sensors, disks, further interfaces.
const SENSOR_ROWS: usize = 4;
const DISK_ROWS: usize = 4;
const NET_ROWS: usize = 3;
/// A bar is at least this many cells, and at most this share of its line (percent).
const BAR_MIN: usize = 8;
const BAR_SHARE: usize = 60;
/// A thread's bar in the grid, least and most cells; the cells between grid columns; the cells
/// an entry needs to show its clock as well.
const THREAD_BAR: (usize, usize) = (6, 12);
const GRID_GAP: usize = 3;
const GHZ_ROOM: usize = 6;
/// The widest mount before it is cut in the middle.
const MOUNT_MAX: usize = 24;
/// The network graphs' label column: "peak 1000 KiB/s" and two spaces; with one row each,
/// the rate's column ("↓ 1000 KiB/s" and two spaces), the peak's cells on the right, and the
/// least cells of a graph beside the peak.
const NET_LABEL: usize = 17;
const NET_ONE: usize = 14;
const PEAK_W: usize = 15;
const NET_GRAPH_MIN: usize = 12;
/// One rate's cell in the disks box: "w 1000 KiB/s".
const RATE_W: usize = 12;
/// The narrowest name in the process table before other columns give way: first the optional
/// ones, then DISK, VRAM and GPU.
const NAME_MIN: usize = 16;
const NAME_LEAST: usize = 12;
/// The keys in the process box's bottom border, as many as fit; `? help` is always kept.
const KEYS: [&str; 9] = [
    "q quit",
    "↑↓ select",
    "←→ sort",
    "/ filter",
    "enter detail",
    "t tree",
    "k end",
    "space pause",
    "s sensors",
];
const HELP_KEY: &str = "? help";
const LEAVE: &str = "ctrl-c to leave";
const LEVELS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
const SPACES: &str = "                                                                ";
const TOP: (&str, &str) = ("╭", "╮");
const BOTTOM: (&str, &str) = ("╰", "╯");
/// The process table's columns after the marker, with their widths; the name comes last.
const PROC_COLS: [(&str, usize); 9] = [
    ("CPU", 6),
    ("MEM", 9),
    ("GPU", 5),
    ("VRAM", 9),
    ("DISK", 11),
    ("THR", 5),
    ("AGE", 5),
    ("PID", 7),
    ("USER", 8),
];
/// The keys, for the help box: the keys, then what they do.
const HELP: [(&str, &str); 13] = [
    ("↑ ↓  PgUp PgDn", "select a process"),
    ("Home End", "the first or the last one"),
    ("← →", "the sort key"),
    ("r", "reverse the order"),
    ("t", "tree (f: flat; grouped is the default)"),
    (
        "/",
        "filter by name or command line; Enter keeps it, Esc clears it",
    ),
    ("space", "pause the list"),
    ("Enter", "the selected process in full"),
    ("k  K", "end it, or force it (then y or n)"),
    ("+  -", "the interval"),
    ("s", "every sensor, in place of the table"),
    ("?", "this help"),
    ("q", "quit"),
];

/// One line of the frame: its text with its colour codes, and the cells it takes. It never
/// grows past `cap`; a piece that does not fit is cut.
struct Line {
    text: String,
    width: usize,
    cap: usize,
    colour: bool,
    /// The selected row: every piece in reverse video.
    rev: bool,
    /// Where the last piece starts in `text`, and its cells: a full line's last cell is written
    /// after the erase, so the erase never meets a cursor waiting at the margin.
    last_at: usize,
    last_cells: usize,
}

impl Line {
    fn new(cap: usize, colour: bool) -> Line {
        Line {
            text: String::new(),
            width: 0,
            cap,
            colour,
            rev: false,
            last_at: 0,
            last_cells: 0,
        }
    }

    fn code(&self, role: Role) -> Option<&'static str> {
        if !self.colour {
            return None;
        }
        Some(match (self.rev, role) {
            (false, Role::Plain) => return None,
            (false, Role::Accent) => "36",
            (false, Role::Warn) => "33",
            (false, Role::Critical) => "31",
            (false, Role::Dim) => "2",
            (false, Role::Title) => "1;36",
            (true, Role::Plain) => "7",
            (true, Role::Accent) => "7;36",
            (true, Role::Warn) => "7;33",
            (true, Role::Critical) => "7;31",
            // Dim inside the reversed row would leave darker patches in it.
            (true, Role::Dim) => "7",
            (true, Role::Title) => "7;1;36",
        })
    }

    fn push(&mut self, s: &str, role: Role) {
        let room = self.cap.saturating_sub(self.width);
        if room == 0 || s.is_empty() {
            return;
        }
        // A control character in a name would move the cursor; it is shown as "?".
        let clean: String;
        let s = if s.chars().any(char::is_control) {
            clean = s
                .chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .collect();
            clean.as_str()
        } else {
            s
        };
        let (piece, cells) = fit(s, room);
        if piece.is_empty() {
            return;
        }
        self.last_at = self.text.len();
        self.last_cells = cells;
        self.width += cells;
        match self.code(role) {
            Some(code) => {
                self.text.push_str("\x1b[");
                self.text.push_str(code);
                self.text.push('m');
                self.text.push_str(piece);
                self.text.push_str("\x1b[0m");
            }
            None => self.text.push_str(piece),
        }
    }

    fn plain(&mut self, s: &str) {
        self.push(s, Role::Plain);
    }

    fn gap(&mut self, mut n: usize) {
        while n > 0 {
            let k = n.min(SPACES.len());
            self.push(&SPACES[..k], Role::Plain);
            n -= k;
            if self.width >= self.cap {
                break;
            }
        }
    }

    /// A number in `role` and its unit dim: `9.8 GiB`, `52 °C`.
    fn measure(&mut self, num: &str, unit: &str, role: Role) {
        self.push(num, role);
        self.push(unit, Role::Dim);
    }

    /// The dim dot between facts.
    fn dot(&mut self) {
        self.push(" · ", Role::Dim);
    }

    /// Adds another line whole, its codes included. It is dropped when it does not fit.
    fn join(&mut self, o: &Line) {
        if o.width == 0 || self.width + o.width > self.cap {
            return;
        }
        self.last_at = self.text.len() + o.last_at;
        self.last_cells = o.last_cells;
        self.text.push_str(&o.text);
        self.width += o.width;
    }

    fn pad_to(&mut self, width: usize) {
        if self.width < width {
            self.gap(width - self.width);
        }
    }
}

/// A line with no cap, for the parts of a border or a fact.
fn loose(colour: bool) -> Line {
    Line::new(usize::MAX, colour)
}

/// The cells a character takes: two for the wide East Asian ranges and emoji, none for the
/// combining marks, one for the rest.
fn char_cells(c: char) -> usize {
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
fn cells(s: &str) -> usize {
    s.chars().map(char_cells).sum()
}

/// The longest start of `s` within `room` cells, and its cells.
fn fit(s: &str, room: usize) -> (&str, usize) {
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
fn cut(s: &str, n: usize) -> String {
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
fn cut_middle(s: &str, n: usize) -> String {
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

/// Draws one frame into `out`: the synchronised-output marks, the cursor home, each line ended
/// with erase-to-end-of-line. `s` keeps the scroll that keeps the marker in view.
pub fn render(
    out: &mut Vec<u8>,
    r: &Reading,
    v: &View,
    s: &mut Screen,
    hist: &History,
    tree: &mut Tree,
    rank: &dyn Fn(&Proc) -> f64,
) {
    let (cols, rows, colour) = (s.cols, s.rows, s.colour);
    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    if rows > 0 && cols > 0 {
        let d = Data::new(r, v, hist, s);
        lines.push(title(r, s, cols, d.nets.first().copied()));
        let question = s.prompt.clone().filter(|p| !p.starts_with("/ "));
        let question = question.as_deref();
        let lower = v.shows("procs") || s.overlay != Overlay::Table;
        let room = rows - 1;
        let low = if lower {
            lower_need(&d, s, cols).min(room)
        } else {
            0
        };
        let budget = room - low;
        let share = if lower {
            (rows * UPPER_SHARE / 100).saturating_sub(1)
        } else {
            room
        };
        let p = plan(&d, cols, budget, share);
        let mut upper = bands(&d, &p, cols);
        if !lower {
            // Nothing below: the last band takes the rest of the screen.
            let used: usize = upper.iter().map(|b| b.height).sum();
            if let Some(last) = upper.last_mut() {
                last.height += room.saturating_sub(used);
                fill(&d, last);
            }
        }
        for band in &upper {
            draw_band(&d, &p, band, cols, &mut lines);
        }
        if lower {
            let left = rows.saturating_sub(lines.len());
            let ask = match question {
                Some(_) if left >= 3 + 2 + PROC_ROWS_SHORT => 3,
                _ => 0,
            };
            let h = left - ask;
            // A question with no room for its box goes into the process box's border.
            let inline = if ask == 0 { question } else { None };
            match s.overlay {
                Overlay::Table => lines.extend(proc_box(&d, v, s, tree, rank, h, inline)),
                Overlay::Detail => lines.extend(detail_box(&d, v, s, tree, rank, h)),
                Overlay::Help => lines.extend(help_box(cols, h, colour)),
                Overlay::Sensors => lines.extend(sensors_page(&d, cols, h)),
            }
            if let (3, Some(q)) = (ask, question) {
                lines.extend(question_box(q, cols, colour));
            }
        }
        lines.truncate(rows);
        while lines.len() < rows {
            let mut l = Line::new(cols, colour);
            l.gap(cols);
            lines.push(l);
        }
    }
    emit(out, &lines, cols);
}

/// Writes the lines, each ended with erase-to-end-of-line, all in one write. A line that fills
/// the width has its erase before its last cell: after the last cell the cursor waits at the
/// margin, where an erase would take that cell away.
fn emit(out: &mut Vec<u8>, lines: &[Line], cols: usize) {
    out.extend_from_slice(b"\x1b[?2026h\x1b[H");
    for (i, l) in lines.iter().enumerate() {
        if l.width >= cols && l.last_cells == 1 {
            out.extend_from_slice(&l.text.as_bytes()[..l.last_at]);
            out.extend_from_slice(b"\x1b[K");
            out.extend_from_slice(&l.text.as_bytes()[l.last_at..]);
        } else {
            out.extend_from_slice(l.text.as_bytes());
            out.extend_from_slice(b"\x1b[K");
        }
        if i + 1 < lines.len() {
            out.extend_from_slice(b"\r\n");
        }
    }
    if lines.last().is_none_or(|l| l.width < cols) {
        out.extend_from_slice(b"\x1b[J");
    }
    out.extend_from_slice(b"\x1b[?2026l");
}

/// What one frame draws from: the reading, the parts the view shows, and the sensors in order.
struct Data<'a> {
    r: &'a Reading,
    hist: &'a History,
    interval_s: f64,
    colour: bool,
    disks: Vec<&'a DiskSample>,
    nets: Vec<&'a aether::sys::NetSample>,
    /// Every sensor: the hottest temperatures first, then fans, then power.
    sensors: Vec<&'a Sensor>,
    /// The widest label and value pair of the sensors.
    sensor_w: usize,
    /// The boxes there is something to show in, top to bottom.
    kinds: Vec<Kind>,
    /// Whether the first interface shown is the one the history follows.
    net_hist: bool,
}

impl<'a> Data<'a> {
    fn new(r: &'a Reading, v: &View, hist: &'a History, s: &Screen) -> Data<'a> {
        let disks: Vec<&DiskSample> = if v.shows("disk") {
            r.disks.iter().filter(|d| v.shows_disk(&d.mount)).collect()
        } else {
            Vec::new()
        };
        let nets: Vec<&aether::sys::NetSample> = if v.shows("net") {
            r.nets.iter().filter(|n| v.shows_net(&n.name)).collect()
        } else {
            Vec::new()
        };
        let mut sensors: Vec<&Sensor> = r.sensors.iter().collect();
        sensors.sort_by(|a, b| {
            kind_rank(a.kind)
                .cmp(&kind_rank(b.kind))
                .then(b.value.total_cmp(&a.value))
        });
        let sensor_w = sensors
            .iter()
            .map(|x| {
                let (name, num, unit, _) = sensor_pair(x);
                cells(&name) + 2 + cells(&num) + cells(unit)
            })
            .max()
            .unwrap_or(0);
        let net_hist = match (nets.first(), r.nets.first()) {
            (Some(a), Some(b)) => a.name == b.name,
            _ => false,
        };
        let mut kinds = Vec::new();
        let c = &r.cpu;
        if v.shows("cpu") && (c.pct.is_some() || c.load.is_some() || !c.threads.is_empty()) {
            kinds.push(Kind::Cpu);
        }
        if v.shows("mem") && r.mem.total > 0 {
            kinds.push(Kind::Mem);
        }
        if v.shows("gpu") {
            for (i, g) in r.gpus.iter().enumerate() {
                if gpu_lines(g) > 0 {
                    kinds.push(Kind::Gpu(i));
                }
            }
        }
        if !disks.is_empty() {
            kinds.push(Kind::Disks);
        }
        if !nets.is_empty() {
            kinds.push(Kind::Net);
        }
        if v.shows("sensors") && !sensors.is_empty() {
            kinds.push(Kind::Sensors);
        }
        Data {
            r,
            hist,
            interval_s: s.interval_s,
            colour: s.colour,
            disks,
            nets,
            sensors,
            sensor_w,
            kinds,
            net_hist,
        }
    }
}

/// The tiers by width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Narrow,
    Medium,
    Wide,
}

fn tier(cols: usize) -> Tier {
    if cols >= WIDE {
        Tier::Wide
    } else if cols >= MEDIUM {
        Tier::Medium
    } else {
        Tier::Narrow
    }
}

/// The boxes above the processes. `MemGpu` and `DiskNet` are the narrow screen's shared boxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Cpu,
    Mem,
    Gpu(usize),
    Disks,
    Net,
    Sensors,
    MemGpu,
    DiskNet,
}

/// What a frame shows above the processes: the graph rows, and what a short screen gave up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    /// Graph rows: CPU, memory, GPU, and each of the network's two.
    graph: [usize; 4],
    sensors: bool,
    /// The thread grid; without it, one cell per thread.
    grid: bool,
    /// Every disk and interface; without it, the first of each.
    all: bool,
    /// The memory and GPU graphs.
    side: bool,
    /// Memory with the GPU, and disks with network, in one box each (narrow only).
    merge: bool,
    /// Whole boxes left out, from the least wanted.
    dropped: usize,
}

impl Plan {
    fn full() -> Plan {
        Plan {
            graph: [1; 4],
            sensors: true,
            grid: true,
            all: true,
            side: true,
            merge: false,
            dropped: 0,
        }
    }

    /// Gives up (or takes back) one thing, in the order a short screen gives them up.
    fn set(&mut self, step: usize, on: bool) {
        match step {
            0 => self.sensors = on,
            1 => self.grid = on,
            2 => self.all = on,
            3 => self.side = on,
            _ => self.merge = !on,
        }
    }
}

/// A band of boxes side by side: its height, and its columns (each a width and the boxes on top
/// of each other in it, with their rows inside the borders).
struct Band {
    height: usize,
    cols: Vec<(usize, Vec<(Kind, usize)>)>,
}

/// The boxes a plan keeps, top to bottom.
fn effective(kinds: &[Kind], p: &Plan) -> Vec<Kind> {
    let mut out: Vec<Kind> = kinds
        .iter()
        .copied()
        .filter(|k| p.sensors || *k != Kind::Sensors)
        .collect();
    if p.merge {
        let gpu = out.iter().any(|k| matches!(k, Kind::Gpu(_)));
        if gpu && out.contains(&Kind::Mem) {
            out.retain(|k| !matches!(k, Kind::Gpu(_)));
            for k in out.iter_mut().filter(|k| **k == Kind::Mem) {
                *k = Kind::MemGpu;
            }
        }
        if out.contains(&Kind::Disks) && out.contains(&Kind::Net) {
            out.retain(|k| *k != Kind::Net);
            for k in out.iter_mut().filter(|k| **k == Kind::Disks) {
                *k = Kind::DiskNet;
            }
        }
    }
    for _ in 0..p.dropped {
        let least = (0..out.len()).min_by_key(|&i| drop_rank(out[i]));
        if let Some(i) = least {
            out.remove(i);
        }
    }
    out
}

/// Which box goes first when whole boxes must go.
fn drop_rank(k: Kind) -> (u8, usize) {
    match k {
        Kind::Sensors => (0, 0),
        Kind::DiskNet => (1, 0),
        Kind::Disks => (2, 0),
        Kind::Net => (3, 0),
        Kind::Gpu(i) => (4, usize::MAX - i),
        Kind::MemGpu => (5, 0),
        Kind::Mem => (6, 0),
        Kind::Cpu => (7, 0),
    }
}

/// The boxes in bands from the top, by tier: each band its columns with their widths.
fn arrange(kinds: &[Kind], cols: usize) -> Vec<Vec<(usize, Vec<Kind>)>> {
    let pick = |f: fn(&Kind) -> bool| -> Vec<Kind> { kinds.iter().copied().filter(f).collect() };
    let cpu = kinds.contains(&Kind::Cpu);
    let mut bands = Vec::new();
    match tier(cols) {
        Tier::Wide => {
            let right = pick(|k| matches!(k, Kind::Mem | Kind::Gpu(_) | Kind::MemGpu));
            match (cpu, right.is_empty()) {
                (true, false) => {
                    let wl = cols * CPU_SHARE / 100;
                    bands.push(vec![(wl, vec![Kind::Cpu]), (cols - wl, right)]);
                }
                (true, true) => bands.push(vec![(cols, vec![Kind::Cpu])]),
                (false, false) => bands.push(vec![(cols, right)]),
                (false, true) => {}
            }
            let low =
                pick(|k| matches!(k, Kind::Disks | Kind::Net | Kind::Sensors | Kind::DiskNet));
            if !low.is_empty() {
                bands.push(split(cols, low));
            }
        }
        Tier::Medium => {
            if cpu {
                bands.push(vec![(cols, vec![Kind::Cpu])]);
            }
            for group in [
                pick(|k| matches!(k, Kind::Mem | Kind::Gpu(_) | Kind::MemGpu)),
                pick(|k| matches!(k, Kind::Disks | Kind::Net | Kind::DiskNet)),
                pick(|k| *k == Kind::Sensors),
            ] {
                if !group.is_empty() {
                    bands.push(split(cols, group));
                }
            }
        }
        Tier::Narrow => {
            for k in kinds {
                bands.push(vec![(cols, vec![*k])]);
            }
        }
    }
    bands
}

/// Boxes side by side in equal shares of the width.
fn split(cols: usize, boxes: Vec<Kind>) -> Vec<(usize, Vec<Kind>)> {
    let n = boxes.len().max(1);
    boxes
        .into_iter()
        .enumerate()
        .map(|(i, k)| (cols * (i + 1) / n - cols * i / n, vec![k]))
        .collect()
}

/// The bands a plan draws, each as tall as its tallest column; the others grow to match.
fn bands(d: &Data, p: &Plan, cols: usize) -> Vec<Band> {
    let kinds = effective(&d.kinds, p);
    arrange(&kinds, cols)
        .into_iter()
        .map(|band| {
            let cols: Vec<(usize, Vec<(Kind, usize)>)> = band
                .into_iter()
                .map(|(w, ks)| {
                    let sized = ks
                        .into_iter()
                        .map(|k| (k, need(d, k, w.saturating_sub(4), p)))
                        .collect();
                    (w, sized)
                })
                .collect();
            let height = cols
                .iter()
                .map(|(_, b)| b.iter().map(|(_, r)| r + 2).sum::<usize>())
                .max()
                .unwrap_or(0);
            let mut band = Band { height, cols };
            fill(d, &mut band);
            band
        })
        .collect()
}

/// Gives a column shorter than its band the rows it lacks: to its graphs, shared evenly, or
/// to its last box when it has none.
fn fill(d: &Data, band: &mut Band) {
    for (_, boxes) in band.cols.iter_mut() {
        let used: usize = boxes.iter().map(|(_, r)| r + 2).sum();
        let extra = band.height.saturating_sub(used);
        if extra == 0 {
            continue;
        }
        let growers: Vec<usize> = (0..boxes.len()).filter(|&i| grows(d, boxes[i].0)).collect();
        if growers.is_empty() {
            if let Some(b) = boxes.last_mut() {
                b.1 += extra;
            }
            continue;
        }
        let n = growers.len();
        for (j, &i) in growers.iter().enumerate() {
            boxes[i].1 += extra / n + usize::from(j < extra % n);
        }
    }
}

/// Whether a box has a graph to give more rows to.
fn grows(d: &Data, k: Kind) -> bool {
    match k {
        Kind::Disks | Kind::Sensors => false,
        Kind::Gpu(i) => i == 0,
        Kind::Net | Kind::DiskNet => d.net_hist,
        Kind::Cpu | Kind::Mem | Kind::MemGpu => true,
    }
}

/// The rows a box needs inside its borders under a plan, `cw` cells wide inside.
fn need(d: &Data, k: Kind, cw: usize, p: &Plan) -> usize {
    let side = |g: usize| if p.side { g } else { 0 };
    match k {
        Kind::Cpu => cpu_lines(d) + p.graph[0] + thread_rows(d, cw, p.grid),
        Kind::Mem => mem_lines(d) + side(p.graph[1]),
        Kind::Gpu(i) => {
            d.r.gpus.get(i).map_or(0, gpu_lines) + if i == 0 { side(p.graph[2]) } else { 0 }
        }
        Kind::Disks => disk_need(d, p.all),
        Kind::Net => net_need(d, p.all) + 2 * p.graph[3],
        Kind::Sensors => sensor_need(d, cw).min(SENSOR_ROWS),
        Kind::MemGpu => {
            mem_lines(d)
                + d.r.gpus.iter().map(gpu_lines).sum::<usize>()
                + side(p.graph[1])
                + side(p.graph[2])
        }
        Kind::DiskNet => disk_need(d, p.all) + net_need(d, p.all) + 2 * p.graph[3],
    }
}

/// The plan for the room there is. A screen too short for everything gives up, in order: the
/// sensors, the thread grid, the disks and interfaces past the first, the memory and GPU
/// graphs, then (narrow) the shared boxes, then whole boxes, until the parts above the
/// processes fit in `budget` rows. What is left of `share` goes back to what was given up
/// last, then to the graphs, a row at a time.
fn plan(d: &Data, cols: usize, budget: usize, share: usize) -> Plan {
    let height = |p: &Plan| bands(d, p, cols).iter().map(|b| b.height).sum::<usize>();
    let steps = if tier(cols) == Tier::Narrow { 5 } else { 4 };
    let mut p = Plan::full();
    let mut given = 0;
    while given < steps && height(&p) > budget {
        p.set(given, false);
        given += 1;
    }
    let count = effective(&d.kinds, &p).len();
    while p.dropped < count && height(&p) > budget {
        p.dropped += 1;
    }
    let limit = share.min(budget);
    for step in (0..given.min(4)).rev() {
        if step == 0 && p.dropped > 0 {
            continue;
        }
        let mut t = p;
        t.set(step, true);
        if height(&t) <= limit {
            p = t;
        }
    }
    // The network's graphs grow only in a band of their own; beside the disks and the sensors
    // they take the rows those need, so no box beside them stands half empty.
    let kinds = effective(&d.kinds, &p);
    let alone = arrange(&kinds, cols).iter().all(|band| {
        band.len() == 1
            || !band
                .iter()
                .any(|(_, ks)| ks.iter().any(|k| matches!(k, Kind::Net | Kind::DiskNet)))
    });
    let mut stuck = [false, false, false, !alone];
    while stuck.iter().any(|s| !s) {
        for i in 0..4 {
            if stuck[i] {
                continue;
            }
            let mut t = p;
            t.graph[i] += 1;
            // Beside the CPU, memory and the GPUs grow no taller than the CPU box can.
            let past = matches!(beside_cpu(d, &t, cols), Some((cpu, right)) if right > cpu);
            if p.graph[i] >= GRAPH_MAX[i] || height(&t) > limit || (i > 0 && past) {
                stuck[i] = true;
            } else {
                p = t;
            }
        }
    }
    p
}

/// On a wide screen with memory or a GPU beside the CPU: the CPU box's height with its graph at
/// its most rows, and the height of the column beside it.
fn beside_cpu(d: &Data, p: &Plan, cols: usize) -> Option<(usize, usize)> {
    let kinds = effective(&d.kinds, p);
    let bands = arrange(&kinds, cols);
    let band = bands
        .first()
        .filter(|b| b.len() == 2 && tier(cols) == Tier::Wide)?;
    let (wl, wr) = (band[0].0.saturating_sub(4), band[1].0.saturating_sub(4));
    let mut most = *p;
    most.graph[0] = GRAPH_MAX[0];
    let cpu = need(d, Kind::Cpu, wl, &most) + 2;
    let right = band[1].1.iter().map(|k| need(d, *k, wr, p) + 2).sum();
    Some((cpu, right))
}

/// The rows the part below needs: the process box with its least rows (or the overlay in its
/// place), and the question's box.
fn lower_need(d: &Data, s: &Screen, cols: usize) -> usize {
    let table = 3 + if s.rows < 30 {
        PROC_ROWS_SHORT
    } else {
        PROC_ROWS
    };
    let ask = match s.prompt.as_deref() {
        Some(p) if !p.starts_with("/ ") => 3,
        _ => 0,
    };
    let own = match s.overlay {
        Overlay::Table | Overlay::Detail => table,
        Overlay::Help => 2 + HELP.len(),
        Overlay::Sensors => 2 + sensor_need(d, cols.saturating_sub(4)).max(1),
    };
    own.max(table) + ask
}

/// Draws a band's boxes side by side into its rows of the frame.
fn draw_band(d: &Data, p: &Plan, band: &Band, cols: usize, lines: &mut Vec<Line>) {
    let mut rows: Vec<Line> = (0..band.height)
        .map(|_| Line::new(cols, d.colour))
        .collect();
    for (w, boxes) in &band.cols {
        let mut at = 0;
        for &(k, r) in boxes {
            let drawn = match k {
                Kind::Cpu => cpu_box(d, *w, r, p.grid),
                Kind::Mem => mem_box(d, *w, r),
                Kind::Gpu(i) => gpu_box(d, i, *w, r),
                Kind::Disks => disk_box(d, *w, r),
                Kind::Net => net_box(d, *w, r, p.all),
                Kind::Sensors => sensor_box(d, *w, r),
                Kind::MemGpu => mem_gpu_box(d, *w, r),
                Kind::DiskNet => disk_net_box(d, *w, r, p.all),
            };
            for l in drawn {
                if let Some(row) = rows.get_mut(at) {
                    row.join(&l);
                }
                at += 1;
            }
        }
    }
    for row in rows.iter_mut() {
        row.pad_to(cols);
    }
    lines.extend(rows);
}

// ---------------------------------------------------------------------------------------------
// Boxes

/// A box `w` wide: its top border, `rows` body lines between the side borders with a space of
/// padding on each side, and its bottom border.
fn framed(
    w: usize,
    colour: bool,
    top: Line,
    body: Vec<Line>,
    rows: usize,
    bottom: Line,
) -> Vec<Line> {
    let mut out = Vec::with_capacity(rows + 2);
    out.push(top);
    let mut body = body.into_iter();
    for _ in 0..rows {
        let mut l = Line::new(w, colour);
        l.push("│", Role::Dim);
        l.gap(1);
        if let Some(b) = body.next() {
            l.join(&b);
        }
        l.pad_to(w.saturating_sub(1));
        l.push("│", Role::Dim);
        out.push(l);
    }
    out.push(bottom);
    out
}

/// A border line: its corners, the left part after `─ `, dashes, and the right part before ` ─`.
/// The right part goes first when both do not fit, then the left.
fn border(
    w: usize,
    colour: bool,
    ends: (&str, &str),
    left: Option<&Line>,
    right: Option<&Line>,
) -> Line {
    let lw = left.map_or(0, |x| if x.width > 0 { x.width + 2 } else { 0 });
    let rw = right.map_or(0, |x| if x.width > 0 { x.width + 3 } else { 0 });
    let rw = if 4 + lw + rw <= w { rw } else { 0 };
    let lw = if 4 + lw + rw <= w { lw } else { 0 };
    let mut l = Line::new(w, colour);
    l.push(ends.0, Role::Dim);
    l.push("─", Role::Dim);
    if let (true, Some(x)) = (lw > 0, left) {
        l.gap(1);
        l.join(x);
        l.gap(1);
    }
    l.push(&"─".repeat(w.saturating_sub(l.width + rw + 1)), Role::Dim);
    if let (true, Some(x)) = (rw > 0, right) {
        l.gap(1);
        l.join(x);
        l.push(" ─", Role::Dim);
    }
    l.push(ends.1, Role::Dim);
    l
}

/// A top border: the title in bold accent, then as many facts as fit, a dim dot between them.
fn top(w: usize, colour: bool, title: &str, facts: &[Line], right: Option<&Line>) -> Line {
    let rw = right.map_or(0, |r| r.width + 3);
    let room = w.saturating_sub(6 + rw);
    let mut head = loose(colour);
    head.push(title, Role::Title);
    for (n, f) in facts.iter().filter(|f| f.width > 0).enumerate() {
        if head.width + 3 + f.width > room {
            break;
        }
        head.push(if n == 0 { " ─ " } else { " · " }, Role::Dim);
        head.join(f);
    }
    border(w, colour, TOP, Some(&head), right)
}

/// A bottom border with the span of the box's graph on the right.
fn bottom(w: usize, colour: bool, left: Option<&Line>, span: Option<&Line>) -> Line {
    border(w, colour, BOTTOM, left, span)
}

/// Keys for a border: each key plain and its word dim, a dim dot between them.
fn key_items(items: &[&str], colour: bool) -> Line {
    let mut l = loose(colour);
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            l.dot();
        }
        key_item(&mut l, item);
    }
    l
}

fn key_item(l: &mut Line, item: &str) {
    match item.split_once(' ') {
        Some((key, word)) => {
            l.plain(key);
            l.push(" ", Role::Dim);
            l.push(word, Role::Dim);
        }
        None => l.plain(item),
    }
}

/// One fact for a border: a number and its unit.
fn fact(colour: bool, num: &str, unit: &str, role: Role) -> Line {
    let mut l = loose(colour);
    l.measure(num, unit, role);
    l
}

fn words(colour: bool, s: &str, role: Role) -> Line {
    let mut l = loose(colour);
    l.push(s, role);
    l
}

/// The graph's span for the bottom border: the time its columns cover.
fn span(d: &Data, shown: usize) -> Option<Line> {
    if shown == 0 {
        return None;
    }
    let secs = (shown as f64 * d.interval_s).round() as u64;
    let text = if secs < 120 {
        format!("last {secs} s")
    } else {
        format!("last {} min", sig3(secs as f64 / 60.0))
    };
    Some(words(d.colour, &text, Role::Dim))
}

/// "+N more" for a bottom border.
fn more(colour: bool, n: usize) -> Option<Line> {
    (n > 0).then(|| words(colour, &format!("+{n} more"), Role::Dim))
}

/// The columns from which each overview item gets a meter bar, and the bar's cells.
const OVERVIEW_BARS: usize = 140;
const OVERVIEW_CELLS: usize = 6;

/// How busy the first network interface is: a share of its link, or the plain rate when the
/// link speed is not known.
#[derive(Clone, Copy, Debug, PartialEq)]
enum NetLoad {
    Pct(f32),
    Rate(u64),
}

/// The larger of the two directions, as a percent of the link speed (bits against bits) or,
/// without a link speed, as a rate. `None` when no rate is read.
fn net_load(n: &aether::sys::NetSample) -> Option<NetLoad> {
    let rate = match (n.rx_bps, n.tx_bps) {
        (None, None) => return None,
        (a, b) => a.unwrap_or(0).max(b.unwrap_or(0)),
    };
    match n.link_bps.filter(|&l| l > 0) {
        Some(link) => Some(NetLoad::Pct(
            (rate as f64 * 8.0 / link as f64 * 100.0) as f32,
        )),
        None => Some(NetLoad::Rate(rate)),
    }
}

/// A percentage with one decimal under 10 and none from 10, the sign not included.
fn pct_short(x: f32) -> String {
    if x < 9.95 {
        format!("{x:.1}")
    } else {
        format!("{x:.0}")
    }
}

/// One overview item: the label dim, a bar when asked for, the number in its role.
fn overview_item(l: &mut Line, label: &str, load: NetLoad, bars: bool) {
    l.push(label, Role::Dim);
    l.push(" ", Role::Plain);
    match load {
        NetLoad::Pct(p) => {
            let role = load_role(p);
            if bars {
                bar(l, p / 100.0, OVERVIEW_CELLS, role);
                l.push(" ", Role::Plain);
            }
            l.measure(&pct_short(p), "%", role);
        }
        NetLoad::Rate(b) => size_item(l, b, "/s", Role::Plain),
    }
}

/// The overview: CPU, GPU, RAM and network, as far as each is read. `gpu` and `net` are the
/// items still allowed.
fn overview(
    r: &Reading,
    net: Option<&aether::sys::NetSample>,
    colour: bool,
    bars: bool,
    gpu: bool,
    netw: bool,
) -> Line {
    let mut items: Vec<(&str, NetLoad)> = Vec::new();
    if let Some(p) = r.cpu.pct {
        items.push(("CPU", NetLoad::Pct(p)));
    }
    if gpu {
        if let Some(p) = r.gpus.first().and_then(|g| g.pct) {
            items.push(("GPU", NetLoad::Pct(p)));
        }
    }
    if r.mem.total > 0 {
        let p = r.mem.used as f64 / r.mem.total as f64 * 100.0;
        items.push(("RAM", NetLoad::Pct(p as f32)));
    }
    if netw {
        if let Some(load) = net.and_then(net_load) {
            items.push(("NET", load));
        }
    }
    let mut l = loose(colour);
    for (i, (label, load)) in items.into_iter().enumerate() {
        if i > 0 {
            l.gap(3);
        }
        overview_item(&mut l, label, load, bars);
    }
    l
}

/// The right part: uptime, threads, `paused`, interval, each as far as it is kept.
fn title_right(r: &Reading, s: &Screen, up: bool, threads: bool, interval: bool) -> Line {
    let mut right = loose(s.colour);
    let mut first = true;
    let mut sep = |l: &mut Line| {
        if !first {
            l.dot();
        }
        first = false;
    };
    if up {
        sep(&mut right);
        right.plain(&uptime_short(r.host.uptime_s));
    }
    if threads {
        sep(&mut right);
        right.plain(&format!("{} threads", r.host.threads));
    }
    if s.paused {
        sep(&mut right);
        right.push("paused", Role::Warn);
    }
    if interval {
        sep(&mut right);
        right.measure(&format!("{:.1}", s.interval_s), " s", Role::Plain);
    }
    right
}

/// The first line: the name, the overview of CPU, GPU, RAM and network; on the right the
/// battery (when there is one and it fits), the uptime, the threads, `paused` and the interval.
/// When it is too narrow it gives up the bars, the interval, the threads, the uptime, the
/// network and the GPU, in that order; CPU and RAM stay.
fn title(r: &Reading, s: &Screen, cols: usize, net: Option<&aether::sys::NetSample>) -> Line {
    let colour = s.colour;
    let brand = 1 + 6 + 3;
    // (bars, interval, threads, uptime, network, gpu)
    let steps = [
        (true, true, true, true, true, true),
        (false, true, true, true, true, true),
        (false, false, true, true, true, true),
        (false, false, false, true, true, true),
        (false, false, false, false, true, true),
        (false, false, false, false, false, true),
        (false, false, false, false, false, false),
    ];
    let mut chosen = None;
    for (bars, interval, threads, up, netw, gpu) in steps {
        let bars = bars && cols >= OVERVIEW_BARS;
        let o = overview(r, net, colour, bars, gpu, netw);
        let right = title_right(r, s, up, threads, interval);
        let need = brand + o.width + if right.width > 0 { 3 + right.width } else { 0 } + 1;
        let fits = need <= cols;
        chosen = Some((o, right));
        if fits {
            break;
        }
    }
    let Some((over, right)) = chosen else {
        return Line::new(cols, colour);
    };
    let mut bat = loose(colour);
    if let Some(b) = &r.battery {
        battery_text(&mut bat, b);
    }
    let used = brand + over.width + if right.width > 0 { 3 + right.width } else { 0 } + 1;
    let with_bat = bat.width > 0 && used + bat.width + 3 <= cols;
    let tail = right.width + 1 + if with_bat { bat.width + 3 } else { 0 };
    let mut l = Line::new(cols, colour);
    l.gap(1);
    l.push("aether", Role::Title);
    l.gap(3);
    l.join(&over);
    l.pad_to(cols.saturating_sub(tail));
    if with_bat {
        l.join(&bat);
        if right.width > 0 {
            l.dot();
        }
    }
    l.join(&right);
    l.pad_to(cols);
    l
}

fn battery_text(l: &mut Line, b: &BatterySample) {
    l.push("battery ", Role::Dim);
    l.measure(&format!("{:.0}", b.pct), "%", Role::Plain);
    l.dot();
    l.plain(b.state);
    if let Some(p) = b.power_w {
        l.dot();
        l.measure(&format!("{p:.0}"), " W", Role::Plain);
    }
    if let Some(m) = b.minutes_left {
        l.dot();
        l.plain(&format!("{}h {:02}m left", m / 60, m % 60));
    }
}

// The CPU box ----------------------------------------------------------------------------------

fn cpu_lines(d: &Data) -> usize {
    usize::from(d.r.cpu.pct.is_some() || d.r.cpu.load.is_some())
}

/// The CPU: its clock, temperature and power in the border; the total with its bar and the
/// load; the graph of the total; the threads.
fn cpu_box(d: &Data, w: usize, rows: usize, grid: bool) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let c = &d.r.cpu;
    let mut facts = Vec::new();
    if let Some(mhz) = c.mhz {
        facts.push(fact(
            colour,
            &format!("{:.1}", f64::from(mhz) / 1000.0),
            " GHz",
            Role::Plain,
        ));
    }
    if let Some(t) = c.temp_c {
        facts.push(fact(colour, &format!("{t:.0}"), " °C", temp_role(t)));
    }
    if let Some(p) = c.power_w {
        facts.push(fact(colour, &format!("{p:.0}"), " W", Role::Plain));
    }
    let mut body = Vec::with_capacity(rows);
    if cpu_lines(d) > 0 {
        body.push(cpu_summary(d, cw));
    }
    let threads = if grid {
        thread_grid(d, cw)
    } else {
        thread_cells(d, cw)
    };
    let g = rows.saturating_sub(body.len() + threads.len());
    let (graph, shown, _) = graph(&d.hist.cpu, g, cw, Some(100.0), true, colour);
    body.extend(graph);
    body.extend(threads);
    let foot = bottom(w, colour, None, span(d, shown).as_ref());
    framed(
        w,
        colour,
        top(w, colour, "CPU", &facts, None),
        body,
        rows,
        foot,
    )
}

/// The total, its bar to the load, and the load on the right.
fn cpu_summary(d: &Data, cw: usize) -> Line {
    let c = &d.r.cpu;
    let mut load = loose(d.colour);
    if let Some([a, b, x]) = c.load {
        load.push("load ", Role::Dim);
        load.plain(&format!("{a:.1} {b:.1} {x:.1}"));
    }
    let mut l = Line::new(cw, d.colour);
    if let Some(p) = c.pct {
        let role = load_role(p);
        pct_cell(&mut l, p, role);
        l.gap(2);
        let tail = if load.width > 0 { load.width + 2 } else { 0 };
        let bw = flex(cw.saturating_sub(l.width + tail), cw);
        bar(&mut l, p / 100.0, bw, role);
    }
    l.pad_to(cw.saturating_sub(load.width));
    l.join(&load);
    l
}

/// The rows the threads take: the grid, or one cell per thread.
fn thread_rows(d: &Data, cw: usize, grid: bool) -> usize {
    let n = d.r.cpu.threads.len();
    if n == 0 {
        return 0;
    }
    match grid_shape(n, cw) {
        Some((_, rows, _)) if grid => rows,
        _ => cells_shape(n, cw).2,
    }
}

fn digits(n: usize) -> usize {
    let mut d = 1;
    let mut n = n / 10;
    while n > 0 {
        d += 1;
        n /= 10;
    }
    d
}

/// The thread grid's shape for `n` threads in `cw` cells: as many columns as fit with bars of
/// six cells at least, the rows they need, then the columns those rows need, spread over the
/// whole width. Returns the columns, the rows and an entry's cells.
fn grid_shape(n: usize, cw: usize) -> Option<(usize, usize, usize)> {
    if n == 0 {
        return None;
    }
    let least = digits(n - 1) + 1 + THREAD_BAR.0 + 1 + 4;
    let k = ((cw + GRID_GAP) / (least + GRID_GAP)).min(n);
    if k == 0 {
        return None;
    }
    let rows = n.div_ceil(k);
    let k = n.div_ceil(rows);
    Some((k, rows, (cw + GRID_GAP) / k - GRID_GAP))
}

/// The threads as a grid of `id bar pct` entries, down each column first; with room, each
/// thread's clock in dim after its percent.
fn thread_grid(d: &Data, cw: usize) -> Vec<Line> {
    let t = &d.r.cpu.threads;
    let Some((k, rows, entry)) = grid_shape(t.len(), cw) else {
        return thread_cells(d, cw);
    };
    let id = digits(t.len() - 1);
    let bw = entry.saturating_sub(id + 6).min(THREAD_BAR.1);
    let mhz = &d.r.cpu.threads_mhz;
    let ghz = entry >= id + 6 + bw + GHZ_ROOM && mhz.len() == t.len();
    let mut out = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut l = Line::new(cw, d.colour);
        for j in 0..k {
            let i = j * rows + r;
            let Some(&pct) = t.get(i) else { break };
            l.pad_to(j * (cw + GRID_GAP) / k);
            let role = load_role(pct);
            l.push(&format!("{i:>id$}"), Role::Dim);
            l.gap(1);
            bar(&mut l, pct / 100.0, bw, role);
            l.gap(1);
            pct_cell(&mut l, pct, role);
            if let (true, Some(m)) = (ghz, mhz.get(i)) {
                l.gap(2);
                l.push(&format!("{:.1}", f64::from(*m) / 1000.0), Role::Dim);
            }
        }
        out.push(l);
    }
    out
}

/// One cell per thread: numbered and spread over the width when that takes two rows at most,
/// else the cells alone.
fn cells_shape(n: usize, cw: usize) -> (bool, usize, usize) {
    let numbered = (cw + 2) / (digits(n.saturating_sub(1)) + 4);
    if numbered > 0 && n.div_ceil(numbered) <= 2 {
        return (true, numbered, n.div_ceil(numbered));
    }
    // Spaced cells, or packed ones when spaced ones would take more than three rows.
    let per = cw.div_ceil(2).max(1);
    let per = if n.div_ceil(per) > 3 { cw.max(1) } else { per };
    (false, per, n.div_ceil(per))
}

fn thread_cells(d: &Data, cw: usize) -> Vec<Line> {
    let t = &d.r.cpu.threads;
    let (numbered, per, rows) = cells_shape(t.len(), cw);
    let id = digits(t.len().saturating_sub(1));
    let cols = t.len().div_ceil(rows.max(1));
    let mut out = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut l = Line::new(cw, d.colour);
        for j in 0..per {
            let i = r * per + j;
            let Some(&pct) = t.get(i) else { break };
            let cell = LEVELS[((pct.clamp(0.0, 100.0) / 100.0) * 7.0).round() as usize];
            if numbered {
                l.pad_to(j * (cw + 2) / cols.max(1));
                l.push(&format!("{i:>id$}"), Role::Dim);
                l.gap(1);
            } else if j > 0 && per < cw {
                l.gap(1);
            }
            l.push(cell, load_role(pct));
        }
        out.push(l);
    }
    out
}

// Memory and GPU ---------------------------------------------------------------------------

/// A line of a box with a label, a value, and a bar with its percent flush right.
struct Meter {
    label: &'static str,
    value: Line,
    frac: Option<f32>,
    /// The bar's colour; `None` colours it by its level.
    role: Option<Role>,
}

/// Meters with their labels, values and bars in columns.
fn meters(cw: usize, colour: bool, items: &[Meter]) -> Vec<Line> {
    let lw = items.iter().map(|m| cells(m.label)).max().unwrap_or(0);
    let lw = if lw > 0 { lw + 2 } else { 0 };
    let vw = items.iter().map(|m| m.value.width).max().unwrap_or(0);
    let vw = if vw > 0 { vw + 2 } else { 0 };
    let bw = flex(cw.saturating_sub(lw + vw + 5), cw);
    items
        .iter()
        .map(|m| {
            let mut l = Line::new(cw, colour);
            l.push(m.label, Role::Dim);
            l.pad_to(lw);
            l.join(&m.value);
            if let Some(f) = m.frac {
                let p = f * 100.0;
                let role = m.role.unwrap_or_else(|| load_role(p));
                if bw > 0 {
                    l.pad_to(cw.saturating_sub(bw + 5));
                    bar(&mut l, f, bw, role);
                    l.gap(1);
                } else {
                    l.pad_to(cw.saturating_sub(4));
                }
                pct_cell(&mut l, p, role);
            }
            l
        })
        .collect()
}

fn mem_lines(d: &Data) -> usize {
    2 + usize::from(d.r.mem.swap_total > 0)
}

fn mem_meters(d: &Data) -> Vec<Meter> {
    let (m, colour) = (&d.r.mem, d.colour);
    let total = m.total.max(1) as f32;
    let mut used = loose(colour);
    size_pair(&mut used, m.used, m.total);
    let mut cached = loose(colour);
    size_item(&mut cached, m.cached, "", Role::Plain);
    let mut out = vec![
        Meter {
            label: "used",
            value: used,
            frac: Some(m.used as f32 / total),
            role: None,
        },
        // A big cache is not a warning: its bar stays in the accent.
        Meter {
            label: "cached",
            value: cached,
            frac: Some(m.cached as f32 / total),
            role: Some(Role::Accent),
        },
    ];
    if m.swap_total > 0 {
        let mut swap = loose(colour);
        size_pair(&mut swap, m.swap_used, m.swap_total);
        out.push(Meter {
            label: "swap",
            value: swap,
            frac: Some(m.swap_used as f32 / m.swap_total as f32),
            role: None,
        });
    }
    out
}

/// Memory: used, cached and swap with their bars, then the graph of memory used.
fn mem_box(d: &Data, w: usize, rows: usize) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let mut body = meters(cw, colour, &mem_meters(d));
    let g = rows.saturating_sub(body.len());
    let (graph, shown, _) = graph(&d.hist.mem, g, cw, Some(100.0), true, colour);
    body.extend(graph);
    let foot = bottom(w, colour, None, span(d, shown).as_ref());
    framed(
        w,
        colour,
        top(w, colour, "Memory", &[], None),
        body,
        rows,
        foot,
    )
}

/// The lines a GPU's numbers take: use, VRAM, and its temperature, fan and power.
fn gpu_lines(g: &GpuSample) -> usize {
    usize::from(g.pct.is_some())
        + usize::from(g.vram_used.is_some())
        + usize::from(g.temp_c.is_some() || g.fan_rpm.is_some() || g.power_w.is_some())
}

fn gpu_meters(g: &GpuSample, label: &'static str, colour: bool) -> Vec<Meter> {
    let mut out = Vec::new();
    if let Some(p) = g.pct {
        out.push(Meter {
            label,
            value: loose(colour),
            frac: Some(p / 100.0),
            role: None,
        });
    }
    match (g.vram_used, g.vram_total) {
        (Some(u), Some(t)) if t > 0 => {
            let mut v = loose(colour);
            size_pair(&mut v, u, t);
            out.push(Meter {
                label: "VRAM",
                value: v,
                frac: Some(u as f32 / t as f32),
                role: None,
            });
        }
        (Some(u), _) => {
            let mut v = loose(colour);
            size_item(&mut v, u, "", Role::Plain);
            out.push(Meter {
                label: "VRAM",
                value: v,
                frac: None,
                role: None,
            });
        }
        _ => {}
    }
    out
}

/// The GPU's temperature, fan and power on one line.
fn gpu_facts(g: &GpuSample, cw: usize, colour: bool) -> Option<Line> {
    let mut l = Line::new(cw, colour);
    if let Some(t) = g.temp_c {
        l.measure(&format!("{t:.0}"), " °C", temp_role(t));
    }
    if let Some(rpm) = g.fan_rpm {
        if l.width > 0 {
            l.dot();
        }
        l.measure(&rpm.to_string(), " rpm", Role::Plain);
    }
    if let Some(p) = g.power_w {
        if l.width > 0 {
            l.dot();
        }
        l.measure(&format!("{p:.0}"), " W", Role::Plain);
    }
    (l.width > 0).then_some(l)
}

/// One GPU, named in the border: use, VRAM, its temperature, fan and power, then the graph of
/// its use (the first GPU's; the history follows that one).
fn gpu_box(d: &Data, i: usize, w: usize, rows: usize) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let Some(g) = d.r.gpus.get(i) else {
        return framed(
            w,
            colour,
            top(w, colour, "GPU", &[], None),
            Vec::new(),
            rows,
            bottom(w, colour, None, None),
        );
    };
    let mut body = meters(cw, colour, &gpu_meters(g, "use", colour));
    body.extend(gpu_facts(g, cw, colour));
    let mut shown = 0;
    if i == 0 {
        let gr = rows.saturating_sub(body.len());
        let (graph, n, _) = graph(&d.hist.gpu, gr, cw, Some(100.0), true, colour);
        body.extend(graph);
        shown = n;
    }
    let name = words(colour, &g.name, Role::Plain);
    let foot = bottom(w, colour, None, span(d, shown).as_ref());
    framed(
        w,
        colour,
        top(w, colour, "GPU", &[name], None),
        body,
        rows,
        foot,
    )
}

/// Memory and the GPUs in one box (a narrow screen): memory and its graph, then each GPU.
fn mem_gpu_box(d: &Data, w: usize, rows: usize) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let mut items = mem_meters(d);
    let nm = items.len();
    let mut ends = Vec::new();
    for g in &d.r.gpus {
        items.extend(gpu_meters(g, "GPU", colour));
        ends.push(items.len());
    }
    let mut lines = meters(cw, colour, &items).into_iter();
    let fixed = mem_lines(d) + d.r.gpus.iter().map(gpu_lines).sum::<usize>();
    let rest = rows.saturating_sub(fixed);
    let (gm, gg) = if d.r.gpus.is_empty() {
        (rest, 0)
    } else {
        (rest.div_ceil(2), rest / 2)
    };
    let mut body: Vec<Line> = lines.by_ref().take(nm).collect();
    let (graph_m, shown, _) = graph(&d.hist.mem, gm, cw, Some(100.0), true, colour);
    body.extend(graph_m);
    let mut at = nm;
    for (i, g) in d.r.gpus.iter().enumerate() {
        let end = ends.get(i).copied().unwrap_or(at);
        body.extend(lines.by_ref().take(end - at));
        at = end;
        body.extend(gpu_facts(g, cw, colour));
        if i == 0 {
            body.extend(graph(&d.hist.gpu, gg, cw, Some(100.0), true, colour).0);
        }
    }
    let facts: Vec<Line> =
        d.r.gpus
            .iter()
            .map(|g| words(colour, &g.name, Role::Plain))
            .collect();
    let title = if d.r.gpus.is_empty() {
        "Memory"
    } else {
        "Memory and GPU"
    };
    let foot = bottom(w, colour, None, span(d, shown).as_ref());
    framed(
        w,
        colour,
        top(w, colour, title, &facts, None),
        body,
        rows,
        foot,
    )
}

// Disks and network -------------------------------------------------------------------------

fn disk_need(d: &Data, all: bool) -> usize {
    if all {
        d.disks.len().min(DISK_ROWS)
    } else {
        d.disks.len().min(1)
    }
}

/// The disks in `rows` rows: mount, used of total, a bar and its percent, then `busy` and the
/// rates when there is room; returns the lines and the disks left out.
fn disk_lines(d: &Data, cw: usize, rows: usize) -> (Vec<Line>, usize) {
    let colour = d.colour;
    let list = &d.disks[..d.disks.len().min(rows)];
    if list.is_empty() {
        return (Vec::new(), d.disks.len());
    }
    let values: Vec<Line> = list
        .iter()
        .map(|x| {
            let mut l = loose(colour);
            size_pair(&mut l, x.used, x.total);
            l
        })
        .collect();
    let vw = values.iter().map(|l| l.width).max().unwrap_or(0);
    let longest = list.iter().map(|x| cells(&x.mount)).max().unwrap_or(1);
    let mw = longest
        .min(MOUNT_MAX)
        .min(cw.saturating_sub(vw + 2 + 2 + 5 + BAR_MIN))
        .max(4);
    let busy = list.iter().any(|x| x.busy_pct.is_some());
    let fixed = mw + 2 + vw + 2 + 5;
    // The optional columns give way, rates first, so the bar keeps its least cells.
    let opt = |busy: bool, rates: usize| (if busy { 2 + 9 } else { 0 }) + rates * (2 + RATE_W);
    let (mut busy_on, mut rates) = (busy, 2);
    while cw < fixed + opt(busy_on, rates) + BAR_MIN && (busy_on || rates > 0) {
        if rates > 0 {
            rates -= 1;
        } else {
            busy_on = false;
        }
    }
    let right = opt(busy_on, rates);
    let bw = flex(cw.saturating_sub(fixed + right), cw);
    let mut out = Vec::with_capacity(list.len());
    for (x, value) in list.iter().zip(&values) {
        let mut l = Line::new(cw, colour);
        l.plain(&cut_middle(&x.mount, mw));
        l.pad_to(mw + 2);
        l.join(value);
        l.pad_to(cw.saturating_sub(bw + 5 + right));
        let p = x.used as f32 / x.total.max(1) as f32 * 100.0;
        let role = load_role(p);
        if bw > 0 {
            bar(&mut l, p / 100.0, bw, role);
            l.gap(1);
        }
        pct_cell(&mut l, p, role);
        if busy_on {
            l.gap(2);
            if let Some(b) = x.busy_pct {
                l.push("busy ", Role::Dim);
                l.measure(&format!("{:>3.0}", b.clamp(0.0, 100.0)), "%", Role::Plain);
            } else {
                l.gap(9);
            }
        }
        let shown: Vec<(&str, u64)> = [("r ", x.read_bps), ("w ", x.write_bps)]
            .into_iter()
            .filter_map(|(k, v)| v.filter(|v| *v > 0).map(|v| (k, v)))
            .collect();
        // With room for one rate, the larger one.
        let pick: Vec<(&str, u64)> = if rates >= shown.len() {
            shown
        } else {
            shown
                .into_iter()
                .max_by_key(|(_, v)| *v)
                .into_iter()
                .collect()
        };
        for (k, v) in pick {
            l.gap(2);
            let start = l.width;
            l.push(k, Role::Dim);
            size_item(&mut l, v, "/s", Role::Plain);
            l.pad_to(start + RATE_W);
        }
        out.push(l);
    }
    (out, d.disks.len() - list.len())
}

fn disk_box(d: &Data, w: usize, rows: usize) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let (body, hidden) = disk_lines(d, cw, rows);
    let foot = bottom(w, colour, more(colour, hidden).as_ref(), None);
    framed(
        w,
        colour,
        top(w, colour, "Disks", &[], None),
        body,
        rows,
        foot,
    )
}

fn net_need(d: &Data, all: bool) -> usize {
    match d.nets.len() {
        0 => 0,
        n if all => 1 + (n - 1).min(NET_ROWS),
        _ => 1,
    }
}

/// The network in `rows` rows: the first interface with its address, its down and up rates
/// each with a graph (half the graph rows each), then further interfaces one line each.
/// Returns the lines, the columns the graphs show and the interfaces left out.
fn net_lines(d: &Data, cw: usize, rows: usize, all: bool) -> (Vec<Line>, usize, usize) {
    let colour = d.colour;
    let Some(first) = d.nets.first() else {
        return (Vec::new(), 0, 0);
    };
    let mut out = Vec::with_capacity(rows);
    let mut l = Line::new(cw, colour);
    l.plain(&first.name);
    for a in &first.addrs {
        if l.width + 2 + cells(a) > cw {
            break;
        }
        l.gap(2);
        l.plain(a);
    }
    out.push(l);
    let wanted = if all {
        (d.nets.len() - 1).min(NET_ROWS)
    } else {
        0
    };
    let extras = wanted.min(rows.saturating_sub(3));
    let graph_rows = rows.saturating_sub(1 + extras).max(2);
    let mut shown = 0;
    for (arrow, rate, ring, g) in [
        ("↓ ", first.rx_bps, &d.hist.down, graph_rows.div_ceil(2)),
        ("↑ ", first.tx_bps, &d.hist.up, graph_rows / 2),
    ] {
        let mut label = loose(colour);
        label.push(arrow, Role::Dim);
        rate_text(&mut label, rate);
        // One row each: the rate, the graph, and the peak on the right when the graph keeps
        // its least cells; two rows or more: the rate over the peak, then the graph.
        let one = g == 1;
        let with_peak = !one || cw >= NET_ONE + 2 + PEAK_W + NET_GRAPH_MIN;
        let lw = if one { NET_ONE } else { NET_LABEL };
        let gw = cw.saturating_sub(lw + if one && with_peak { 2 + PEAK_W } else { 0 });
        let (lines, n, peak) = if d.net_hist {
            graph(ring, g, gw, None, false, colour)
        } else {
            (Vec::new(), 0, 0.0)
        };
        shown = shown.max(n);
        let mut peak_l = loose(colour);
        if n > 0 && with_peak {
            peak_l.push("peak ", Role::Dim);
            let (num, unit) = size_parts(peak as u64);
            peak_l.measure(&num, &format!(" {unit}/s"), Role::Dim);
        }
        for k in 0..g {
            let mut l = Line::new(cw, colour);
            match k {
                0 => l.join(&label),
                1 => l.join(&peak_l),
                _ => {}
            }
            l.pad_to(lw);
            if let Some(gl) = lines.get(k) {
                l.join(gl);
            }
            if one {
                l.pad_to(cw.saturating_sub(peak_l.width));
                l.join(&peak_l);
            }
            out.push(l);
        }
    }
    let rest = &d.nets[1..1 + extras];
    let nw = rest
        .iter()
        .map(|n| cells(&n.name))
        .max()
        .unwrap_or(0)
        .min(16);
    let aw = rest
        .iter()
        .map(|n| n.addrs.first().map_or(0, |a| cells(a)))
        .max()
        .unwrap_or(0)
        .min(cw.saturating_sub(nw + 2 + 2 * (RATE_W + 2)));
    for n in rest {
        let mut l = Line::new(cw, colour);
        l.plain(&cut(&n.name, nw));
        l.pad_to(nw + 2);
        if let Some(a) = n.addrs.first() {
            l.plain(&cut(a, aw));
        }
        l.pad_to(nw + 2 + aw);
        for (arrow, rate) in [("↓ ", n.rx_bps), ("↑ ", n.tx_bps)] {
            l.gap(2);
            let start = l.width;
            l.push(arrow, Role::Dim);
            rate_text(&mut l, rate);
            l.pad_to(start + RATE_W);
        }
        out.push(l);
    }
    (out, shown, d.nets.len() - 1 - extras)
}

fn rate_text(l: &mut Line, rate: Option<u64>) {
    match rate {
        Some(x) => size_item(l, x, "/s", Role::Plain),
        None => l.plain("—"),
    }
}

fn net_box(d: &Data, w: usize, rows: usize, all: bool) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let (body, shown, hidden) = net_lines(d, cw, rows, all);
    let foot = bottom(
        w,
        colour,
        more(colour, hidden).as_ref(),
        span(d, shown).as_ref(),
    );
    framed(
        w,
        colour,
        top(w, colour, "Network", &[], None),
        body,
        rows,
        foot,
    )
}

/// Disks and network in one box (a narrow screen).
fn disk_net_box(d: &Data, w: usize, rows: usize, all: bool) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let dr = disk_need(d, all).min(rows);
    let (mut body, hidden_d) = disk_lines(d, cw, dr);
    let (net, shown, hidden_n) = net_lines(d, cw, rows.saturating_sub(dr), all);
    body.extend(net);
    let foot = bottom(
        w,
        colour,
        more(colour, hidden_d + hidden_n).as_ref(),
        span(d, shown).as_ref(),
    );
    framed(
        w,
        colour,
        top(w, colour, "Disks and network", &[], None),
        body,
        rows,
        foot,
    )
}

// Sensors --------------------------------------------------------------------------------------

/// A sensor's label, value and unit, and the value's colour.
fn sensor_pair(s: &Sensor) -> (String, String, &'static str, Role) {
    let (unit, role) = match s.kind {
        SensorKind::Temp => (" °C", temp_role(s.value)),
        SensorKind::Fan => (" rpm", Role::Plain),
        SensorKind::Power => (" W", Role::Plain),
    };
    (
        format!("{} {}", s.chip, s.label),
        format!("{:.0}", s.value),
        unit,
        role,
    )
}

fn sensor_per_row(pw: usize, cw: usize) -> usize {
    ((cw + 3) / (pw.min(cw) + 3)).max(1)
}

fn sensor_need(d: &Data, cw: usize) -> usize {
    d.sensors.len().div_ceil(sensor_per_row(d.sensor_w, cw))
}

/// The sensors in `rows` rows, in as many columns as fit, down each column first; returns the
/// lines and the sensors left out.
fn sensor_lines(d: &Data, cw: usize, rows: usize) -> (Vec<Line>, usize) {
    let n = d.sensors.len();
    let pw = d.sensor_w.min(cw);
    let per = sensor_per_row(pw, cw);
    let used = n.div_ceil(per).min(rows);
    let shown = (used * per).min(n);
    let mut out = Vec::with_capacity(used);
    for r in 0..used {
        let mut l = Line::new(cw, d.colour);
        for j in 0..per {
            let i = j * used + r;
            let Some(s) = d.sensors.get(i).filter(|_| i < shown) else {
                break;
            };
            let x = j * (cw + 3) / per;
            let (name, num, unit, role) = sensor_pair(s);
            let vw = cells(&num) + cells(unit);
            l.pad_to(x);
            l.plain(&cut(&name, pw.saturating_sub(vw + 2)));
            l.pad_to(x + pw - vw);
            l.measure(&num, unit, role);
        }
        out.push(l);
    }
    (out, n - shown)
}

fn sensor_box(d: &Data, w: usize, rows: usize) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let (body, hidden) = sensor_lines(d, cw, rows);
    let mut left = loose(colour);
    if let Some(m) = more(colour, hidden) {
        left.join(&m);
        left.dot();
        key_item(&mut left, "s all");
    }
    let foot = bottom(w, colour, Some(&left), None);
    framed(
        w,
        colour,
        top(w, colour, "Sensors", &[], None),
        body,
        rows,
        foot,
    )
}

/// Every sensor, in place of the process table.
fn sensors_page(d: &Data, w: usize, h: usize) -> Vec<Line> {
    let (cw, colour) = (w.saturating_sub(4), d.colour);
    let rows = h.saturating_sub(2);
    let (mut body, hidden) = sensor_lines(d, cw, rows);
    if d.sensors.is_empty() {
        body.push(words(colour, "No sensors.", Role::Dim));
    }
    let count = words(colour, &format!("{}", d.sensors.len()), Role::Plain);
    let back = key_items(&["s back", "esc back"], colour);
    let foot = bottom(w, colour, Some(&back), more(colour, hidden).as_ref());
    fit_box(
        framed(
            w,
            colour,
            top(w, colour, "Sensors", &[count], None),
            body,
            rows,
            foot,
        ),
        h,
    )
}

fn kind_rank(kind: SensorKind) -> u8 {
    match kind {
        SensorKind::Temp => 0,
        SensorKind::Fan => 1,
        SensorKind::Power => 2,
    }
}

/// A box drawn for a room too short for its borders keeps only the lines that fit.
fn fit_box(mut lines: Vec<Line>, h: usize) -> Vec<Line> {
    lines.truncate(h);
    lines
}

// The processes --------------------------------------------------------------------------------

/// The process box, down to the last row: the order, the filter and the counts in the top
/// border, the header, the rows, and the keys (or the filter being typed) in the bottom border.
fn proc_box(
    d: &Data,
    v: &View,
    s: &mut Screen,
    tree: &mut Tree,
    rank: &dyn Fn(&Proc) -> f64,
    h: usize,
    ask: Option<&str>,
) -> Vec<Line> {
    let (w, colour) = (s.cols, d.colour);
    let grouped = !v.flat && !v.tree;
    let (rows, marks) = text::process_rows(&d.r.procs, v, rank, tree);
    let mut facts = Vec::new();
    let mut by = loose(colour);
    by.push("by ", Role::Dim);
    by.plain(text::sort_word(v.by));
    by.plain(" ");
    by.push(if v.reverse { "▴" } else { "▾" }, Role::Accent);
    facts.push(by);
    if v.steady && v.by.smooths() {
        facts.push(words(colour, "steady", Role::Plain));
    }
    if let Some(f) = &v.filter {
        let mut l = loose(colour);
        l.push("filter ", Role::Dim);
        l.plain(f);
        facts.push(l);
    }
    let count = if grouped {
        format!("{} in {} apps", d.r.procs.len(), rows.len())
    } else {
        format!("{} processes", d.r.procs.len())
    };
    let head = top(
        w,
        colour,
        "Processes",
        &facts,
        Some(&words(colour, &count, Role::Plain)),
    );
    let foot = match ask {
        Some(q) => words(colour, q, Role::Warn),
        None => key_line(s, w.saturating_sub(6), colour),
    };
    let foot = bottom(w, colour, Some(&foot), None);
    let inner = w.saturating_sub(2);
    let c = proc_columns(inner, w >= 120, !grouped, v.by, grouped);
    let mut body = Vec::with_capacity(h);
    body.push(c.header(inner, colour, v.by, grouped));
    let room = h.saturating_sub(3);
    if let Some(i) = s.selected {
        if room > 0 {
            if i < s.scroll {
                s.scroll = i;
            } else if i >= s.scroll + room {
                s.scroll = i + 1 - room;
            }
        }
    }
    let uptime = d.r.host.uptime_s;
    for (i, row) in rows.iter().enumerate().skip(s.scroll).take(room) {
        let mut l = Line::new(inner, colour);
        let sel = s.selected == Some(i);
        l.rev = sel && colour;
        l.plain(if sel { "▸" } else { " " });
        let mark = marks.get(i).map_or("", String::as_str);
        c.row(&mut l, row, mark, grouped, &s.users, uptime);
        l.pad_to(inner);
        body.push(l);
    }
    let mut out = Vec::with_capacity(h);
    out.push(head);
    for i in 0..h.saturating_sub(2) {
        let mut l = Line::new(w, colour);
        l.push("│", Role::Dim);
        if let Some(b) = body.get(i) {
            l.join(b);
        }
        l.pad_to(w.saturating_sub(1));
        l.push("│", Role::Dim);
        out.push(l);
    }
    out.push(foot);
    fit_box(out, h)
}

/// The keys for the process box's bottom border, as many as fit in `room`, `? help` always;
/// the filter being typed in their place; without keys, how to leave.
fn key_line(s: &Screen, room: usize, colour: bool) -> Line {
    let mut l = loose(colour);
    if let Some(p) = s.prompt.as_deref().filter(|p| p.starts_with("/ ")) {
        l.plain(p);
        l.push("▏", Role::Accent);
        l.dot();
        l.join(&key_items(&["enter keep", "esc clear"], colour));
        return l;
    }
    if !s.keys {
        l.push(LEAVE, Role::Dim);
        return l;
    }
    let help = cells(HELP_KEY);
    for k in KEYS {
        if l.width + cells(k) + 3 + help + 3 > room {
            break;
        }
        if l.width > 0 {
            l.dot();
        }
        key_item(&mut l, k);
    }
    if l.width > 0 {
        l.dot();
    }
    key_item(&mut l, HELP_KEY);
    l
}

/// The table's columns shown, by `PROC_COLS`, and the name's cells.
struct Columns {
    on: [bool; 9],
    /// Cells added to every column shown, so a wide screen spreads the table out.
    extra: usize,
    name_w: usize,
}

/// The column a sort key is shown in.
fn sort_col(by: Sort) -> Option<usize> {
    match by {
        Sort::Cpu => Some(0),
        Sort::Mem => Some(1),
        Sort::Gpu => Some(2),
        Sort::Disk => Some(4),
        Sort::Threads => Some(5),
        Sort::Age => Some(6),
        Sort::Pid => Some(7),
        Sort::User => Some(8),
        Sort::Name => None,
    }
}

/// The columns for an inner width: THR and AGE from 120 columns, PID and USER for single
/// processes; they give way (USER, PID, AGE, THR) so the name keeps 16 cells, then DISK, VRAM
/// and GPU so it keeps 12. The sorted column stays.
fn proc_columns(inner: usize, wide: bool, single: bool, by: Sort, _grouped: bool) -> Columns {
    let mut on = [true, true, true, true, true, wide, wide, single, single];
    let fixed = |on: &[bool; 9]| {
        2 + PROC_COLS
            .iter()
            .zip(on)
            .filter(|(_, o)| **o)
            .map(|((_, w), _)| w + 2)
            .sum::<usize>()
    };
    let keep = sort_col(by);
    for (i, least) in [
        (8, NAME_MIN),
        (7, NAME_MIN),
        (6, NAME_MIN),
        (5, NAME_MIN),
        (4, NAME_LEAST),
        (3, NAME_LEAST),
        (2, NAME_LEAST),
    ] {
        if inner < fixed(&on) + least && keep != Some(i) {
            on[i] = false;
        }
    }
    // Each number keeps its width and two spaces; the name takes all the rest.
    Columns {
        name_w: inner.saturating_sub(fixed(&on)),
        extra: 0,
        on,
    }
}

impl Columns {
    /// The header: the labels dim, the sorted one in accent.
    fn header(&self, inner: usize, colour: bool, by: Sort, grouped: bool) -> Line {
        let mut l = Line::new(inner, colour);
        l.gap(1);
        let sorted = sort_col(by);
        for (i, (label, w)) in PROC_COLS.iter().enumerate() {
            if self.on[i] {
                let role = if sorted == Some(i) {
                    Role::Accent
                } else {
                    Role::Dim
                };
                cell(&mut l, label, role, *w + self.extra);
            }
        }
        let name = if grouped { "APP" } else { "NAME" };
        let role = if by == Sort::Name {
            Role::Accent
        } else {
            Role::Dim
        };
        l.push(&cut(name, self.name_w), role);
        l
    }

    fn row(
        &self,
        l: &mut Line,
        r: &Row,
        mark: &str,
        grouped: bool,
        users: &HashMap<u32, String>,
        uptime: u64,
    ) {
        // A value that is exactly zero is drawn dim; a missing one is "—".
        let disk = match (r.read, r.write) {
            (None, None) => None,
            (a, b) => {
                let total = a.unwrap_or(0).saturating_add(b.unwrap_or(0));
                Some((size_text(total, "/s"), total == 0))
            }
        };
        let values = [
            r.cpu.map(|x| (proc_pct(x), x == 0.0)),
            Some((size_text(r.mem, ""), r.mem == 0)),
            r.gpu.map(|x| (format!("{x:.0}%"), x == 0.0)),
            r.vram.map(|x| (size_text(x, ""), x == 0)),
            disk,
            Some((r.threads.to_string(), false)),
            Some((age_text(uptime.saturating_sub(r.started)), false)),
            r.pid.map(|p| (p.to_string(), false)),
            Some((user_name(r.uid, users), false)),
        ];
        for (i, value) in values.into_iter().enumerate() {
            if !self.on[i] {
                continue;
            }
            let w = PROC_COLS[i].1 + self.extra;
            match value {
                Some((text, zero)) => cell(l, &text, if zero { Role::Dim } else { Role::Plain }, w),
                None => cell(l, "—", Role::Plain, w),
            }
        }
        let mut name = format!("{mark}{}", r.name);
        if grouped && r.count > 1 {
            name.push_str(&format!(" ×{}", r.count));
        }
        l.plain(&cut(&name, self.name_w));
    }
}

/// The selected process in full, in a box in place of the table.
fn detail_box(
    d: &Data,
    v: &View,
    s: &Screen,
    tree: &mut Tree,
    rank: &dyn Fn(&Proc) -> f64,
    h: usize,
) -> Vec<Line> {
    let (w, colour) = (s.cols, d.colour);
    let cw = w.saturating_sub(4);
    let (rows, _) = text::process_rows(&d.r.procs, v, rank, tree);
    let row = s.selected.and_then(|i| rows.get(i));
    let mut facts = Vec::new();
    let mut body = Vec::new();
    if let Some(row) = row {
        facts.push(words(colour, &row.name, Role::Plain));
        body = detail_lines(row, cw, colour, &s.users, d.r.host.uptime_s);
    }
    let back = key_items(&["enter back", "esc back"], colour);
    let foot = bottom(w, colour, Some(&back), None);
    fit_box(
        framed(
            w,
            colour,
            top(w, colour, "Process", &facts, None),
            body,
            h.saturating_sub(2),
            foot,
        ),
        h,
    )
}

fn detail_lines(
    row: &Row,
    cw: usize,
    colour: bool,
    users: &HashMap<u32, String>,
    uptime: u64,
) -> Vec<Line> {
    let pair = |l: &mut Line, label: &str, value: &str| {
        if l.width > 0 {
            l.gap(3);
        }
        l.push(label, Role::Dim);
        l.push(" ", Role::Dim);
        l.plain(value);
    };
    let dash = || "—".to_string();
    let mut ids = Line::new(cw, colour);
    pair(
        &mut ids,
        "pid",
        &row.pid.map_or_else(dash, |p| p.to_string()),
    );
    pair(&mut ids, "user", &user_name(row.uid, users));
    pair(&mut ids, "threads", &row.threads.to_string());
    pair(&mut ids, "processes", &row.count.to_string());
    pair(
        &mut ids,
        "age",
        &age_text(uptime.saturating_sub(row.started)),
    );
    let mut nums = Line::new(cw, colour);
    pair(&mut nums, "cpu", &row.cpu.map_or_else(dash, proc_pct));
    pair(&mut nums, "mem", &size_text(row.mem, ""));
    pair(
        &mut nums,
        "gpu",
        &row.gpu.map_or_else(dash, |x| format!("{x:.0}%")),
    );
    pair(
        &mut nums,
        "vram",
        &row.vram.map_or_else(dash, |x| size_text(x, "")),
    );
    let mut io = Line::new(cw, colour);
    pair(
        &mut io,
        "read",
        &row.read.map_or_else(dash, |x| size_text(x, "/s")),
    );
    pair(
        &mut io,
        "write",
        &row.write.map_or_else(dash, |x| size_text(x, "/s")),
    );
    let mut name = Line::new(cw, colour);
    pair(&mut name, "name", &row.name);
    vec![ids, nums, io, name]
}

/// The keys, one per line, in a box in place of the table.
fn help_box(w: usize, h: usize, colour: bool) -> Vec<Line> {
    let cw = w.saturating_sub(4);
    let body = HELP
        .iter()
        .map(|(key, what)| {
            let mut l = Line::new(cw, colour);
            l.plain(key);
            l.pad_to(18);
            l.push(what, Role::Dim);
            l
        })
        .collect();
    let back = key_items(&["? back", "esc back"], colour);
    let foot = bottom(w, colour, Some(&back), None);
    fit_box(
        framed(
            w,
            colour,
            top(w, colour, "Keys", &[], None),
            body,
            h.saturating_sub(2),
            foot,
        ),
        h,
    )
}

/// The question before a process is ended, in a box under the processes.
fn question_box(q: &str, w: usize, colour: bool) -> Vec<Line> {
    let mut l = Line::new(w.saturating_sub(4), colour);
    l.push(q, Role::Warn);
    let keys = key_items(&["y yes", "n no"], colour);
    let foot = bottom(w, colour, Some(&keys), None);
    framed(
        w,
        colour,
        top(w, colour, "End process", &[], None),
        vec![l],
        1,
        foot,
    )
}

// Graphs, bars and numbers ---------------------------------------------------------------------

/// A graph `rows` tall and `width` wide of a ring's newest values, one column a reading, newest
/// on the right; columns not yet read are blank. A value is its share of `top` times the rows
/// times eight eighths, stacked from the bottom row; every reading shows one eighth at least.
/// `top` is the scale's top (100 for a percentage), else the highest value shown. A column is
/// coloured by its level (`by_level`), else in the accent. Returns the lines, the columns shown
/// and the highest value shown.
fn graph(
    ring: &Ring,
    rows: usize,
    width: usize,
    top: Option<f32>,
    by_level: bool,
    colour: bool,
) -> (Vec<Line>, usize, f32) {
    if rows == 0 || width == 0 {
        return (Vec::new(), 0, 0.0);
    }
    let shown = ring.len().min(width);
    let peak = ring.newest(shown).flatten().fold(0.0_f32, f32::max);
    let top = top.unwrap_or(peak);
    let full = rows * 8;
    let cols: Vec<(usize, Role)> = ring
        .newest(shown)
        .map(|v| match v {
            Some(x) => {
                let e = if top > 0.0 {
                    ((x / top).clamp(0.0, 1.0) * full as f32).round() as usize
                } else {
                    0
                };
                let role = if by_level { load_role(x) } else { Role::Accent };
                (e.clamp(1, full), role)
            }
            None => (0, Role::Plain),
        })
        .collect();
    let mut out = Vec::with_capacity(rows);
    let mut run = String::new();
    for k in 0..rows {
        let base = (rows - 1 - k) * 8;
        let mut l = Line::new(width, colour);
        l.gap(width - shown);
        let mut role_now = Role::Plain;
        for &(e, role) in &cols {
            let (ch, role) = if e >= base + 8 {
                ("█", role)
            } else if e > base {
                (LEVELS[e - base - 1], role)
            } else {
                (" ", Role::Plain)
            };
            if role != role_now && !run.is_empty() {
                l.push(&run, role_now);
                run.clear();
            }
            role_now = role;
            run.push_str(ch);
        }
        if !run.is_empty() {
            l.push(&run, role_now);
            run.clear();
        }
        out.push(l);
    }
    (out, shown, peak)
}

/// A bar's cells for the room its line leaves: none under the least, else at most the share.
fn flex(room: usize, cw: usize) -> usize {
    if room < BAR_MIN {
        0
    } else {
        room.min((cw * BAR_SHARE / 100).max(BAR_MIN))
    }
}

/// A bar of `cells` cells: `━` filled, a `╸` half cell from half a cell up, the rest `╌` dim.
fn bar(l: &mut Line, frac: f32, cells: usize, role: Role) {
    if cells == 0 {
        return;
    }
    let fill = frac.clamp(0.0, 1.0) * cells as f32;
    let full = (fill.floor() as usize).min(cells);
    l.push(&"━".repeat(full), role);
    let mut used = full;
    if fill - fill.floor() >= 0.5 && full < cells {
        l.push("╸", role);
        used += 1;
    }
    l.push(&"╌".repeat(cells - used), Role::Dim);
}

/// A whole percentage in four cells, right-aligned: the number in `role`, the sign dim.
fn pct_cell(l: &mut Line, p: f32, role: Role) {
    l.measure(&format!("{:>3.0}", p.clamp(0.0, 999.0)), "%", role);
}

fn load_role(pct: f32) -> Role {
    if pct >= 90.0 {
        Role::Critical
    } else if pct >= 70.0 {
        Role::Warn
    } else {
        Role::Accent
    }
}

fn temp_role(t: f32) -> Role {
    if t >= 95.0 {
        Role::Critical
    } else if t >= 80.0 {
        Role::Warn
    } else {
        Role::Plain
    }
}

/// A number of up to three significant digits with no trailing ".0": 9.8, 15.5, 212, 1.1.
fn sig3(v: f64) -> String {
    if v >= 99.95 {
        return format!("{v:.0}");
    }
    let s = format!("{v:.1}");
    match s.strip_suffix(".0") {
        Some(whole) => whole.to_string(),
        None => s,
    }
}

/// A byte count as its number and its unit: `9.8` and `GiB`.
fn size_parts(n: u64) -> (String, &'static str) {
    if n < 1024 {
        return (n.to_string(), UNITS[0]);
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    (sig3(v), UNITS[i])
}

/// A size or a rate as one item: `400 MiB`, `3.2 MiB/s` (`suffix` is `/s` for a rate).
fn size_item(l: &mut Line, n: u64, suffix: &str, role: Role) {
    let (num, unit) = size_parts(n);
    l.push(&num, role);
    l.push(" ", Role::Dim);
    l.push(unit, Role::Dim);
    l.push(suffix, Role::Dim);
}

/// Two sizes: a pair with one unit names it once (`9.8 of 15.5 GiB`), else both (`400 MiB of 16 GiB`).
fn size_pair(l: &mut Line, a: u64, b: u64) {
    let (na, ua) = size_parts(a);
    let (nb, ub) = size_parts(b);
    l.push(&na, Role::Plain);
    if ua != ub {
        l.push(" ", Role::Dim);
        l.push(ua, Role::Dim);
    }
    l.push(" of ", Role::Dim);
    l.push(&nb, Role::Plain);
    l.push(" ", Role::Dim);
    l.push(ub, Role::Dim);
}

/// A percentage in the process table: one decimal under 10, whole from 10.
fn proc_pct(x: f32) -> String {
    if x < 9.95 {
        format!("{x:.1}%")
    } else {
        format!("{x:.0}%")
    }
}

/// Seconds as "up 3d 04:12", or "up 04:12" under a day.
fn uptime_short(s: u64) -> String {
    let d = s / 86_400;
    let h = (s % 86_400) / 3_600;
    let m = (s % 3_600) / 60;
    if d > 0 {
        format!("up {d}d {h:02}:{m:02}")
    } else {
        format!("up {h:02}:{m:02}")
    }
}

/// A process's age in its largest whole unit: `45s`, `12m`, `3h`, `2d`.
fn age_text(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3_600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3_600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// The user's name for a uid; a uid with no name is shown as the number.
fn user_name(uid: u32, users: &HashMap<u32, String>) -> String {
    users.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
}

/// A size or rate as text, for a table cell: `2.3 GiB`, `0 B`, `3.2 MiB/s`.
fn size_text(n: u64, suffix: &str) -> String {
    if n < 1024 {
        return format!("{n} B{suffix}");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    // One decimal under 10 (3.0 GiB beside 3.4 GiB), whole from 10 (12 GiB, 400 MiB).
    let num = if v < 9.95 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    };
    format!("{num} {}{suffix}", UNITS[i])
}

/// One table cell: the text cut to its width, right-aligned, then two spaces.
fn cell(l: &mut Line, text: &str, role: Role, w: usize) {
    let text = cut(text, w);
    l.gap(w.saturating_sub(cells(&text)));
    l.push(&text, role);
    l.gap(2);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cli::parse;
    use aether::sys::{HostSample, NetSample};

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn view() -> View {
        parse(&args("")).unwrap().view
    }

    fn proc(pid: u32, name: &str, cpu: f32, mem: u64) -> Proc {
        Proc {
            pid,
            ppid: 1,
            uid: 1000,
            threads: 4,
            name: name.to_string(),
            state: 0,
            cpu_pct: Some(cpu),
            gpu_pct: None,
            mem,
            rss: mem,
            started_s: u64::from(pid) * 60,
            vram: None,
            read_bps: None,
            write_bps: None,
        }
    }

    /// A machine with invented values: one CPU, memory, one disk, one interface, a few apps.
    fn sample(threads: usize, procs: Vec<Proc>) -> Reading {
        let mut r = Reading {
            host: HostSample {
                uptime_s: 3 * 86_400 + 4 * 3_600 + 12 * 60,
                threads: threads as u32,
            },
            ..Reading::default()
        };
        r.cpu.pct = Some(23.4);
        r.cpu.mhz = Some(3_900);
        r.cpu.temp_c = Some(52.0);
        r.cpu.power_w = Some(38.0);
        r.cpu.load = Some([1.2, 0.9, 0.8]);
        r.cpu.threads = (0..threads).map(|i| (i * 7 % 100) as f32).collect();
        r.mem.total = 16 * 1024 * 1024 * 1024;
        r.mem.used = 10 * 1024 * 1024 * 1024;
        r.mem.available = 6 * 1024 * 1024 * 1024;
        r.mem.cached = 3 * 1024 * 1024 * 1024;
        r.disks.push(DiskSample {
            mount: "/".to_string(),
            dev: "/dev/sda1".to_string(),
            total: 465 * 1024 * 1024 * 1024,
            used: 212 * 1024 * 1024 * 1024,
            read_bps: Some(0),
            write_bps: Some(0),
            busy_pct: Some(3.0),
        });
        r.nets.push(NetSample {
            name: "eth0".to_string(),
            rx_bps: Some(3_400_000),
            tx_bps: Some(120_000),
            link_bps: None,
            addrs: vec!["192.0.2.10".to_string()],
        });
        r.procs = procs;
        r
    }

    fn screen(cols: usize, rows: usize, colour: bool) -> Screen {
        Screen {
            cols,
            rows,
            colour,
            selected: Some(0),
            scroll: 0,
            interval_s: 1.0,
            paused: false,
            overlay: Overlay::Table,
            keys: true,
            prompt: None,
            users: Rc::default(),
        }
    }

    /// A frame with a history of the one reading.
    fn frame(r: &Reading, v: &View, s: &Screen) -> String {
        let mut hist = History::default();
        hist.push(r);
        frame_with(r, v, s, &hist)
    }

    fn frame_with(r: &Reading, v: &View, s: &Screen, hist: &History) -> String {
        let mut s = s.clone();
        let mut tree = Tree::new();
        let rank = |p: &Proc| f64::from(p.cpu_pct.unwrap_or(-1.0));
        let mut out = Vec::new();
        render(&mut out, r, v, &mut s, hist, &mut tree, &rank);
        String::from_utf8(out).unwrap()
    }

    /// The text lines of a frame, without the marks and erase codes.
    fn lines_of(frame: &str) -> Vec<String> {
        let mut s = String::new();
        let mut chars = frame.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // A CSI sequence ends at its first letter.
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                s.push(c);
            }
        }
        s.split("\r\n").map(str::to_string).collect()
    }

    /// Asserts a frame is the screen: exactly `rows` lines, none wider than `cols` cells.
    fn assert_fits(f: &str, cols: usize, rows: usize, what: &str) {
        let lines = lines_of(f);
        assert_eq!(
            lines.len(),
            rows,
            "{what} {cols}x{rows}: {} lines",
            lines.len()
        );
        for l in &lines {
            assert!(
                cells(l) <= cols,
                "{what} {cols}x{rows}: {} cells in {l:?}",
                cells(l)
            );
        }
    }

    /// The invented machine of the pictures and the expected frames: twelve threads, a GPU,
    /// two disks, two interfaces, six sensors, 26 processes in 22 apps, and four minutes of
    /// history with the shape of ordinary use. The rows add up to about the machine's 23 %, the
    /// GPU rows to its 41 %, the VRAM rows to its 2.1 GiB.
    fn review() -> (Reading, History, Rc<HashMap<u32, String>>) {
        let mb = |m: u64| m * 1024 * 1024;
        // Name, pid, CPU %, memory in MiB, GPU %, VRAM in MiB, bytes written a second.
        type Spec = (
            &'static str,
            u32,
            f32,
            u64,
            Option<f32>,
            Option<u64>,
            Option<u64>,
        );
        let spec: &[Spec] = &[
            ("game", 301, 6.2, 1600, Some(20.0), Some(1000), None),
            ("game", 302, 2.1, 500, Some(8.0), Some(300), None),
            ("game", 303, 0.8, 255, Some(3.0), Some(134), None),
            ("browser", 410, 2.6, 2100, Some(6.0), Some(307), None),
            ("browser", 411, 1.4, 1000, None, None, None),
            ("editor", 330, 2.3, 410, None, None, None),
            ("sync", 610, 1.2, 96, None, None, Some(1_048_576)),
            ("chat", 520, 0.9, 900, None, None, None),
            ("terminal", 700, 0.7, 60, None, None, None),
            ("terminal", 701, 0.4, 80, None, None, None),
            ("music", 800, 1.0, 210, None, None, None),
            ("files", 950, 0.5, 64, None, None, None),
            ("mail", 900, 0.4, 180, None, None, None),
            ("notes", 960, 0.4, 38, None, None, None),
            ("shell", 970, 0.1, 12, None, None, None),
            ("compositor", 1100, 0.6, 180, Some(4.0), Some(409), None),
            ("audio", 1210, 0.3, 24, None, None, None),
            ("indexer", 1320, 0.2, 140, None, None, Some(204_800)),
            ("updates", 1400, 0.1, 90, None, None, None),
            ("session", 1830, 0.1, 30, None, None, None),
            ("network", 2050, 0.1, 18, None, None, None),
            ("keyring", 1500, 0.0, 9, None, None, None),
            ("bluetooth", 1610, 0.0, 6, None, None, None),
            ("printer", 1720, 0.0, 14, None, None, None),
            ("clock", 1940, 0.0, 4, None, None, None),
            ("spell", 2160, 0.0, 22, None, None, None),
        ];
        let mut procs = Vec::new();
        for (k, &(name, pid, cpu, mem, gpu, vram, write)) in spec.iter().enumerate() {
            let mut p = proc(pid, name, cpu, mb(mem));
            p.gpu_pct = gpu;
            p.vram = vram.map(mb);
            p.write_bps = write;
            p.threads = [
                41, 12, 6, 38, 22, 17, 9, 24, 3, 3, 11, 5, 8, 4, 1, 14, 6, 4, 3, 5, 4, 4, 3, 2, 2,
                2,
            ][k];
            p.started_s = 3 * 86_400 + 4 * 3_600
                - [
                    7_200, 7_100, 7_000, 30_000, 29_000, 3_000, 250_000, 80_000, 600, 400, 12_000,
                    1_500, 90_000, 45_000, 50, 270_000, 265_000, 9_000, 200_000, 269_000, 268_200,
                    268_000, 268_500, 150_000, 100_000, 5_000,
                ][k];
            procs.push(p);
        }
        let mut r = sample(12, procs);
        r.cpu.threads = vec![
            41.0, 18.0, 9.0, 33.0, 4.0, 27.0, 12.0, 6.0, 52.0, 3.0, 38.0, 33.0,
        ];
        r.cpu.threads_mhz = vec![
            4_210, 3_880, 3_650, 4_020, 3_400, 3_950, 3_700, 3_500, 4_380, 3_400, 4_100, 4_050,
        ];
        r.mem.swap_total = 8 * 1024 * 1024 * 1024;
        r.mem.swap_used = 200 * 1024 * 1024;
        r.gpus.push(GpuSample {
            name: "Example 580".to_string(),
            vendor: "example",
            pct: Some(41.0),
            temp_c: Some(67.0),
            power_w: Some(96.0),
            vram_used: Some(2_254_857_830),
            vram_total: Some(4_294_967_296),
            fan_rpm: Some(1180),
        });
        for (chip, label, kind, value) in [
            ("cpu", "package", SensorKind::Temp, 61.0),
            ("gpu", "edge", SensorKind::Temp, 67.0),
            ("nvme", "composite", SensorKind::Temp, 38.5),
            ("board", "chassis", SensorKind::Fan, 820.0),
            ("gpu", "fan", SensorKind::Fan, 1180.0),
            ("psu", "power", SensorKind::Power, 42.0),
        ] {
            r.sensors.push(Sensor {
                chip: chip.to_string(),
                label: label.to_string(),
                kind,
                value,
            });
        }
        r.disks.push(DiskSample {
            mount: "/data".to_string(),
            dev: "/dev/sdb1".to_string(),
            total: 1_980_000_000_000,
            used: 1_200_000_000_000,
            read_bps: Some(0),
            write_bps: Some(mb(12)),
            busy_pct: Some(34.0),
        });
        r.nets.push(NetSample {
            name: "wlan0".to_string(),
            rx_bps: Some(1_200),
            tx_bps: Some(0),
            link_bps: Some(866_000_000),
            addrs: vec!["198.51.100.7".to_string()],
        });
        // Ordinary use: quiet most of the time, a few bursts, a settle, and now.
        let mut hist = History::default();
        let (cpu, gpu, used) = (r.cpu.pct, r.gpus[0].pct, r.mem.used);
        for i in 0..240 {
            let t = i as f32;
            let burst = |every: usize, len: usize| i % every < len;
            let c = 17.0
                + 7.0 * (t / 7.0).sin()
                + 5.0 * (t / 2.3).sin()
                + if burst(53, 5) { 34.0 } else { 0.0 };
            r.cpu.pct = Some(c.clamp(3.0, 97.0));
            let g = 33.0 + 9.0 * (t / 11.0).sin() + if burst(41, 6) { 22.0 } else { 0.0 };
            r.gpus[0].pct = Some(g.clamp(0.0, 100.0));
            let m = 0.60 + 0.02 * (t / 40.0).sin() + 0.004 * (t / 3.0).sin();
            r.mem.used = (r.mem.total as f32 * m) as u64;
            let down = 1_400_000.0 * (1.0 + (t / 5.0).sin())
                + if burst(37, 3) { 9_000_000.0 } else { 0.0 };
            r.nets[0].rx_bps = Some(down as u64);
            r.nets[0].tx_bps = Some((90_000.0 * (1.2 + (t / 9.0).sin())) as u64);
            hist.push(&r);
        }
        r.cpu.pct = cpu;
        r.gpus[0].pct = gpu;
        r.mem.used = used;
        r.nets[0].rx_bps = Some(2_900_000);
        r.nets[0].tx_bps = Some(117_000);
        hist.push(&r);
        let users = Rc::new(
            [(1000, "dana".to_string()), (0, "root".to_string())]
                .into_iter()
                .collect::<HashMap<u32, String>>(),
        );
        (r, hist, users)
    }

    fn review_screen(
        cols: usize,
        rows: usize,
        colour: bool,
        users: &Rc<HashMap<u32, String>>,
    ) -> Screen {
        Screen {
            selected: Some(2),
            users: Rc::clone(users),
            ..screen(cols, rows, colour)
        }
    }

    #[test]
    fn the_whole_frame_matches_the_expected_text() {
        let (r, hist, users) = review();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/view");
        let bless = std::env::var_os("AETHER_BLESS").is_some();
        for (cols, rows) in [(200, 50), (140, 40), (120, 40), (80, 24)] {
            let s = review_screen(cols, rows, false, &users);
            let text = lines_of(&frame_with(&r, &view(), &s, &hist)).join("\n") + "\n";
            let path = dir.join(format!("{cols}x{rows}.txt"));
            if bless {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&path, &text).unwrap();
                continue;
            }
            let want = std::fs::read_to_string(&path).unwrap();
            for (i, (got, want)) in text.lines().zip(want.lines()).enumerate() {
                assert_eq!(got, want, "{cols}x{rows}, line {i}");
            }
            assert_eq!(text, want, "{cols}x{rows}");
        }
    }

    #[test]
    fn every_frame_is_the_screen_at_every_size() {
        let (review, hist, users) = review();
        let long = vec![
            proc(10, &"a-very-long-application-name-".repeat(4), 9.1, 2 << 30),
            proc(11, "browser", 4.0, 3 << 30),
        ];
        let mut threads = sample(200, long);
        threads.disks[0].mount =
            "/run/media/someone/a-disk-with-a-very-long-label/Data".to_string();
        let mut bare = sample(4, Vec::new());
        bare.disks.clear();
        bare.nets.clear();
        let cases = [("review", &review), ("threads", &threads), ("bare", &bare)];
        for cols in [60, 80, 99, 100, 120, 139, 140, 200, 300] {
            for rows in [15, 24, 30, 40, 50, 80] {
                for (what, r) in cases {
                    let s = review_screen(cols, rows, false, &users);
                    let f = frame_with(r, &view(), &s, &hist);
                    assert_fits(&f, cols, rows, what);
                    let lines = lines_of(&f);
                    assert!(lines[0].starts_with(" aether"), "{what} {cols}x{rows}");
                    // The process box keeps its least rows: its header and the rows under it.
                    let least = if rows < 30 {
                        PROC_ROWS_SHORT
                    } else {
                        PROC_ROWS
                    };
                    let header = lines
                        .iter()
                        .position(|l| l.contains("CPU") && l.contains("MEM"));
                    let header =
                        header.unwrap_or_else(|| panic!("{what} {cols}x{rows}: no header"));
                    assert!(rows - header - 2 >= least, "{what} {cols}x{rows}: {header}");
                }
            }
        }
        // The overlays and the question keep to the screen too.
        for overlay in [Overlay::Help, Overlay::Sensors, Overlay::Detail] {
            for (cols, rows) in [(60, 15), (80, 24), (200, 50)] {
                let mut s = review_screen(cols, rows, true, &users);
                s.overlay = overlay;
                assert_fits(
                    &frame_with(&review, &view(), &s, &hist),
                    cols,
                    rows,
                    "overlay",
                );
            }
        }
        let mut s = review_screen(60, 15, true, &users);
        s.prompt = Some("end game (pid 301)? y or n".to_string());
        let f = lines_of(&frame_with(&review, &view(), &s, &hist));
        assert!(f.iter().any(|l| l.contains("end game (pid 301)? y or n")));
    }

    #[test]
    fn every_box_is_whole_and_the_boxes_fill_the_width() {
        let (r, hist, users) = review();
        for (cols, rows) in [(200, 50), (140, 40), (120, 40), (80, 24), (60, 15)] {
            let s = review_screen(cols, rows, false, &users);
            let lines = lines_of(&frame_with(&r, &view(), &s, &hist));
            for l in &lines[1..] {
                assert_eq!(cells(l), cols, "{cols}x{rows}: {l:?}");
                let first = l.chars().next().unwrap();
                let last = l.chars().last().unwrap();
                assert!("╭│╰".contains(first), "{cols}x{rows}: {l:?}");
                assert!("╮│╯".contains(last), "{cols}x{rows}: {l:?}");
            }
            // Every top corner has its bottom corner under it, with sides between.
            for (y, l) in lines.iter().enumerate() {
                for (x, c) in l.chars().enumerate() {
                    if c != '╭' {
                        continue;
                    }
                    let below: Vec<char> = lines[y + 1..]
                        .iter()
                        .map(|l| l.chars().nth(x).unwrap())
                        .collect();
                    let end = below.iter().position(|c| *c == '╰');
                    let end = end
                        .unwrap_or_else(|| panic!("{cols}x{rows}: a box at {y},{x} has no bottom"));
                    assert!(
                        below[..end].iter().all(|c| *c == '│'),
                        "{cols}x{rows}: {y},{x}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_net_overview_is_a_share_of_the_link_or_the_plain_rate() {
        let mut n = NetSample {
            name: "eth0".into(),
            rx_bps: Some(12_500_000),
            tx_bps: Some(1_000),
            link_bps: Some(1_000_000_000),
            addrs: Vec::new(),
        };
        assert_eq!(net_load(&n), Some(NetLoad::Pct(10.0)));
        n.link_bps = None;
        assert_eq!(net_load(&n), Some(NetLoad::Rate(12_500_000)));
        n.link_bps = Some(1_000_000_000);
        n.rx_bps = Some(0);
        n.tx_bps = Some(0);
        assert_eq!(net_load(&n), Some(NetLoad::Pct(0.0)));
        n.rx_bps = None;
        n.tx_bps = None;
        assert_eq!(net_load(&n), None);
    }

    #[test]
    fn the_first_line_gives_up_its_right_items_in_order() {
        let mut r = sample(12, Vec::new());
        r.gpus.push(aether::sys::GpuSample {
            name: "g".into(),
            vendor: "v",
            pct: Some(7.5),
            temp_c: None,
            power_w: None,
            vram_used: None,
            vram_total: None,
            fan_rpm: None,
        });
        r.nets[0].link_bps = Some(100_000_000);
        let first = |r: &Reading, cols: usize| {
            let f = frame(r, &view(), &screen(cols, 30, false));
            lines_of(&f).remove(0)
        };
        let wide = first(&r, 200);
        assert_eq!(wide.chars().count(), 200);
        for want in [
            "CPU",
            "GPU",
            "RAM",
            "NET",
            "━",
            "23%",
            "7.5%",
            "62%",
            "up 3d 04:12",
            "12 threads",
            "1.0 s",
        ] {
            assert!(wide.contains(want), "{want} in {wide}");
        }
        // 27.2 Mbit/s of 100 Mbit/s
        assert!(wide.contains("27%"), "{wide}");
        let mid = first(&r, 120);
        assert_eq!(mid.chars().count(), 120);
        assert!(!mid.contains('━') && !mid.contains('╌'), "{mid}");
        for want in [
            "CPU 23%",
            "GPU 7.5%",
            "RAM 62%",
            "NET 27%",
            "up 3d 04:12",
            "12 threads",
            "1.0 s",
        ] {
            assert!(mid.contains(want), "{want} in {mid}");
        }
        let narrow = first(&r, 60);
        assert_eq!(narrow.chars().count(), 60);
        assert!(
            narrow.contains("CPU 23%") && narrow.contains("RAM 62%"),
            "{narrow}"
        );
        for gone in ["1.0 s", "threads", "up 3d"] {
            assert!(!narrow.contains(gone), "{gone} in {narrow}");
        }
        let tiny = first(&r, 35);
        assert!(
            tiny.contains("CPU 23%") && tiny.contains("RAM 62%"),
            "{tiny}"
        );
        assert!(!tiny.contains("GPU") && !tiny.contains("NET"), "{tiny}");
        // no link speed: the rate stands in; no GPU: left out
        r.nets[0].link_bps = None;
        r.gpus.clear();
        let rate = first(&r, 120);
        assert!(
            rate.contains("NET 3.2 MiB/s") && !rate.contains("GPU"),
            "{rate}"
        );
    }

    #[test]
    fn a_missing_source_is_left_out_not_shown_empty() {
        let mut r = sample(4, Vec::new());
        r.battery = None;
        for (cols, rows) in [(200, 50), (120, 40), (80, 24)] {
            let f = lines_of(&frame(&r, &view(), &screen(cols, rows, false)));
            let text = f.join("\n");
            assert!(!text.contains("GPU ─"), "{cols}x{rows}");
            assert!(!text.contains("Sensors"));
            assert!(!text.contains("battery"));
            assert!(text.contains("╭─ Processes"));
            assert!(text.contains("0 in 0 apps"));
            assert!(f.last().unwrap().contains("? help"));
        }
        // Without disks and network their boxes go too, and memory takes the right column.
        r.disks.clear();
        r.nets.clear();
        let text = lines_of(&frame(&r, &view(), &screen(200, 50, false))).join("\n");
        assert!(!text.contains("Disks") && !text.contains("Network"));
        assert!(text.contains("╮╭─ Memory"));
    }

    #[test]
    fn two_hundred_threads_fall_back_to_cells_and_keep_the_table() {
        let r = sample(200, vec![proc(1, "init", 0.0, 1 << 20)]);
        for (cols, rows) in [(200, 50), (120, 40), (80, 24)] {
            let f = lines_of(&frame(&r, &view(), &screen(cols, rows, false)));
            assert_eq!(f.len(), rows);
            // No grid entry ("199 ███"), and the cells take a few rows.
            assert!(!f.iter().any(|l| l.contains("199 ━") || l.contains("199 ╌")));
            let cell_rows = f
                .iter()
                .filter(|l| l.chars().filter(|c| "▁▂▃▄▅▆▇█".contains(*c)).count() >= 30)
                .count();
            assert!((1..=4).contains(&cell_rows), "{cols}x{rows}: {cell_rows}");
            assert!(f.iter().any(|l| l.contains("init")));
        }
        // Twelve threads are a grid of entries with their bars.
        let r = sample(12, vec![proc(1, "init", 0.0, 1 << 20)]);
        let f = lines_of(&frame(&r, &view(), &screen(200, 50, false)));
        assert!(f.iter().any(|l| l.contains(" 11 ") && l.contains("77%")));
    }

    #[test]
    fn a_long_name_and_a_long_mount_are_cut() {
        assert_eq!(cut_middle("/run/media/someone/Data", 13), "/run…one/Data");
        assert_eq!(cut_middle("/data", 13), "/data");
        assert_eq!(cells(&cut_middle(&"x".repeat(40), 10)), 10);
        assert_eq!(cut("browser", 4), "bro…");
        let mut r = sample(4, vec![proc(10, &"long-name-".repeat(30), 9.1, 2 << 30)]);
        r.disks[0].mount = "/run/media/someone/a-disk-with-a-very-long-label/Data".to_string();
        let f = lines_of(&frame(&r, &view(), &screen(120, 40, false))).join("\n");
        assert!(f.contains("/run"));
        assert!(f.contains("long-name-long-name-"));
        assert!(f.contains('…'));
    }

    #[test]
    fn wide_characters_take_two_cells_and_controls_are_shown_as_marks() {
        assert_eq!(cells("終端"), 4);
        assert_eq!(cells("e\u{301}"), 1);
        let r = sample(
            4,
            vec![
                proc(
                    10,
                    "終端終端終端終端終端終端終端終端終端終端終端終端",
                    9.1,
                    1 << 20,
                ),
                proc(11, "bad\x1b[2Jname", 1.0, 1 << 20),
            ],
        );
        for cols in [60, 80, 120] {
            let f = frame(&r, &view(), &screen(cols, 24, false));
            assert_fits(&f, cols, 24, "wide");
            assert!(!f.contains("\x1b[2J"));
            assert!(lines_of(&f).join("\n").contains("bad?[2Jname"));
        }
    }

    #[test]
    fn colour_uses_only_its_roles_and_reset() {
        let mut procs = vec![proc(10, "game", 95.0, 2 << 30)];
        procs[0].gpu_pct = Some(31.0);
        let (mut r, hist, users) = review();
        r.procs = procs;
        r.cpu.temp_c = Some(96.0);
        r.cpu.threads[3] = 75.0;
        let mut s = review_screen(200, 50, true, &users);
        s.selected = Some(0);
        let f = frame_with(&r, &view(), &s, &hist);
        let mut params = Vec::new();
        let mut rest = f.as_str();
        while let Some(at) = rest.find("\x1b[") {
            let after = &rest[at + 2..];
            let end = after.find(|c: char| c.is_ascii_alphabetic()).unwrap();
            let (body, letter) = (&after[..end], &after[end..=end]);
            if letter == "m" {
                params.extend(body.split(';').map(str::to_string));
            } else {
                assert!(
                    ["K", "H", "?2026h", "?2026l"].contains(&format!("{body}{letter}").as_str()),
                    "unexpected code {body}{letter}"
                );
            }
            rest = &after[end + 1..];
        }
        for p in &params {
            assert!(
                ["0", "1", "2", "7", "31", "33", "36"].contains(&p.as_str()),
                "unexpected {p}"
            );
        }
        for p in ["1", "2", "7", "31", "33", "36"] {
            assert!(params.iter().any(|x| x == p), "no {p}");
        }
    }

    #[test]
    fn a_frame_without_colour_has_no_colour_codes() {
        let (r, hist, users) = review();
        let f = frame_with(&r, &view(), &review_screen(200, 50, false, &users), &hist);
        assert!(!f.contains('m') || !f.contains("\x1b[0m"));
        assert!(!f.contains("\x1b[3") && !f.contains("\x1b[2m") && !f.contains("\x1b[7"));
        // The marker still shows the selection.
        assert_eq!(lines_of(&f).iter().filter(|l| l.contains('▸')).count(), 1);
    }

    #[test]
    fn the_selected_row_is_reversed_across_the_box() {
        let r = sample(
            4,
            vec![
                proc(1, "init", 3.0, 1 << 20),
                proc(2, "other", 1.0, 1 << 20),
            ],
        );
        let f = frame(&r, &view(), &screen(120, 40, true));
        let row = f.split("\r\n").find(|l| l.contains('▸')).unwrap();
        assert!(row.starts_with("\x1b[2m│\x1b[0m\x1b[7m▸\x1b[0m"));
        assert!(row.ends_with("\x1b[0m\x1b[K\x1b[2m│\x1b[0m"), "{row:?}");
        let other = f.split("\r\n").find(|l| l.contains("other")).unwrap();
        assert!(!other.contains("\x1b[7"));
    }

    #[test]
    fn a_full_line_is_erased_before_its_last_cell() {
        let r = sample(4, Vec::new());
        let f = frame(&r, &view(), &screen(80, 24, false));
        assert!(f.starts_with("\x1b[?2026h\x1b[H"));
        assert!(f.ends_with("\x1b[K╯\x1b[?2026l"));
        for l in f.split("\r\n").skip(1) {
            assert_eq!(l.matches("\x1b[K").count(), 1);
        }
    }

    #[test]
    fn the_keys_fit_the_border_and_help_is_always_kept() {
        let r = sample(4, Vec::new());
        for cols in [60, 80, 120, 200] {
            let f = lines_of(&frame(&r, &view(), &screen(cols, 24, false)));
            let last = f.last().unwrap();
            assert!(last.starts_with("╰─ q quit"), "{last}");
            assert!(last.contains("? help ─"), "{last}");
        }
        let mut s = screen(80, 24, false);
        s.keys = false;
        let f = lines_of(&frame(&r, &view(), &s));
        assert!(f.last().unwrap().contains("ctrl-c to leave"));
        s.keys = true;
        s.prompt = Some("/ brow".to_string());
        let f = lines_of(&frame(&r, &view(), &s));
        assert!(f
            .last()
            .unwrap()
            .contains("/ brow▏ · enter keep · esc clear"));
    }

    #[test]
    fn the_order_and_the_filter_are_in_the_border_and_the_sorted_header_is_accented() {
        let r = sample(4, vec![proc(1, "init", 3.0, 1 << 20)]);
        let v = parse(&args("--by mem --reverse --filter ini --flat"))
            .unwrap()
            .view;
        let f = frame(&r, &v, &screen(120, 40, true));
        let text = lines_of(&f).join("\n");
        assert!(text.contains("Processes ─ by memory ▴ · filter ini"));
        assert!(text.contains("1 processes"));
        assert!(f.contains("\x1b[36mMEM\x1b[0m"));
        assert!(text.contains("PID") && text.contains("USER") && text.contains("NAME"));
    }

    #[test]
    fn a_graph_stacks_eighths_from_the_bottom_and_fills_from_the_right() {
        let mut ring = Ring::default();
        for v in [100.0, 50.0, 0.0, 31.25] {
            ring.push(Some(v));
        }
        let (lines, shown, peak) = graph(&ring, 2, 6, Some(100.0), true, false);
        assert_eq!(shown, 4);
        assert_eq!(peak, 100.0);
        assert_eq!(lines[0].text, "  █   ");
        assert_eq!(lines[1].text, "  ██▁▅");
        // Without a top, the highest value shown is the top.
        let (lines, _, _) = graph(&ring, 1, 2, None, false, false);
        assert_eq!(lines[0].text, "▁█");
    }

    #[test]
    fn the_overlays_are_boxes_in_place_of_the_table() {
        let (r, hist, users) = review();
        let mut s = review_screen(120, 40, false, &users);
        s.overlay = Overlay::Help;
        let text = lines_of(&frame_with(&r, &view(), &s, &hist)).join("\n");
        assert!(text.contains("╭─ Keys"));
        assert!(text.contains("reverse the order"));
        s.overlay = Overlay::Sensors;
        let text = lines_of(&frame_with(&r, &view(), &s, &hist)).join("\n");
        assert!(text.contains("╭─ Sensors ─ 6"));
        assert!(text.contains("psu power"));
        s.overlay = Overlay::Detail;
        let text = lines_of(&frame_with(&r, &view(), &s, &hist)).join("\n");
        assert!(text.contains("╭─ Process ─"));
        assert!(text.contains("Process ─ editor") && text.contains("processes 1"));
        s.overlay = Overlay::Table;
        s.prompt = Some("end game (pid 301)? y or n".to_string());
        let lines = lines_of(&frame_with(&r, &view(), &s, &hist));
        assert!(lines[lines.len() - 3].starts_with("╭─ End process"));
        assert!(lines[lines.len() - 2].contains("end game (pid 301)? y or n"));
    }

    #[test]
    fn short_formats_keep_three_digits_and_no_trailing_zero() {
        assert_eq!(sig3(9.8), "9.8");
        assert_eq!(sig3(15.5), "15.5");
        assert_eq!(sig3(212.4), "212");
        assert_eq!(sig3(400.0), "400");
        assert_eq!(sig3(1.04), "1");
        assert_eq!(proc_pct(9.14), "9.1%");
        assert_eq!(proc_pct(31.4), "31%");
        assert_eq!(age_text(45), "45s");
        assert_eq!(age_text(7_300), "2h");
        assert_eq!(age_text(3 * 86_400), "3d");
        assert_eq!(
            uptime_short(3 * 86_400 + 4 * 3_600 + 12 * 60),
            "up 3d 04:12"
        );
        assert_eq!(uptime_short(4 * 3_600 + 12 * 60), "up 04:12");
        let g = 1024_u64 * 1024 * 1024;
        let mut l = Line::new(80, false);
        size_pair(&mut l, g * 98 / 10, g * 155 / 10);
        assert_eq!(l.text, "9.8 of 15.5 GiB");
        let mut l = Line::new(80, false);
        size_pair(&mut l, 400 * 1024 * 1024, 16 * g);
        assert_eq!(l.text, "400 MiB of 16 GiB");
        let mut l = Line::new(80, false);
        size_item(&mut l, 3_355_443, "/s", Role::Plain);
        assert_eq!(l.text, "3.2 MiB/s");
        let users: HashMap<u32, String> = [(1000, "dana".to_string())].into_iter().collect();
        assert_eq!(user_name(1000, &users), "dana");
        assert_eq!(user_name(977, &users), "977");
    }

    #[test]
    fn a_zero_in_the_table_is_dim_and_a_missing_one_is_a_dash() {
        let r = sample(
            4,
            vec![proc(1, "init", 0.0, 1 << 20), proc(2, "x", 1.0, 1 << 20)],
        );
        let mut s = screen(120, 40, true);
        s.selected = Some(0);
        let f = frame(&r, &view(), &s);
        assert!(f.contains("\x1b[2m0.0%\x1b[0m"));
        assert!(lines_of(&f).iter().any(|l| l.contains("—")));
    }

    #[test]
    fn a_tall_screen_gives_the_graphs_rows_and_a_short_one_gives_them_up() {
        let (r, hist, users) = review();
        let tall = lines_of(&frame_with(
            &r,
            &view(),
            &review_screen(200, 80, false, &users),
            &hist,
        ));
        let short = lines_of(&frame_with(
            &r,
            &view(),
            &review_screen(200, 24, false, &users),
            &hist,
        ));
        // The CPU box: its graph has its eight rows on the tall screen.
        let cpu_rows = |f: &[String]| f[1..].iter().position(|l| l.starts_with('╰')).unwrap() + 1;
        assert_eq!(cpu_rows(&tall), 2 + 1 + 8 + 2);
        assert!(cpu_rows(&short) < cpu_rows(&tall));
        // The short screen gave up the memory graph: its box ends under its three lines.
        let m = short.iter().position(|l| l.contains("╭─ Memory")).unwrap();
        assert!(short[m + 4].contains("│╰"), "{}", short[m + 4]);
        // Above the processes: at most 55 % of a tall screen.
        let at = tall
            .iter()
            .position(|l| l.contains("╭─ Processes"))
            .unwrap();
        assert!(at <= 80 * UPPER_SHARE / 100, "{at}");
        assert!(!short.join("\n").contains("Sensors"));
    }

    /// Writes the review frames, text and colour, when AETHER_FRAME_DIR names a folder.
    #[test]
    fn the_review_frames_are_written_when_a_folder_is_given() {
        let Some(dir) = std::env::var_os("AETHER_FRAME_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let (r, hist, users) = review();
        for (cols, rows) in [
            (200, 50),
            (160, 45),
            (140, 40),
            (120, 40),
            (100, 30),
            (80, 24),
            (60, 15),
        ] {
            let s = review_screen(cols, rows, false, &users);
            let text = frame_with(&r, &view(), &s, &hist);
            std::fs::write(
                dir.join(format!("view-{cols}x{rows}.txt")),
                lines_of(&text).join("\n") + "\n",
            )
            .unwrap();
            let s = review_screen(cols, rows, true, &users);
            let ansi = frame_with(&r, &view(), &s, &hist);
            std::fs::write(dir.join(format!("view-{cols}x{rows}.ansi")), ansi).unwrap();
        }
        for (name, overlay, prompt) in [
            ("help", Overlay::Help, None),
            ("sensors", Overlay::Sensors, None),
            ("detail", Overlay::Detail, None),
            ("ask", Overlay::Table, Some("end game (pid 301)? y or n")),
            ("filter", Overlay::Table, Some("/ ga")),
        ] {
            let mut s = review_screen(120, 40, true, &users);
            s.overlay = overlay;
            s.prompt = prompt.map(str::to_string);
            let ansi = frame_with(&r, &view(), &s, &hist);
            std::fs::write(dir.join(format!("view-{name}.ansi")), ansi).unwrap();
        }
    }
}
