//! The text view for people: one reading, sorted and grouped as the view asks.

use crate::cli::{compare, Keys, Sort, View};
use crate::out::tree::Tree;
use aether::sys::{
    BatterySample, CpuSample, DiskSample, GpuSample, HostSample, MemSample, NetSample, Proc,
    Sensor, SensorKind,
};
use aether::Reading;
use std::io::Write;

/// Lines below the process rows that the screen keeps free: the procs line, the table's header
/// and rule, the note about rows left out, and one spare line.
const TABLE_LINES: usize = 5;

/// Appends one reading as text to `out`. A part the view does not name is left out. With
/// `screen` (the rows of a terminal) the process rows stop where the screen ends; `None` prints
/// every row.
pub fn write(
    reading: &Reading,
    view: &View,
    out: &mut Vec<u8>,
    screen: Option<usize>,
    tree: &mut Tree,
    rank: &dyn Fn(&Proc) -> f64,
) {
    let start = out.len();
    if shown(view, "host") {
        host(out, &reading.host);
    }
    if shown(view, "cpu") {
        cpu(out, &reading.cpu);
    }
    if shown(view, "mem") {
        mem(out, &reading.mem);
    }
    if shown(view, "gpu") {
        gpus(out, &reading.gpus);
    }
    if shown(view, "sensors") {
        sensors(out, &reading.sensors);
    }
    if shown(view, "disk") {
        disks(out, &reading.disks, view);
    }
    if shown(view, "net") {
        nets(out, &reading.nets, view);
    }
    if shown(view, "battery") {
        battery(out, reading.battery.as_ref());
    }
    if shown(view, "procs") {
        let above = out[start..].iter().filter(|b| **b == b'\n').count();
        let room = screen.map(|rows| rows.saturating_sub(above + TABLE_LINES));
        procs(out, &reading.procs, view, room, tree, rank);
    }
}

/// Whether `--only` names this part (no `--only` shows everything).
fn shown(view: &View, part: &str) -> bool {
    view.shows(part)
}

/// One labelled line: the label in a column of eight, then the content.
fn line(out: &mut Vec<u8>, label: &str, content: &str) {
    let _ = writeln!(out, "{label:<8}{content}");
}

fn host(out: &mut Vec<u8>, h: &HostSample) {
    let _ = writeln!(
        out,
        "aether  up {}  {} threads",
        uptime(h.uptime_s),
        h.threads
    );
}

fn cpu(out: &mut Vec<u8>, c: &CpuSample) {
    let ghz = match c.mhz {
        Some(mhz) => match (c.threads_mhz.iter().min(), c.threads_mhz.iter().max()) {
            (Some(lo), Some(hi)) => format!(
                "{:.2} GHz ({:.2} to {:.2})",
                f64::from(mhz) / 1000.0,
                f64::from(*lo) / 1000.0,
                f64::from(*hi) / 1000.0
            ),
            _ => format!("{:.2} GHz", f64::from(mhz) / 1000.0),
        },
        None => "-".to_string(),
    };
    let load = match c.load {
        Some([a, b, d]) => format!("load {a:.2} {b:.2} {d:.2}"),
        None => "load -".to_string(),
    };
    let pct = opt_f32(c.pct, 1, "%");
    let temp = opt_f32(c.temp_c, 1, " °C");
    let power = opt_f32(c.power_w, 1, " W");
    line(
        out,
        "cpu",
        &format!("{pct}  {ghz}  {temp}  {power}  {load}"),
    );
    if !c.threads.is_empty() {
        let each: Vec<String> = c.threads.iter().map(|t| format!("{t:.0}")).collect();
        line(out, "", &format!("threads {}", each.join(" ")));
    }
}

