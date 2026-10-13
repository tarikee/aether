//! The full-screen view: sections under rules, bars and history lines, the process table down to
//! the last line. A pure function of one reading and the screen's state: no system call, no
//! clock, no environment. Every line is cut to its width, so nothing wraps.

use super::history::History;
use super::text::{self, Row};
use super::tree::Tree;
use crate::cli::View;
use aether::sys::{
    BatterySample, CpuSample, DiskSample, GpuSample, MemSample, NetSample, Proc, Sensor, SensorKind,
};
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
    /// Seconds between readings: the title's interval, and what a history line spans.
    pub interval_s: f64,
    /// Whether the list is paused (shown in the title).
    pub paused: bool,
    /// What is shown in place of the process table.
    pub overlay: Overlay,
    /// Whether the keys are on: the last line names them, else it says how to leave.
    pub keys: bool,
    /// The last line in place of the keys: the filter being typed, or a question.
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

/// The four colour roles; plain text needs none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Plain,
    Accent,
    Warn,
    Critical,
    Dim,
}

/// The margin on each side of the screen, and the indent of a body line under a rule.
const MARGIN: usize = 1;
const INDENT: usize = 2;
/// The most cells of one history line.
const HISTORY_MAX: usize = 60;
/// The room a history line leaves after its field for its label ("  last 60 s").
const LABEL_ROOM: usize = 14;
/// Rows the process table needs below the sensors: its rule, its header, 15 rows and the key line.
const TABLE_MIN: usize = 15 + 3;
/// Lines of sensors at most in the normal view.
const SENSOR_LINES: usize = 3;
/// Disk and interface rows at most before "+N more".
const DISKS_MAX: usize = 4;
const NETS_MAX: usize = 3;
const KEYS_LINE: &str =
    "↑↓ select   ←→ sort   / filter   t tree   k end   space pause   ? keys   q quit";
const LEAVE_LINE: &str = "ctrl-c to leave";
const EIGHTHS: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
const LEVELS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
/// The keys, for the help view: the keys, then what they do.
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

/// One line of the frame: its text with its colour codes, and the width it takes. It never
/// grows past `cap`; a piece that does not fit is cut.
struct Line {
    text: String,
    width: usize,
    cap: usize,
    colour: bool,
}

impl Line {
    fn new(cap: usize, colour: bool) -> Line {
        Line {
            text: String::new(),
            width: 0,
            cap,
            colour,
        }
    }

    /// A body line: the indent, then the items, each after three spaces.
    fn body(cap: usize, colour: bool) -> Line {
        let mut l = Line::new(cap, colour);
        l.plain(&" ".repeat(INDENT));
        l
    }

    fn code(&self, role: Role) -> Option<&'static str> {
        if !self.colour {
            return None;
        }
        match role {
            Role::Plain => None,
            Role::Accent => Some("36"),
            Role::Warn => Some("33"),
            Role::Critical => Some("31"),
            Role::Dim => Some("2"),
        }
    }

    fn push(&mut self, s: &str, role: Role) {
        let room = self.cap.saturating_sub(self.width);
        if room == 0 || s.is_empty() {
            return;
        }
        let owned: String;
        let piece = if s.chars().count() > room {
            owned = s.chars().take(room).collect();
            owned.as_str()
        } else {
            s
        };
        self.width += piece.chars().count();
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

    fn gap(&mut self, n: usize) {
        self.push(&" ".repeat(n), Role::Plain);
    }

    /// Three spaces between items, not before the first one (the body's indent is not an item).
    fn sep(&mut self) {
        if self.width > INDENT {
            self.gap(3);
        }
    }

    /// A measure such as "9.8 GiB": the number in `role`, the unit dim.
    fn measure(&mut self, s: &str, role: Role) {
        match s.rsplit_once(' ') {
            Some((num, unit)) => {
                self.push(num, role);
                self.push(" ", Role::Dim);
                self.push(unit, Role::Dim);
            }
            None => self.push(s, role),
        }
    }

    /// Adds another line whole, its codes included. It is dropped when it does not fit.
    fn join(&mut self, o: &Line) {
        if self.width + o.width > self.cap {
            return;
        }
        self.text.push_str(&o.text);
        self.width += o.width;
    }

    fn pad_to(&mut self, width: usize) {
        if self.width < width {
            self.gap(width - self.width);
        }
    }
}

