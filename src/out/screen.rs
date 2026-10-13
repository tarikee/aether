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
    /// The first process row on the screen.
    pub scroll: usize,
    /// Seconds between readings: the title's interval, and what a history line spans.
    pub interval_s: f64,
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
/// Rows the process table needs below the sensors: its rule, its header, 15 rows and the key line.
const TABLE_MIN: usize = 15 + 3;
/// Lines of sensors at most.
const SENSOR_LINES: usize = 3;
/// Disk and interface rows at most before "+N more".
const DISKS_MAX: usize = 4;
const NETS_MAX: usize = 3;
const KEY_LINE: &str = "ctrl-c to leave";
const EIGHTHS: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
const LEVELS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

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

    /// A body line: the indent, then the items, each after two spaces.
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

    /// Two spaces between items, not before the first one (the body's indent is not an item).
    fn sep(&mut self) {
        if self.width > INDENT {
            self.gap(2);
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
/// with erase-to-end-of-line, and the rest of the screen erased after the last one.
pub fn render(
    out: &mut Vec<u8>,
    r: &Reading,
    v: &View,
    s: &Screen,
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
    if v.shows("sensors") {
        let room = s.rows.saturating_sub(lines.len());
        lines.extend(sensor_block(&r.sensors, room, cap, colour));
    }
    if v.shows("procs") {
        lines.extend(table(r, v, s, tree, rank, cap, colour));
    }
    // The last line is the key line; a screen too short for the rest keeps only what fits.
    lines.truncate(s.rows.saturating_sub(1));
    while lines.len() < s.rows.saturating_sub(1) {
        lines.push(Line::new(cap, colour));
    }
    let mut key = Line::new(cap, colour);
    key.push(KEY_LINE, Role::Dim);
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
/// highest value shown is the top.
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

/// A rule: the label in accent, the sub-title plain, the rest dashes, and the right text.
fn rule(cap: usize, colour: bool, label: &str, sub: &str, right: &str) -> Line {
    let mut l = Line::new(cap, colour);
    l.push("── ", Role::Dim);
    l.push(label, Role::Accent);
    if !sub.is_empty() {
        l.plain("  ");
        l.plain(sub);
    }
    l.push(" ", Role::Dim);
    let tail = if right.is_empty() {
        String::new()
    } else {
        format!(" {right} ")
    };
    let fill = cap.saturating_sub(l.width + tail.chars().count());
    l.push(&"─".repeat(fill), Role::Dim);
    l.push(&tail, Role::Dim);
    l
}

/// The title: the name on the left; on the right the battery (when there is one and it fits),
/// the uptime, the threads and the interval.
fn title(r: &Reading, s: &Screen, cap: usize) -> Line {
    let colour = s.colour;
    let mut right = Line::new(usize::MAX, colour);
    right.push("up ", Role::Dim);
    right.plain(&text::uptime(r.host.uptime_s));
    right.gap(3);
    right.plain(&format!("{} threads", r.host.threads));
    right.gap(3);
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
    l.plain(&format!("{:.0}%", b.pct));
    l.gap(2);
    l.plain(b.state);
    if let Some(p) = b.power_w {
        l.gap(2);
        l.measure(&format!("{p:.1} W"), Role::Plain);
    }
    if let Some(m) = b.minutes_left {
        l.gap(2);
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
        l.push(&format!("{p:.1}"), load_role(p));
        l.push("%", Role::Dim);
        l.sep();
        bar(&mut l, p / 100.0, cells, load_role(p));
    }
    if let Some(mhz) = c.mhz {
        l.sep();
        l.measure(&format!("{:.2} GHz", f64::from(mhz) / 1000.0), Role::Plain);
    }
    if let Some(t) = c.temp_c {
        l.sep();
        l.measure(&format!("{t:.1} °C"), temp_role(t));
    }
    if let Some(w) = c.power_w {
        l.sep();
        l.measure(&format!("{w:.1} W"), Role::Plain);
    }
    if let Some([a, b, d]) = c.load {
        l.sep();
        l.push("load ", Role::Dim);
        l.plain(&format!("{a:.2} {b:.2} {d:.2}"));
    }
    if l.width > INDENT {
        out.push(l);
    }
    let n = hist
        .cpu
        .len()
        .min(HISTORY_MAX)
        .min(cap.saturating_sub(INDENT + 12));
    if n > 0 {
        let mut h = Line::body(cap, colour);
        spark(&mut h, hist.cpu.newest(n), Some(100.0));
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
    l.measure(&text::bytes(m.used), Role::Plain);
    l.push(" of ", Role::Dim);
    l.measure(&text::bytes(m.total), Role::Plain);
    l.sep();
    bar(&mut l, frac, cells, load_role(frac * 100.0));
    l.sep();
    l.push("cached ", Role::Dim);
    l.measure(&text::bytes(m.cached), Role::Plain);
    if m.swap_total > 0 {
        l.sep();
        l.push("swap ", Role::Dim);
        l.measure(&text::bytes(m.swap_used), Role::Plain);
        l.push(" of ", Role::Dim);
        l.measure(&text::bytes(m.swap_total), Role::Plain);
    }
    out.push(l);
    out
}

fn gpu_block(g: &GpuSample, cap: usize, cells: usize, colour: bool) -> Vec<Line> {
    let mut out = vec![rule(cap, colour, "GPU", &g.name, "")];
    let mut l = Line::body(cap, colour);
    if let Some(p) = g.pct {
        l.push(&format!("{p:.0}"), load_role(p));
        l.push("%", Role::Dim);
        l.sep();
        bar(&mut l, p / 100.0, cells, load_role(p));
    }
    match (g.vram_used, g.vram_total) {
        (Some(u), Some(t)) => {
            l.sep();
            l.push("VRAM ", Role::Dim);
            l.measure(&text::bytes(u), Role::Plain);
            l.push(" of ", Role::Dim);
            l.measure(&text::bytes(t), Role::Plain);
        }
        (Some(u), None) => {
            l.sep();
            l.push("VRAM ", Role::Dim);
            l.measure(&text::bytes(u), Role::Plain);
        }
        _ => {}
    }
    if let Some(t) = g.temp_c {
        l.sep();
        l.measure(&format!("{t:.0} °C"), temp_role(t));
    }
    if let Some(rpm) = g.fan_rpm {
        l.sep();
        l.measure(&format!("{rpm} rpm"), Role::Plain);
    }
    if let Some(w) = g.power_w {
        l.sep();
        l.measure(&format!("{w:.1} W"), Role::Plain);
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
    let mut out = vec![rule(cap, colour, "Disks", "", "")];
    for d in shown.iter().take(DISKS_MAX) {
        let mut l = Line::body(cap, colour);
        let mount = cut(&d.mount, 14);
        let w = mount.chars().count();
        l.plain(&mount);
        l.gap(14 - w + 2);
        l.measure(&text::bytes(d.used), Role::Plain);
        l.push(" of ", Role::Dim);
        l.measure(&text::bytes(d.total), Role::Plain);
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
                l.measure(&text::rate(Some(x)), Role::Plain);
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

/// The interfaces, at most three rows; the first one's history line (its down rate) under it.
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
        l.measure(&text::rate(n.rx_bps), Role::Plain);
        l.sep();
        l.push("↑ ", Role::Dim);
        l.measure(&text::rate(n.tx_bps), Role::Plain);
        out.push(l);
        if first == Some(n.name.as_str()) {
            let k = hist
                .down
                .len()
                .min(HISTORY_MAX)
                .min(cap.saturating_sub(INDENT));
            if k > 0 {
                let mut h = Line::body(cap, colour);
                spark(&mut h, hist.down.newest(k), None);
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

/// The sensors, hottest temperatures first, then fans, then power, as many columns as fit, at
/// most three lines, and only while the process table keeps its rows. `room` is the lines left.
fn sensor_block(sensors: &[Sensor], room: usize, cap: usize, colour: bool) -> Vec<Line> {
    if sensors.is_empty() || room < 1 + TABLE_MIN + 1 {
        return Vec::new();
    }
    let max_lines = (room - 1 - TABLE_MIN).min(SENSOR_LINES);
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
                SensorKind::Temp => (format!("{:.1} °C", s.value), temp_role(s.value)),
                SensorKind::Fan => (format!("{:.0} rpm", s.value), Role::Plain),
                SensorKind::Power => (format!("{:.1} W", s.value), Role::Plain),
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
    let per_row = ((cap + 2) / (width + 2)).max(1);
    let lines = cells.len().div_ceil(per_row).min(max_lines);
    if lines == 0 {
        return Vec::new();
    }
    let mut out = vec![rule(cap, colour, "Sensors", "", "")];
    for row in 0..lines {
        let mut l = Line::body(cap, colour);
        for (j, (name, value, role)) in cells.iter().skip(row * per_row).take(per_row).enumerate() {
            if j > 0 {
                l.gap(2);
            }
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

/// The process table: its rule, its header and the rows that fit down to the key line.
fn table(
    r: &Reading,
    v: &View,
    s: &Screen,
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
    // The name takes the rest; PID and USER go first when it would be too narrow.
    // The marker (2) and CPU 6, MEM 9, GPU 5, VRAM 9, DISK 11, each with its two spaces: 52.
    let fixed = |pid: bool, user: bool| 52 + if pid { 9 } else { 0 } + if user { 8 } else { 0 };
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
    for (i, row) in rows.iter().enumerate().skip(s.scroll).take(room) {
        let mut l = Line::new(cap, colour);
        l.plain(if s.selected == Some(i) { "▸ " } else { "  " });
        let mark = marks.get(i).map_or("", String::as_str);
        head.row(&mut l, row, mark, grouped);
        out.push(l);
    }
    out
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

    fn row(&self, l: &mut Line, r: &Row, mark: &str, grouped: bool) {
        let cpu = r
            .cpu
            .map_or_else(|| "—".to_string(), |x| format!("{x:.1}%"));
        let mem = text::bytes(r.mem);
        let gpu = r
            .gpu
            .map_or_else(|| "—".to_string(), |x| format!("{x:.0}%"));
        let vram = r.vram.map_or_else(|| "—".to_string(), text::bytes);
        let disk = match (r.read, r.write) {
            (None, None) => "—".to_string(),
            (a, b) => text::rate(Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0)))),
        };
        let fields = [cpu, mem, gpu, vram, disk];
        for (i, f) in fields.iter().enumerate() {
            let w = [6, 9, 5, 9, 11][i];
            pad_left(l, &cut(f, w), w);
        }
        if self.pid {
            let pid = r.pid.map_or_else(|| "—".to_string(), |p| p.to_string());
            pad_left(l, &pid, 7);
        }
        if self.user {
            pad_left(l, &r.uid.to_string(), 6);
        }
        let mut name = format!("{mark}{}", r.name);
        if grouped && r.count > 1 {
            name.push_str(&format!(" ×{}", r.count));
        }
        l.plain(&cut(&name, self.name_w));
    }
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
        "USER" => 6,
        _ => label.chars().count(),
    }
}

fn pad_left(l: &mut Line, text: &str, w: usize) {
    l.gap(w.saturating_sub(text.chars().count()));
    l.plain(text);
    l.gap(2);
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
        }
    }

    fn frame(r: &Reading, v: &View, s: &Screen) -> String {
        let mut hist = History::default();
        hist.push(r);
        let mut tree = Tree::new();
        let rank = |p: &Proc| f64::from(p.cpu_pct.unwrap_or(-1.0));
        let mut out = Vec::new();
        render(&mut out, r, v, s, &hist, &mut tree, &rank);
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
        assert_eq!(f.last().map(String::as_str), Some(KEY_LINE));
    }

    #[test]
    fn the_key_line_is_the_last_line_and_the_frame_is_one_screen() {
        let r = sample(4, vec![proc(1, "init", 0.0, 1 << 20)]);
        let f = frame(&r, &view(), &screen(80, 24, false));
        let lines = lines_of(&f);
        assert_eq!(lines.len(), 24);
        assert_eq!(lines[23], KEY_LINE);
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
}