fn mem(out: &mut Vec<u8>, m: &MemSample) {
    let used = format!(
        "{} used of {}  {} available  {} cached",
        bytes(m.used),
        bytes(m.total),
        bytes(m.available),
        bytes(m.cached)
    );
    line(out, "mem", &used);
    let pressure = opt_f32(m.pressure, 1, "%");
    let swap = format!(
        "swap {} of {}  pressure {pressure}",
        bytes(m.swap_used),
        bytes(m.swap_total)
    );
    line(out, "", &swap);
}

fn gpus(out: &mut Vec<u8>, gpus: &[GpuSample]) {
    if gpus.is_empty() {
        line(out, "gpu", "-");
    }
    for (i, g) in gpus.iter().enumerate() {
        let label = if i == 0 { "gpu" } else { "" };
        let vram = match (g.vram_used, g.vram_total) {
            (Some(u), Some(t)) => format!("{} of {}", bytes(u), bytes(t)),
            (Some(u), None) => bytes(u),
            _ => "-".to_string(),
        };
        let fan = match g.fan_rpm {
            Some(rpm) => format!("{rpm} rpm"),
            None => "-".to_string(),
        };
        let pct = opt_f32(g.pct, 0, "%");
        let temp = opt_f32(g.temp_c, 0, " °C");
        let power = opt_f32(g.power_w, 1, " W");
        let content = format!("{}  {pct}  vram {vram}  {temp}  {power}  fan {fan}", g.name);
        line(out, label, &content);
    }
}

fn sensors(out: &mut Vec<u8>, sensors: &[Sensor]) {
    if sensors.is_empty() {
        line(out, "sensors", "-");
    }
    for (i, s) in sensors.iter().enumerate() {
        let label = if i == 0 { "sensors" } else { "" };
        let value = match s.kind {
            SensorKind::Temp => format!("{:.1} °C", s.value),
            SensorKind::Fan => format!("{:.0} rpm", s.value),
            SensorKind::Power => format!("{:.1} W", s.value),
        };
        line(out, label, &format!("{} {}  {value}", s.chip, s.label));
    }
}

fn disks(out: &mut Vec<u8>, disks: &[DiskSample], view: &View) {
    let mut first = true;
    for d in disks.iter().filter(|d| view.shows_disk(&d.mount)) {
        let label = if first { "disk" } else { "" };
        first = false;
        let content = format!(
            "{}  {}  {} of {}  read {}  write {}  busy {}",
            d.mount,
            d.dev,
            bytes(d.used),
            bytes(d.total),
            rate(d.read_bps),
            rate(d.write_bps),
            opt_f32(d.busy_pct, 0, "%")
        );
        line(out, label, &content);
    }
    if first {
        line(out, "disk", "-");
    }
}

fn nets(out: &mut Vec<u8>, nets: &[NetSample], view: &View) {
    let mut first = true;
    for n in nets.iter().filter(|n| view.shows_net(&n.name)) {
        let label = if first { "net" } else { "" };
        first = false;
        let mut content = format!("{}  down {}  up {}", n.name, rate(n.rx_bps), rate(n.tx_bps));
        for addr in &n.addrs {
            content.push_str("  ");
            content.push_str(addr);
        }
        line(out, label, &content);
    }
    if first {
        line(out, "net", "-");
    }
}

fn battery(out: &mut Vec<u8>, b: Option<&BatterySample>) {
    let Some(b) = b else {
        line(out, "battery", "-");
        return;
    };
    let left = match b.minutes_left {
        Some(m) => format!("{}h {:02}m left", m / 60, m % 60),
        None => "-".to_string(),
    };
    let power = opt_f32(b.power_w, 1, " W");
    line(
        out,
        "battery",
        &format!("{:.0}%  {}  {power}  {left}", b.pct, b.state),
    );
}

/// One row of the process table, before its cells are made.
pub(crate) struct Row {
    pub(crate) pid: Option<u32>,
    pub(crate) uid: u32,
    /// The newest start time of the processes in the row.
    pub(crate) started: u64,
    /// The number the numeric keys sort by: the sum for a group, missing (-1) only when all are.
    pub(crate) rank: f64,
    pub(crate) name: String,
    pub(crate) count: u32,
    pub(crate) threads: u32,
    pub(crate) cpu: Option<f32>,
    pub(crate) mem: u64,
    pub(crate) gpu: Option<f32>,
    pub(crate) vram: Option<u64>,
    pub(crate) read: Option<u64>,
    pub(crate) write: Option<u64>,
}