/// Draws one frame into `out`: the synchronised-output marks, the cursor home, each line ended
/// with erase-to-end-of-line, and the rest of the screen erased after the last one. `s` keeps
/// the scroll that keeps the marker in view.
pub fn render(
    out: &mut Vec<u8>,
    r: &Reading,
    v: &View,
    s: &mut Screen,
    hist: &History,
    tree: &mut Tree,
    rank: &dyn Fn(&Proc) -> f64,
) {
    let cap = s.cols.saturating_sub(2 * MARGIN);
    let cells = if s.cols < 90 { 16 } else { 20 };
    let colour = s.colour;
    let mut lines: Vec<Line> = vec![title(r, s, cap)];
    if v.shows("cpu") {
        lines.extend(cpu_block(&r.cpu, hist, s, cap, cells));
    }
    if v.shows("mem") {
        lines.extend(mem_block(&r.mem, cap, cells, colour));
    }
    if v.shows("gpu") {
        for g in &r.gpus {
            lines.extend(gpu_block(g, cap, cells, colour));
        }
    }
    let disks_on = v.shows("disk") && r.disks.iter().any(|d| v.shows_disk(&d.mount));
    let nets_on = v.shows("net") && r.nets.iter().any(|n| v.shows_net(&n.name));
    let first_net = r.nets.first().map(|n| n.name.as_str());
    if disks_on && nets_on && s.cols >= 100 {
        let lw = cap.saturating_sub(2) / 2;
        let rw = cap.saturating_sub(2 + lw);
        let left = disk_block(&r.disks, v, lw, colour);
        let right = net_block(&r.nets, v, hist, first_net, rw, colour);
        lines.extend(beside(&left, &right, lw, cap, colour));
    } else {
        if disks_on {
            lines.extend(disk_block(&r.disks, v, cap, colour));
        }
        if nets_on {
            lines.extend(net_block(&r.nets, v, hist, first_net, cap, colour));
        }
    }
    // The upper sections give way on a short terminal, so the table keeps its rows.
    lines.truncate(s.rows.saturating_sub(4));
    match s.overlay {
        Overlay::Table => {
            let room = s.rows.saturating_sub(lines.len());
            if v.shows("sensors") && !r.sensors.is_empty() && room > 1 + TABLE_MIN {
                let max = (room - 1 - TABLE_MIN).min(SENSOR_LINES);
                lines.extend(sensor_block(&r.sensors, max, cap, colour));
            }
            if v.shows("procs") {
                lines.extend(table(r, v, s, tree, rank, cap, colour));
            }
        }
        Overlay::Sensors => {
            let max = s.rows.saturating_sub(lines.len() + 2);
            lines.extend(sensor_block(&r.sensors, max, cap, colour));
        }
        Overlay::Help => lines.extend(help_block(cap, colour)),
        Overlay::Detail => lines.extend(table(r, v, s, tree, rank, cap, colour)),
    }
    // The last line is the keys (or the prompt); a screen too short for the rest keeps only what fits.
    lines.truncate(s.rows.saturating_sub(1));
    while lines.len() < s.rows.saturating_sub(1) {
        lines.push(Line::new(cap, colour));
    }
    let mut key = Line::new(cap, colour);
    match &s.prompt {
        Some(p) => key.plain(p),
        None => key.push(if s.keys { KEYS_LINE } else { LEAVE_LINE }, Role::Dim),
    }
    lines.push(key);
    if s.rows == 0 {
        lines.clear();
    }
    emit(out, &lines);
}