impl Row {
    fn of(p: &Proc, rank: f64) -> Row {
        Row {
            pid: Some(p.pid),
            uid: p.uid,
            started: p.started_s,
            rank,
            name: p.name.clone(),
            count: 1,
            threads: p.threads,
            cpu: p.cpu_pct,
            mem: p.mem,
            gpu: p.gpu_pct,
            vram: p.vram,
            read: p.read_bps,
            write: p.write_bps,
        }
    }

    fn keys(&self) -> Keys<'_> {
        Keys {
            pid: self.pid.unwrap_or(0),
            uid: self.uid,
            threads: self.threads,
            started: self.started,
            name: &self.name,
            rank: self.rank,
        }
    }

    /// Adds another process of the same name to this group: the lowest pid stays, the uid is
    /// the first member's, and the start is the newest.
    fn absorb(&mut self, o: &Row) {
        self.pid = self.pid.min(o.pid);
        self.started = self.started.max(o.started);
        self.rank = match (self.rank < 0.0, o.rank < 0.0) {
            (true, _) => o.rank,
            (false, true) => self.rank,
            (false, false) => self.rank + o.rank,
        };
        self.count += o.count;
        self.threads += o.threads;
        self.cpu = add(self.cpu, o.cpu);
        self.mem += o.mem;
        self.gpu = add(self.gpu, o.gpu);
        self.vram = add(self.vram, o.vram);
        self.read = add(self.read, o.read);
        self.write = add(self.write, o.write);
    }
}

/// Adds two readings; a missing one counts as nothing, and two missing stay missing.
fn add<T: std::ops::Add<Output = T> + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// The key's name, as the header and the full-screen rule say it.
pub(crate) fn sort_word(by: Sort) -> &'static str {
    match by {
        Sort::Cpu => "cpu",
        Sort::Mem => "memory",
        Sort::Gpu => "gpu",
        Sort::Disk => "disk",
        Sort::Pid => "pid",
        Sort::User => "user",
        Sort::Threads => "threads",
        Sort::Name => "name",
        Sort::Age => "age",
    }
}

fn sort_name(view: &View) -> String {
    let name = sort_word(view.by);
    match (view.reverse, view.steady && view.by.smooths()) {
        (false, false) => name.to_string(),
        (true, false) => format!("{name}, reversed"),
        (false, true) => format!("{name}, steady"),
        (true, true) => format!("{name}, steady, reversed"),
    }
}

/// Merges neighbouring rows with the same name, after sorting them by name.
fn group(mut rows: Vec<Row>) -> Vec<Row> {
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<Row> = Vec::new();
    for r in rows {
        match out.last_mut() {
            Some(last) if last.name == r.name => last.absorb(&r),
            _ => out.push(r),
        }
    }
    out
}

/// The rows of the process table in the order the view asks for, and the tree's marks (one
/// per row, only under `--tree`). Grouped by app unless `--flat` or `--tree`.
pub(crate) fn process_rows(
    procs: &[Proc],
    view: &View,
    rank: &dyn Fn(&Proc) -> f64,
    tree: &mut Tree,
) -> (Vec<Row>, Vec<String>) {
    let grouped = !view.flat && !view.tree;
    let mut rows: Vec<Row> = procs.iter().map(|p| Row::of(p, rank(p))).collect();
    if grouped {
        rows = group(rows);
    }
    // A tree is single processes, so rows[i] is procs[i] here and the tree can order by index.
    let mut marks: Vec<String> = Vec::new();
    if view.tree {
        tree.build(procs, |a, b| {
            compare(view, &rows[a].keys(), &rows[b].keys())
        });
        rows = tree
            .rows()
            .iter()
            .map(|r| {
                let mut mark = String::new();
                tree.draw(r, &mut mark);
                marks.push(mark);
                Row::of(&procs[r.index], rank(&procs[r.index]))
            })
            .collect();
    } else {
        rows.sort_by(|a, b| compare(view, &a.keys(), &b.keys()));
    }
    (rows, marks)
}

fn procs(
    out: &mut Vec<u8>,
    procs: &[Proc],
    view: &View,
    room: Option<usize>,
    tree: &mut Tree,
    rank: &dyn Fn(&Proc) -> f64,
) {
    let grouped = !view.flat && !view.tree;
    let (mut rows, mut marks) = process_rows(procs, view, rank, tree);
    let total = rows.len();
    if let Some(room) = room {
        rows.truncate(room);
        marks.truncate(room);
    }
    let head: Vec<String> = if grouped {
        [
            "NAME", "COUNT", "CPU", "MEM", "GPU", "VRAM", "READ/s", "WRITE/s",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    } else {
        [
            "PID", "NAME", "THR", "CPU", "MEM", "GPU", "VRAM", "READ/s", "WRITE/s",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    };
    let body: Vec<Vec<String>> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| cells(r, grouped, marks.get(i).map_or("", String::as_str)))
        .collect();
    let how = if grouped {
        "grouped by app"
    } else if view.tree {
        "as a tree"
    } else {
        "single processes"
    };
    line(out, "procs", &format!("by {}, {how}", sort_name(view)));
    let left = if grouped { 0 } else { 1 };
    table(out, &head, &body, left);
    if rows.len() < total {
        let note = format!(
            "{} more rows past the screen; --all shows every row\n",
            total - rows.len()
        );
        out.extend_from_slice(note.as_bytes());
    }
}

/// The cells of one row, in the order of the table's header.
fn cells(r: &Row, grouped: bool, mark: &str) -> Vec<String> {
    let cpu = opt_f32(r.cpu, 1, "");
    let gpu = opt_f32(r.gpu, 1, "");
    let vram = opt_bytes(r.vram);
    let read = opt_bytes(r.read);
    let write = opt_bytes(r.write);
    let mem = bytes(r.mem);
    if grouped {
        vec![
            r.name.clone(),
            r.count.to_string(),
            cpu,
            mem,
            gpu,
            vram,
            read,
            write,
        ]
    } else {
        let pid = r.pid.map_or_else(|| "-".to_string(), |p| p.to_string());
        vec![
            pid,
            format!("{mark}{}", r.name),
            r.threads.to_string(),
            cpu,
            mem,
            gpu,
            vram,
            read,
            write,
        ]
    }
}

/// Writes a table: each column as wide as its widest cell, the `left` column left-aligned.
fn table(out: &mut Vec<u8>, head: &[String], rows: &[Vec<String>], left: usize) {
    let mut width: Vec<usize> = head.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (j, cell) in row.iter().enumerate() {
            if let Some(w) = width.get_mut(j) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    row_line(out, head, &width, left);
    for row in rows {
        row_line(out, row, &width, left);
    }
}

fn row_line(out: &mut Vec<u8>, cells: &[String], width: &[usize], left: usize) {
    let mut s = String::new();
    for (j, cell) in cells.iter().enumerate() {
        if j > 0 {
            s.push_str("  ");
        }
        let short = width
            .get(j)
            .copied()
            .unwrap_or(0)
            .saturating_sub(cell.chars().count());
        let fill = " ".repeat(short);
        if j == left {
            s.push_str(cell);
            s.push_str(&fill);
        } else {
            s.push_str(&fill);
            s.push_str(cell);
        }
    }
    let _ = writeln!(out, "{}", s.trim_end());
}

/// A number with a fixed number of decimals and a unit, or `-` when missing.
fn opt_f32(v: Option<f32>, decimals: usize, unit: &str) -> String {
    match v {
        Some(x) => format!("{x:.decimals$}{unit}"),
        None => "-".to_string(),
    }
}

fn opt_bytes(v: Option<u64>) -> String {
    v.map_or_else(|| "-".to_string(), bytes)
}

pub(crate) fn rate(v: Option<u64>) -> String {
    match v {
        Some(b) => format!("{}/s", bytes(b)),
        None => "-".to_string(),
    }
}

/// A byte count for people: "512 B", "1.5 KiB", "31.3 GiB".
pub(crate) fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let (v, i) = scale(n, 999.95);
    if i == 0 {
        return format!("{n} B");
    }
    format!("{v:.1} {}", UNITS[i])
}

/// A size as a number and the index of its unit (B, KiB, MiB, GiB, TiB). A number from `limit`
/// up is the next unit's, so a size never shows as "1000.0 KiB"; `limit` is the smallest value
/// that is written with four digits.
pub(crate) fn scale(n: u64, limit: f64) -> (f64, usize) {
    let mut v = n as f64;
    let mut i = 0;
    while (v >= 1024.0 || v >= limit) && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    (v, i)
}

/// Seconds as "3d 04h 12m", "1h 01m" or "12m 34s".
pub(crate) fn uptime(s: u64) -> String {
    let d = s / 86_400;
    let h = (s % 86_400) / 3_600;
    let m = (s % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h:02}h {m:02}m")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m {:02}s", s % 60)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn view(only: Option<&[&str]>, flat: bool, by: Sort) -> View {
        View {
            disks: None,
            nets: None,
            by,
            reverse: false,
            steady: false,
            filter: None,
            flat,
            tree: false,
            all: false,
            only: only.map(|l| l.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn proc(pid: u32, name: &str, cpu: f32, mem: u64) -> Proc {
        Proc {
            pid,
            ppid: 1,
            uid: 1000,
            threads: 2,
            name: name.to_string(),
            state: b'S',
            cpu_pct: Some(cpu),
            gpu_pct: None,
            mem,
            rss: mem,
            started_s: 0,
            vram: None,
            read_bps: None,
            write_bps: None,
        }
    }

    fn render(r: &Reading, v: &View) -> String {
        let mut out = Vec::new();
        let by = v.by;
        let rank = |p: &Proc| match by {
            Sort::Mem => p.mem as f64,
            _ => f64::from(p.cpu_pct.unwrap_or(-1.0)),
        };
        write(r, v, &mut out, None, &mut Tree::new(), &rank);
        String::from_utf8(out).unwrap_or_default()
    }

    #[test]
    fn byte_counts_and_uptime_read_for_people() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1.0 KiB");
        assert_eq!(bytes(999 * 1024 + 970), "999.9 KiB");
        assert_eq!(bytes(1000 * 1024), "1.0 MiB");
        assert_eq!(bytes((1023.99 * 1024.0) as u64), "1.0 MiB");
        assert_eq!(bytes(1000 * 1024 * 1024), "1.0 GiB");
        assert_eq!(bytes(1000 * 1024 * 1024 * 1024), "1.0 TiB");
        assert_eq!(bytes(5000 * 1024 * 1024 * 1024 * 1024), "5000.0 TiB");
        assert_eq!(bytes(1 << 30), "1.0 GiB");
        assert_eq!(bytes(3 << 40), "3.0 TiB");
        assert_eq!(uptime(754), "12m 34s");
        assert_eq!(uptime(3_600 + 60), "1h 01m");
        assert_eq!(uptime(3 * 86_400 + 4 * 3_600 + 12 * 60), "3d 04h 12m");
    }

    #[test]
    fn host_and_cpu_lines_match_exactly() {
        let r = Reading {
            host: HostSample {
                uptime_s: 754,
                threads: 8,
            },
            cpu: CpuSample {
                threads_mhz: vec![],
                power_w: None,
                pct: Some(42.5),
                threads: vec![10.0, 75.0],
                mhz: Some(3200),
                temp_c: Some(61.0),
                load: Some([1.0, 0.5, 0.25]),
            },
            ..Reading::default()
        };
        let text = render(&r, &view(Some(&["host", "cpu"]), false, Sort::Cpu));
        assert_eq!(
            text,
            "aether  up 12m 34s  8 threads\n\
             cpu     42.5%  3.20 GHz  61.0 °C  -  load 1.00 0.50 0.25\n\
             \x20       threads 10 75\n"
        );
    }

    #[test]
    fn missing_readings_show_a_dash_not_zero() {
        let r = Reading::default();
        let text = render(
            &r,
            &view(Some(&["cpu", "battery", "gpu"]), false, Sort::Cpu),
        );
        assert!(text.contains("cpu     -  -  -  -  load -\n"), "{text}");
        assert!(text.contains("battery -\n"), "{text}");
        assert!(text.contains("gpu     -\n"), "{text}");
    }

    #[test]
    fn processes_are_grouped_by_app_and_sorted() {
        let r = Reading {
            procs: vec![
                proc(10, "firefox", 5.0, 100),
                proc(11, "firefox", 1.5, 50),
                proc(20, "bash", 9.0, 10),
            ],
            ..Reading::default()
        };
        let text = render(&r, &view(Some(&["procs"]), false, Sort::Cpu));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "procs   by cpu, grouped by app");
        let head: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(
            head,
            ["NAME", "COUNT", "CPU", "MEM", "GPU", "VRAM", "READ/s", "WRITE/s"]
        );
        let bash: Vec<&str> = lines[2].split_whitespace().collect();
        assert_eq!(bash, ["bash", "1", "9.0", "10", "B", "-", "-", "-", "-"]);
        let ff: Vec<&str> = lines[3].split_whitespace().collect();
        assert_eq!(ff, ["firefox", "2", "6.5", "150", "B", "-", "-", "-", "-"]);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn a_tree_orders_siblings_by_the_view_and_a_missing_parent_is_a_root() {
        let kids = vec![
            proc(5, "alpha", 1.0, 1),
            Proc {
                ppid: 5,
                ..proc(6, "beta", 1.0, 1)
            },
            Proc {
                ppid: 5,
                ..proc(7, "gamma", 1.0, 1)
            },
            Proc {
                ppid: 99,
                ..proc(8, "delta", 1.0, 1)
            },
        ];
        let places = |reverse: bool| {
            let mut v = view(None, false, Sort::Name);
            v.tree = true;
            v.reverse = reverse;
            let r = Reading {
                procs: kids.clone(),
                ..Reading::default()
            };
            let text = render(&r, &v);
            ["alpha", "beta", "gamma", "delta"].map(|n| text.find(n).unwrap_or(usize::MAX))
        };
        let (fwd, rev) = (places(false), places(true));
        assert!(fwd.iter().all(|&at| at != usize::MAX));
        assert_eq!(fwd[1] < fwd[2], !(rev[1] < rev[2]));
    }

    #[test]
    fn flat_processes_sort_by_memory() {
        let r = Reading {
            procs: vec![proc(10, "a", 9.0, 100), proc(11, "b", 1.0, 900)],
            ..Reading::default()
        };
        let text = render(&r, &view(Some(&["procs"]), true, Sort::Mem));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "procs   by memory, single processes");
        let first: Vec<&str> = lines[2].split_whitespace().collect();
        assert_eq!(first[0], "11");
        assert_eq!(first[1], "b");
    }

    #[test]
    fn group_sums_what_the_processes_use() {
        let mut a = Row::of(&proc(1, "x", 2.0, 10), 2.0);
        let b = Row::of(&proc(2, "x", 3.0, 20), 3.0);
        a.absorb(&b);
        assert_eq!(a.count, 2);
        assert_eq!(a.cpu, Some(5.0));
        assert_eq!(a.mem, 30);
        assert_eq!(a.gpu, None);
    }
}