/// Writes the lines: each one after the margin, ended with erase-to-end-of-line; the last one
/// is followed by the rest of the screen's erase, all in one write.
fn emit(out: &mut Vec<u8>, lines: &[Line]) {
    out.extend_from_slice(b"\x1b[?2026h\x1b[H");
    for (i, l) in lines.iter().enumerate() {
        out.push(b' ');
        out.extend_from_slice(l.text.as_bytes());
        out.extend_from_slice(b"\x1b[K");
        if i + 1 < lines.len() {
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(b"\x1b[J\x1b[?2026l");
}

/// Cuts `s` to `n` characters, the last one an ellipsis when it is cut.
fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    if n == 0 {
        return String::new();
    }
    let mut t: String = s.chars().take(n - 1).collect();
    t.push('…');
    t
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
fn size_pair(l: &mut Line, a: u64, b: u64, suffix: &str) {
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
    l.push(suffix, Role::Dim);
}

/// A percentage in a summary line or beside a bar: whole.
fn pct_item(l: &mut Line, p: f32, role: Role) {
    l.push(&format!("{p:.0}"), role);
    l.push("%", Role::Dim);
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

/// A bar of `cells` cells: full blocks, the fraction in eighths, the rest empty.
fn bar(l: &mut Line, frac: f32, cells: usize, role: Role) {
    let fill = (frac.clamp(0.0, 1.0) * cells as f32).min(cells as f32);
    let full = fill.floor() as usize;
    let part = ((fill - fill.floor()) * 8.0) as usize;
    l.push(&"█".repeat(full), role);
    let mut used = full;
    if part > 0 && full < cells {
        l.push(EIGHTHS[part.min(7)], role);
        used += 1;
    }
    l.push(&"░".repeat(cells.saturating_sub(used)), Role::Dim);
}

/// A history line: one cell per value, oldest left. `top` is the scale's top; without one the
/// highest value shown is the top. The caller gives the whole field, blanks included.
fn spark(l: &mut Line, vals: impl Iterator<Item = Option<f32>> + Clone, top: Option<f32>) {
    let top = top.unwrap_or_else(|| vals.clone().flatten().fold(0.0_f32, f32::max));
    let mut s = String::new();
    for v in vals {
        match v {
            Some(x) => {
                let level = if top > 0.0 {
                    ((x / top).clamp(0.0, 1.0) * 7.0).round() as usize
                } else {
                    0
                };
                s.push_str(LEVELS[level.min(7)]);
            }
            None => s.push(' '),
        }
    }
    l.plain(&s);
}

/// A history field of `width` cells: `pad` blanks on the left, then the newest values.
fn field(
    newest: impl Iterator<Item = Option<f32>> + Clone,
    pad: usize,
) -> impl Iterator<Item = Option<f32>> + Clone {
    std::iter::repeat_n(None::<f32>, pad).chain(newest)
}

/// A rule: the label in accent, the sub-title plain, the dashes, and the right text, followed by
/// dashes to the margin.
fn rule(cap: usize, colour: bool, label: &str, sub: &str, right: &str) -> Line {
    let mut l = Line::new(cap, colour);
    l.push("── ", Role::Dim);
    l.push(label, Role::Accent);
    if !sub.is_empty() {
        l.plain("  ");
        l.plain(sub);
    }
    l.push(" ", Role::Dim);
    if right.is_empty() {
        let fill = cap.saturating_sub(l.width);
        l.push(&"─".repeat(fill), Role::Dim);
    } else {
        let tail = format!(" {right} ");
        let fill = cap.saturating_sub(l.width + tail.chars().count() + 3);
        l.push(&"─".repeat(fill), Role::Dim);
        l.push(&tail, Role::Dim);
        l.push("───", Role::Dim);
    }
    l
}

/// The title: the name on the left; on the right the battery (when there is one and it fits),
/// the uptime, the threads, `paused` and the interval.
fn title(r: &Reading, s: &Screen, cap: usize) -> Line {
    let colour = s.colour;
    let mut right = Line::new(usize::MAX, colour);
    right.plain(&uptime_short(r.host.uptime_s));
    right.gap(3);
    right.plain(&format!("{} threads", r.host.threads));
    right.gap(3);
    if s.paused {
        right.push("paused", Role::Warn);
        right.gap(3);
    }
    right.measure(&format!("{:.1} s", s.interval_s), Role::Plain);
    let mut bat = Line::new(usize::MAX, colour);
    if let Some(b) = &r.battery {
        battery_text(&mut bat, b);
    }
    let brand = 6;
    let with_bat = bat.width > 0 && brand + bat.width + 3 + right.width <= cap;
    let tail = right.width + if with_bat { bat.width + 3 } else { 0 };
    let mut l = Line::new(cap, colour);
    l.plain("aether");
    l.pad_to(cap.saturating_sub(tail));
    if with_bat {
        l.join(&bat);
        l.gap(3);
    }
    l.join(&right);
    l
}

fn battery_text(l: &mut Line, b: &BatterySample) {
    l.push("battery ", Role::Dim);
    pct_item(l, b.pct, Role::Plain);
    l.sep();
    l.plain(b.state);
    if let Some(p) = b.power_w {
        l.sep();
        l.plain(&format!("{p:.0}"));
        l.push(" W", Role::Dim);
    }
    if let Some(m) = b.minutes_left {
        l.sep();
        l.plain(&format!("{}h {:02}m left", m / 60, m % 60));
    }
}

/// The CPU: a summary line, its history, and the threads.
fn cpu_block(c: &CpuSample, hist: &History, s: &Screen, cap: usize, cells: usize) -> Vec<Line> {
    let colour = s.colour;
    let mut out = vec![rule(cap, colour, "CPU", "", "")];
    let mut l = Line::body(cap, colour);
    if let Some(p) = c.pct {
        l.sep();
        pct_item(&mut l, p, load_role(p));
        l.sep();
        bar(&mut l, p / 100.0, cells, load_role(p));
    }
    if let Some(mhz) = c.mhz {
        l.sep();
        l.plain(&format!("{:.1}", f64::from(mhz) / 1000.0));
        l.push(" GHz", Role::Dim);
    }
    if let Some(t) = c.temp_c {
        l.sep();
        l.push(&format!("{t:.0}"), temp_role(t));
        l.push(" °C", Role::Dim);
    }
    if let Some(w) = c.power_w {
        l.sep();
        l.plain(&format!("{w:.0}"));
        l.push(" W", Role::Dim);
    }
    if let Some([a, b, d]) = c.load {
        l.sep();
        l.push("load ", Role::Dim);
        l.plain(&format!("{a:.1} {b:.1} {d:.1}"));
    }
    if l.width > INDENT {
        out.push(l);
    }
    let width = HISTORY_MAX.min(cap.saturating_sub(INDENT + LABEL_ROOM));
    let n = hist.cpu.len().min(width);
    if n > 0 {
        let mut h = Line::body(cap, colour);
        spark(&mut h, field(hist.cpu.newest(n), width - n), Some(100.0));
        h.gap(2);
        h.push(
            &format!("last {} s", (n as f64 * s.interval_s).round() as u64),
            Role::Dim,
        );
        out.push(h);
    }
    if !c.threads.is_empty() {
        out.extend(threads_block(&c.threads, cap, colour));
    }
    out
}

/// The threads as cells, `N ▃` each when they fit in two lines, else the cells alone.
fn threads_block(threads: &[f32], cap: usize, colour: bool) -> Vec<Line> {
    let numbered = wrap_threads(threads, cap, colour, true);
    if numbered.len() <= 2 {
        return numbered;
    }
    let mut cells = wrap_threads(threads, cap, colour, false);
    cells.truncate(2);
    cells
}

fn wrap_threads(threads: &[f32], cap: usize, colour: bool, numbered: bool) -> Vec<Line> {
    let mut lines = vec![Line::body(cap, colour)];
    for (i, &t) in threads.iter().enumerate() {
        let cell = LEVELS[((t.clamp(0.0, 100.0) / 100.0) * 7.0).round() as usize];
        let number = if numbered {
            format!("{i} ")
        } else {
            String::new()
        };
        let need = number.chars().count() + 1 + 2;
        let Some(cur) = lines.last_mut() else { break };
        if cur.width > INDENT && cur.width + need > cap {
            lines.push(Line::body(cap, colour));
        }
        let Some(cur) = lines.last_mut() else { break };
        if numbered {
            cur.sep();
            cur.plain(&number);
        }
        cur.push(cell, load_role(t));
    }
    lines
}

fn mem_block(m: &MemSample, cap: usize, cells: usize, colour: bool) -> Vec<Line> {
    if m.total == 0 {
        return Vec::new();
    }
    let mut out = vec![rule(cap, colour, "Memory", "", "")];
    let mut l = Line::body(cap, colour);
    let frac = m.used as f32 / m.total as f32;
    size_pair(&mut l, m.used, m.total, "");
    l.sep();
    bar(&mut l, frac, cells, load_role(frac * 100.0));
    l.sep();
    l.push("cached ", Role::Dim);
    size_item(&mut l, m.cached, "", Role::Plain);
    if m.swap_total > 0 {
        l.sep();
        l.push("swap ", Role::Dim);
        size_pair(&mut l, m.swap_used, m.swap_total, "");
    }
    out.push(l);
    out
}

fn gpu_block(g: &GpuSample, cap: usize, cells: usize, colour: bool) -> Vec<Line> {
    let mut out = vec![rule(cap, colour, "GPU", &g.name, "")];
    let mut l = Line::body(cap, colour);
    if let Some(p) = g.pct {
        pct_item(&mut l, p, load_role(p));
        l.sep();
        bar(&mut l, p / 100.0, cells, load_role(p));
    }
    match (g.vram_used, g.vram_total) {
        (Some(u), Some(t)) => {
            l.sep();
            l.push("VRAM ", Role::Dim);
            size_pair(&mut l, u, t, "");
        }
        (Some(u), None) => {
            l.sep();
            l.push("VRAM ", Role::Dim);
            size_item(&mut l, u, "", Role::Plain);
        }
        _ => {}
    }
    if let Some(t) = g.temp_c {
        l.sep();
        l.push(&format!("{t:.0}"), temp_role(t));
        l.push(" °C", Role::Dim);
    }
    if let Some(rpm) = g.fan_rpm {
        l.sep();
        l.plain(&rpm.to_string());
        l.push(" rpm", Role::Dim);
    }
    if let Some(w) = g.power_w {
        l.sep();
        l.plain(&format!("{w:.0}"));
        l.push(" W", Role::Dim);
    }
    if l.width > INDENT {
        out.push(l);
    }
    out
}

/// The disks, at most four rows; a section with none is left out.
fn disk_block(disks: &[DiskSample], v: &View, cap: usize, colour: bool) -> Vec<Line> {
    let shown: Vec<&DiskSample> = disks.iter().filter(|d| v.shows_disk(&d.mount)).collect();
    if shown.is_empty() {
        return Vec::new();
    }
    // The mount column is as wide as the longest mount shown: 5 at least, 14 at most.
    let mw = shown
        .iter()
        .take(DISKS_MAX)
        .map(|d| d.mount.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(5, 14);
    let mut out = vec![rule(cap, colour, "Disks", "", "")];
    for d in shown.iter().take(DISKS_MAX) {
        let mut l = Line::body(cap, colour);
        let mount = cut(&d.mount, mw);
        let w = mount.chars().count();
        l.plain(&mount);
        l.gap(mw - w + 2);
        size_pair(&mut l, d.used, d.total, "");
        if let Some(b) = d.busy_pct {
            l.sep();
            l.push("busy ", Role::Dim);
            l.plain(&format!("{b:.0}"));
            l.push("%", Role::Dim);
        }
        for (label, rate) in [("r ", d.read_bps), ("w ", d.write_bps)] {
            if let Some(x) = rate.filter(|x| *x > 0) {
                l.sep();
                l.push(label, Role::Dim);
                size_item(&mut l, x, "/s", Role::Plain);
            }
        }
        out.push(l);
    }
    if shown.len() > DISKS_MAX {
        let mut l = Line::body(cap, colour);
        l.push(&format!("+{} more", shown.len() - DISKS_MAX), Role::Dim);
        out.push(l);
    }
    out
}

/// The interfaces, at most three rows; the first one's history line (its down rate) under it,
/// in the field the name starts at.
fn net_block(
    nets: &[NetSample],
    v: &View,
    hist: &History,
    first: Option<&str>,
    cap: usize,
    colour: bool,
) -> Vec<Line> {
    let shown: Vec<&NetSample> = nets.iter().filter(|n| v.shows_net(&n.name)).collect();
    if shown.is_empty() {
        return Vec::new();
    }
    let mut out = vec![rule(cap, colour, "Network", "", "")];
    for n in shown.iter().take(NETS_MAX) {
        let mut l = Line::body(cap, colour);
        l.plain(&n.name);
        if let Some(addr) = n.addrs.first() {
            l.sep();
            l.plain(addr);
        }
        l.sep();
        l.push("↓ ", Role::Dim);
        rate_or_dash(&mut l, n.rx_bps);
        l.sep();
        l.push("↑ ", Role::Dim);
        rate_or_dash(&mut l, n.tx_bps);
        out.push(l);
        if first == Some(n.name.as_str()) {
            let width = HISTORY_MAX.min(cap.saturating_sub(INDENT));
            let k = hist.down.len().min(width);
            if k > 0 {
                let mut h = Line::body(cap, colour);
                spark(&mut h, field(hist.down.newest(k), width - k), None);
                out.push(h);
            }
        }
    }
    if shown.len() > NETS_MAX {
        let mut l = Line::body(cap, colour);
        l.push(&format!("+{} more", shown.len() - NETS_MAX), Role::Dim);
        out.push(l);
    }
    out
}

fn rate_or_dash(l: &mut Line, rate: Option<u64>) {
    match rate {
        Some(x) => size_item(l, x, "/s", Role::Plain),
        None => l.plain("—"),
    }
}

/// Two blocks side by side: the left one `lw` wide, two spaces, the right one.
fn beside(left: &[Line], right: &[Line], lw: usize, cap: usize, colour: bool) -> Vec<Line> {
    let n = left.len().max(right.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut l = Line::new(cap, colour);
        if let Some(a) = left.get(i) {
            l.join(a);
        }
        l.pad_to(lw);
        if let Some(b) = right.get(i) {
            l.gap(2);
            l.join(b);
        }
        out.push(l);
    }
    out
}

/// The sensors under a rule: hottest temperatures first, then fans, then power, as many columns
/// as fit, at most `max_lines` lines. With none, a line says so.
fn sensor_block(sensors: &[Sensor], max_lines: usize, cap: usize, colour: bool) -> Vec<Line> {
    if max_lines == 0 {
        return Vec::new();
    }
    let mut out = vec![rule(cap, colour, "Sensors", "", "")];
    if sensors.is_empty() {
        let mut l = Line::body(cap, colour);
        l.push("No sensors.", Role::Dim);
        out.push(l);
        return out;
    }
    let mut list: Vec<&Sensor> = sensors.iter().collect();
    list.sort_by(|a, b| {
        kind_rank(a.kind)
            .cmp(&kind_rank(b.kind))
            .then(b.value.total_cmp(&a.value))
    });
    let cells: Vec<(String, String, Role)> = list
        .iter()
        .map(|s| {
            let (value, role) = match s.kind {
                SensorKind::Temp => (format!("{:.0} °C", s.value), temp_role(s.value)),
                SensorKind::Fan => (format!("{:.0} rpm", s.value), Role::Plain),
                SensorKind::Power => (format!("{:.0} W", s.value), Role::Plain),
            };
            (format!("{} {}", s.chip, s.label), value, role)
        })
        .collect();
    let width = cells
        .iter()
        .map(|(n, v, _)| n.chars().count() + 2 + v.chars().count())
        .max()
        .unwrap_or(0)
        .min(cap);
    let per_row = ((cap.saturating_sub(INDENT) + 2) / (width + 2)).max(1);
    let lines = cells.len().div_ceil(per_row).min(max_lines);
    for row in 0..lines {
        let mut l = Line::body(cap, colour);
        for (j, (name, value, role)) in cells.iter().skip(row * per_row).take(per_row).enumerate() {
            // A pair that does not fit whole is left out, never cut.
            let sep = if j > 0 { 2 } else { 0 };
            if l.width + sep + width > cap {
                break;
            }
            l.gap(sep);
            let start = l.width;
            l.plain(name);
            let used = value.chars().count();
            let pad = width.saturating_sub(l.width - start + used);
            l.gap(pad);
            l.measure(value, *role);
        }
        out.push(l);
    }
    out
}

fn kind_rank(kind: SensorKind) -> u8 {
    match kind {
        SensorKind::Temp => 0,
        SensorKind::Fan => 1,
        SensorKind::Power => 2,
    }
}

/// The keys, one per line, for the help view.
fn help_block(cap: usize, colour: bool) -> Vec<Line> {
    let mut out = vec![rule(cap, colour, "Keys", "", "")];
    for (key, what) in HELP {
        let mut l = Line::body(cap, colour);
        l.plain(key);
        l.pad_to(INDENT + 18);
        l.push(what, Role::Dim);
        out.push(l);
    }
    out
}

/// The process table: its rule, its header and the rows that fit down to the key line. In the
/// detail view, the selected process in full instead of the rows.
fn table(
    r: &Reading,
    v: &View,
    s: &mut Screen,
    tree: &mut Tree,
    rank: &dyn Fn(&Proc) -> f64,
    cap: usize,
    colour: bool,
) -> Vec<Line> {
    let grouped = !v.flat && !v.tree;
    let (rows, marks) = text::process_rows(&r.procs, v, rank, tree);
    let arrow = if v.reverse { "▴" } else { "▾" };
    let mut sub = format!("by {} {arrow}", text::sort_word(v.by));
    if v.steady && v.by.smooths() {
        sub.push_str(" steady");
    }
    if let Some(f) = &v.filter {
        sub.push_str(&format!(" filter {f}"));
    }
    let right = if grouped {
        format!("{} in {} apps", r.procs.len(), rows.len())
    } else {
        format!("{} processes", r.procs.len())
    };
    let mut out = vec![rule(cap, colour, "Processes", &sub, &right)];
    if s.overlay == Overlay::Detail {
        if let Some(row) = s.selected.and_then(|i| rows.get(i)) {
            out.extend(detail_block(row, cap, colour, &s.users));
        }
        return out;
    }
    // The name takes the rest; PID and USER go first when it would be too narrow.
    // The marker (2) and CPU 6, MEM 9, GPU 5, VRAM 9, DISK 11, each with its two spaces: 52.
    let fixed = |pid: bool, user: bool| 52 + if pid { 9 } else { 0 } + if user { 10 } else { 0 };
    let (mut show_pid, mut show_user) = (!grouped, !grouped);
    while cap.saturating_sub(fixed(show_pid, show_user)) < 16 && (show_pid || show_user) {
        if show_user {
            show_user = false;
        } else {
            show_pid = false;
        }
    }
    let name_w = cap.saturating_sub(fixed(show_pid, show_user));
    let head = Columns {
        pid: show_pid,
        user: show_user,
        name_w,
    };
    let mut header = Line::new(cap, colour);
    header.gap(2);
    head.field(
        &mut header,
        &["CPU", "MEM", "GPU", "VRAM", "DISK"],
        Role::Dim,
    );
    if show_pid {
        head.field(&mut header, &["PID"], Role::Dim);
    }
    if show_user {
        head.field(&mut header, &["USER"], Role::Dim);
    }
    header.push(&cut("APP", name_w), Role::Dim);
    out.push(header);
    let room = s.rows.saturating_sub(out.len() + 1);
    if let Some(i) = s.selected {
        if room > 0 {
            if i < s.scroll {
                s.scroll = i;
            } else if i >= s.scroll + room {
                s.scroll = i + 1 - room;
            }
        }
    }
    for (i, row) in rows.iter().enumerate().skip(s.scroll).take(room) {
        let mut l = Line::new(cap, colour);
        l.plain(if s.selected == Some(i) { "▸ " } else { "  " });
        let mark = marks.get(i).map_or("", String::as_str);
        head.row(&mut l, row, mark, grouped, &s.users);
        out.push(l);
    }
    out
}

/// The selected process in full: its ids, its numbers and its name.
fn detail_block(row: &Row, cap: usize, colour: bool, users: &HashMap<u32, String>) -> Vec<Line> {
    let mut ids = Line::body(cap, colour);
    ids.push("pid ", Role::Dim);
    ids.plain(&row.pid.map_or_else(|| "—".to_string(), |p| p.to_string()));
    ids.sep();
    ids.push("user ", Role::Dim);
    ids.plain(&user_name(row.uid, users));
    ids.sep();
    ids.push("threads ", Role::Dim);
    ids.plain(&row.threads.to_string());
    ids.sep();
    ids.push("processes ", Role::Dim);
    ids.plain(&row.count.to_string());
    let mut nums = Line::body(cap, colour);
    nums.push("cpu ", Role::Dim);
    nums.plain(&row.cpu.map_or_else(|| "—".to_string(), proc_pct));
    nums.sep();
    nums.push("mem ", Role::Dim);
    size_item(&mut nums, row.mem, "", Role::Plain);
    nums.sep();
    nums.push("gpu ", Role::Dim);
    nums.plain(
        &row.gpu
            .map_or_else(|| "—".to_string(), |x| format!("{x:.0}%")),
    );
    nums.sep();
    nums.push("vram ", Role::Dim);
    match row.vram {
        Some(x) => size_item(&mut nums, x, "", Role::Plain),
        None => nums.plain("—"),
    }
    let mut name = Line::body(cap, colour);
    name.push("name ", Role::Dim);
    name.plain(&row.name);
    vec![ids, nums, name]
}

/// The user's name for a uid; a uid with no name is shown as the number.
fn user_name(uid: u32, users: &HashMap<u32, String>) -> String {
    users.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
}

/// The table's columns after the marker: CPU, MEM, GPU, VRAM, DISK, then PID and USER when shown,
/// then the name in what is left.
struct Columns {
    pid: bool,
    user: bool,
    name_w: usize,
}

impl Columns {
    /// A right-aligned column of `text` in its width, then two spaces. The header's labels use
    /// the same widths.
    fn field(&self, l: &mut Line, labels: &[&str], role: Role) {
        for label in labels {
            let w = width_of(label);
            l.gap(w.saturating_sub(label.chars().count()));
            l.push(label, role);
            l.gap(2);
        }
    }

    fn row(&self, l: &mut Line, r: &Row, mark: &str, grouped: bool, users: &HashMap<u32, String>) {
        // A value that is exactly zero is drawn dim; a missing one is "—".
        let cpu = r.cpu.map(|x| (proc_pct(x), x == 0.0));
        let mem = Some((size_text(r.mem, ""), r.mem == 0));
        let gpu = r.gpu.map(|x| (format!("{x:.0}%"), x == 0.0));
        let vram = r.vram.map(|x| (size_text(x, ""), x == 0));
        let disk = match (r.read, r.write) {
            (None, None) => None,
            (a, b) => {
                let total = a.unwrap_or(0).saturating_add(b.unwrap_or(0));
                Some((size_text(total, "/s"), total == 0))
            }
        };
        for (i, f) in [cpu, mem, gpu, vram, disk].into_iter().enumerate() {
            let w = [6, 9, 5, 9, 11][i];
            match f {
                Some((text, zero)) => cell(l, &text, if zero { Role::Dim } else { Role::Plain }, w),
                None => cell(l, "—", Role::Plain, w),
            }
        }
        if self.pid {
            let pid = r.pid.map_or_else(|| "—".to_string(), |p| p.to_string());
            cell(l, &pid, Role::Plain, 7);
        }
        if self.user {
            cell(l, &user_name(r.uid, users), Role::Plain, 8);
        }
        let mut name = format!("{mark}{}", r.name);
        if grouped && r.count > 1 {
            name.push_str(&format!(" ×{}", r.count));
        }
        l.plain(&cut(&name, self.name_w));
    }
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
    l.gap(w.saturating_sub(text.chars().count()));
    l.push(&text, role);
    l.gap(2);
}

/// The width of a column by its label.
fn width_of(label: &str) -> usize {
    match label {
        "CPU" => 6,
        "MEM" => 9,
        "GPU" => 5,
        "VRAM" => 9,
        "DISK" => 11,
        "PID" => 7,
        "USER" => 8,
        _ => label.chars().count(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cli::parse;
    use aether::sys::HostSample;

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
            started_s: u64::from(pid),
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
                threads: 12,
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

    fn frame(r: &Reading, v: &View, s: &Screen) -> String {
        let mut s = s.clone();
        let mut hist = History::default();
        hist.push(r);
        let mut tree = Tree::new();
        let rank = |p: &Proc| f64::from(p.cpu_pct.unwrap_or(-1.0));
        let mut out = Vec::new();
        render(&mut out, r, v, &mut s, &hist, &mut tree, &rank);
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
        s.split("\r\n")
            .map(|l| l.strip_prefix(' ').unwrap_or(l).to_string())
            .collect()
    }

    #[test]
    fn every_line_fits_the_columns_at_every_width() {
        let procs = vec![
            proc(10, &"a-very-long-application-name-".repeat(4), 9.1, 2 << 30),
            proc(11, "browser", 4.0, 3 << 30),
        ];
        let r = sample(200, procs);
        let mut v = view();
        for cols in [60, 80, 100, 120, 200] {
            for rows in [15, 24, 40] {
                let f = frame(&r, &v, &screen(cols, rows, false));
                for l in lines_of(&f) {
                    assert!(
                        l.chars().count() < cols,
                        "{cols}x{rows}: {} columns in {l:?}",
                        l.chars().count()
                    );
                }
            }
        }
        v.tree = true;
        let f = frame(&r, &v, &screen(60, 15, false));
        assert!(lines_of(&f).len() <= 15);
    }

    #[test]
    fn a_missing_source_is_left_out_not_shown_empty() {
        let mut r = sample(4, vec![proc(1, "init", 0.0, 1 << 20)]);
        r.battery = None;
        let f = lines_of(&frame(&r, &view(), &screen(120, 40, false)));
        let text = f.join("\n");
        assert!(!text.contains("── GPU"));
        assert!(!text.contains("Sensors"));
        assert!(!text.contains("battery"));
        assert!(text.contains("Processes"));
        assert_eq!(f.last().map(String::as_str), Some(KEYS_LINE));
    }

    #[test]
    fn the_key_line_is_the_last_line_and_the_frame_is_one_screen() {
        let r = sample(4, vec![proc(1, "init", 0.0, 1 << 20)]);
        let f = frame(&r, &view(), &screen(80, 24, false));
        let lines = lines_of(&f);
        assert_eq!(lines.len(), 24);
        assert!(lines[23].starts_with("↑↓ select"));
        assert!(f.starts_with("\x1b[?2026h\x1b[H"));
        assert!(f.ends_with("\x1b[J\x1b[?2026l"));
    }

    #[test]
    fn colour_uses_only_the_four_roles_and_reset() {
        let mut procs = vec![proc(10, "game", 95.0, 2 << 30)];
        procs[0].gpu_pct = Some(31.0);
        let mut r = sample(12, procs);
        r.cpu.temp_c = Some(96.0);
        let f = frame(&r, &view(), &screen(120, 40, true));
        let mut codes = Vec::new();
        let mut rest = f.as_str();
        while let Some(at) = rest.find("\x1b[") {
            let after = &rest[at + 2..];
            let end = after.find(|c: char| c.is_ascii_alphabetic()).unwrap();
            codes.push(format!("{}{}", &after[..end], &after[end..=end]));
            rest = &after[end + 1..];
        }
        for code in &codes {
            assert!(
                ["36m", "33m", "31m", "2m", "0m", "K", "J", "H", "?2026h", "?2026l"]
                    .contains(&code.as_str()),
                "unexpected code {code:?}"
            );
        }
        assert!(codes.contains(&"31m".to_string()));
        assert!(codes.contains(&"36m".to_string()));
    }

    #[test]
    fn a_frame_without_colour_has_no_colour_codes() {
        let r = sample(12, vec![proc(10, "game", 9.1, 2 << 30)]);
        let f = frame(&r, &view(), &screen(120, 40, false));
        assert!(!f.contains("\x1b[3"));
        assert!(!f.contains("\x1b[2m"));
    }

    #[test]
    fn a_frame_at_120_by_36_and_80_by_24_is_written_for_a_look() {
        let procs = vec![
            proc(4021, "game", 9.1, 2_469_606_195),
            proc(4102, "browser", 4.0, 3_221_225_472),
            proc(4310, "editor", 1.2, 419_430_400),
        ];
        let r = sample(12, procs);
        let v = view();
        let wide = lines_of(&frame(&r, &v, &screen(120, 36, false)));
        assert_eq!(wide.len(), 36);
        let narrow = lines_of(&frame(&r, &v, &screen(80, 24, false)));
        assert_eq!(narrow.len(), 24);
        if let Ok(path) = std::env::var("AETHER_VIEW_FRAME") {
            let mut both = String::new();
            for l in &wide {
                both.push_str(l);
                both.push('\n');
            }
            both.push_str(&format!("{}\n", "-".repeat(80)));
            for l in &narrow {
                both.push_str(l);
                both.push('\n');
            }
            std::fs::write(path, both).unwrap();
        }
    }

    #[test]
    fn two_hundred_threads_keep_to_two_lines_and_the_screen() {
        let r = sample(200, vec![proc(1, "init", 0.0, 1 << 20)]);
        let f = lines_of(&frame(&r, &view(), &screen(120, 40, false)));
        assert_eq!(f.len(), 40);
        assert!(f.iter().any(|l| l.contains("Processes")));
        assert!(
            f.iter()
                .filter(|l| l.contains('▁') || l.contains('▂'))
                .count()
                <= 2
        );
    }

    /// The text of a frame, the history's cells and all, with colour as the screen has it.
    fn render_text(r: &Reading, hist: &History, s: &mut Screen) -> String {
        let mut tree = Tree::new();
        let rank = |p: &Proc| f64::from(p.cpu_pct.unwrap_or(-1.0));
        let mut out = Vec::new();
        render(&mut out, r, &view(), s, hist, &mut tree, &rank);
        String::from_utf8(out).unwrap()
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
        assert_eq!(
            uptime_short(3 * 86_400 + 4 * 3_600 + 12 * 60),
            "up 3d 04:12"
        );
        assert_eq!(uptime_short(4 * 3_600 + 12 * 60), "up 04:12");
        let g = 1024_u64 * 1024 * 1024;
        let mut l = Line::new(80, false);
        size_pair(&mut l, g * 98 / 10, g * 155 / 10, "");
        assert_eq!(l.text, "9.8 of 15.5 GiB");
        let mut l = Line::new(80, false);
        size_pair(&mut l, 400 * 1024 * 1024, 16 * g, "");
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
        let r = sample(4, vec![proc(1, "init", 0.0, 1 << 20)]);
        let mut s = screen(120, 40, true);
        let f = render_text(&r, &History::default(), &mut s);
        assert!(f.contains("\x1b[2m0.0%\x1b[0m"));
    }

    #[test]
    fn the_margin_holds_and_a_history_fills_a_fixed_field() {
        let r = sample(12, vec![proc(1, "init", 3.0, 1 << 20)]);
        let mut hist = History::default();
        for _ in 0..5 {
            hist.push(&r);
        }
        let mut s = screen(120, 40, false);
        let raw = render_text(&r, &hist, &mut s);
        // Every line after the first starts one column in, and none reaches the last column.
        assert!(raw.contains("\r\n ── CPU"));
        let lines = lines_of(&raw);
        assert!(lines.iter().all(|l| l.chars().count() <= 118));
        let h = lines.iter().find(|l| l.contains("last 5 s")).unwrap();
        // The indent, then the field of 60 cells before the label.
        let field = &h[..h.find("  last").unwrap()];
        assert_eq!(field.chars().count(), 2 + 60);
    }

    #[test]
    fn the_review_frames_are_written_when_a_folder_is_given() {
        let Some(dir) = std::env::var_os("AETHER_FRAME_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mb = |m: u64| m * 1024 * 1024;
        // Invented, and believable: the rows add up to most of the machine's 23 %, the GPU rows to
        // its 41 %, the VRAM rows to its 2.1 GiB, and the history has the shape of ordinary use.
        // Name, pid, CPU %, memory in MiB, GPU %, VRAM in MiB, bytes written a second.
        type Row = (
            &'static str,
            u32,
            f32,
            u64,
            Option<f32>,
            Option<u64>,
            Option<u64>,
        );
        let spec: &[Row] = &[
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
        ];
        let mut procs = Vec::new();
        for &(name, pid, cpu, mem, gpu, vram, write) in spec {
            let mut p = proc(pid, name, cpu, mb(mem));
            p.gpu_pct = gpu;
            p.vram = vram.map(mb);
            p.write_bps = write;
            procs.push(p);
        }
        let mut r = sample(12, procs);
        r.cpu.threads = vec![
            41.0, 18.0, 9.0, 33.0, 4.0, 27.0, 12.0, 6.0, 52.0, 3.0, 38.0, 33.0,
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
        // Ordinary use: quiet most of the time, a few bursts, a settle, and now.
        let cpu_hist = [
            14.0, 16.0, 12.0, 18.0, 22.0, 35.0, 48.0, 41.0, 30.0, 19.0, 15.0, 13.0, 17.0, 21.0,
            26.0, 58.0, 64.0, 44.0, 31.0, 22.0, 18.0, 15.0, 14.0, 19.0, 27.0, 33.0, 25.0, 21.0,
            17.0, 16.0, 20.0, 24.0, 38.0, 52.0, 47.0, 29.0, 21.0, 18.0, 15.0, 13.0, 12.0, 16.0,
            19.0, 23.0, 28.0, 42.0, 36.0, 24.0, 20.0, 17.0, 15.0, 14.0, 18.0, 22.0, 26.0, 21.0,
            19.0, 23.0, 27.0, 31.0, 24.0,
        ];
        let mut hist = History::default();
        for (i, &c) in cpu_hist.iter().enumerate() {
            r.cpu.pct = Some(c);
            r.nets[0].rx_bps = Some((i as u64 * 7_919 + 13) % 97 * 40_000);
            hist.push(&r);
        }
        r.cpu.pct = Some(23.4);
        r.nets[0].rx_bps = Some(2_900_000);
        r.nets[0].tx_bps = Some(117_000);
        hist.push(&r);
        let users = Rc::new(
            [(1000, "dana".to_string()), (0, "root".to_string())]
                .into_iter()
                .collect::<HashMap<u32, String>>(),
        );
        let mut s = Screen {
            cols: 120,
            rows: 36,
            colour: false,
            selected: Some(2),
            scroll: 0,
            interval_s: 1.0,
            paused: false,
            overlay: Overlay::Table,
            keys: true,
            prompt: None,
            users: Rc::clone(&users),
        };
        let text = render_text(&r, &hist, &mut s);
        std::fs::write(
            dir.join("aether-view-frame.txt"),
            lines_of(&text).join("\n") + "\n",
        )
        .unwrap();
        s.colour = true;
        let ansi = render_text(&r, &hist, &mut s).replace("\x1b[H", "");
        std::fs::write(dir.join("aether-view-frame.ansi"), ansi).unwrap();
        // The frame for the picture: 100 by 30, colour on.
        let mut small = Screen {
            cols: 100,
            rows: 30,
            ..s.clone()
        };
        let small_ansi = render_text(&r, &hist, &mut small).replace("\x1b[H", "");
        std::fs::write(dir.join("aether-view-100x30.ansi"), small_ansi).unwrap();
    }
}
