//! One JSON object per reading, every process included. A missing value is `null`.

use crate::cli::View;
use crate::out::mask::mask_secrets;
use aether::sys::{
    BatterySample, CpuSample, DiskSample, GpuSample, HostSample, MemSample, NetSample, Proc,
    ProcDetail, Sensor, SensorKind, ThreadRow,
};
use aether::Reading;
use std::io::Write;

/// The version of the JSON shape; every object starts with it as `"schema"`. It goes up when a
/// field is removed or changes meaning, never for an added field.
pub const SCHEMA: u32 = 1;

/// Appends one reading as one JSON object and a newline to `out`. `t` is always there; each
/// other part is there when `--only` names it (the same names as the text form).
pub fn write(reading: &Reading, view: &View, out: &mut Vec<u8>) {
    write_with(reading, view, out, false);
}

/// One line of a recording: like `write`, but the numbers have at most two decimals and, unless
/// `full`, each process is only its pid, name, CPU and memory (what `peaks` reads).
pub fn write_record(reading: &Reading, view: &View, out: &mut Vec<u8>, full: bool) {
    let from = out.len();
    write_with(reading, view, out, !full);
    let line = out.split_off(from);
    shorten(&line, out);
}

/// Copies a JSON line, writing each number with a fraction as at most two decimals.
fn shorten(line: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    let mut quoted = false;
    while i < line.len() {
        let c = line[i];
        if quoted {
            out.push(c);
            i += 1;
            if c == b'\\' && i < line.len() {
                out.push(line[i]);
                i += 1;
            } else if c == b'"' {
                quoted = false;
            }
        } else if c == b'"' {
            quoted = true;
            out.push(c);
            i += 1;
        } else if c.is_ascii_digit() || c == b'-' {
            let end = line[i..]
                .iter()
                .position(|b| {
                    !(b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'e' | b'E' | b'+'))
                })
                .map_or(line.len(), |n| i + n);
            let token = &line[i..end];
            let short = std::str::from_utf8(token)
                .ok()
                .filter(|t| {
                    t.bytes().filter(|&b| b == b'.').count() == 1 && !t.contains(['e', 'E'])
                })
                .and_then(|t| t.split_once('.'))
                .filter(|(_, f)| f.len() > 2)
                .and_then(|(w, f)| format!("{w}.{f}").parse::<f64>().ok());
            match short {
                Some(v) => {
                    let t = format!("{v:.2}");
                    let t = t.trim_end_matches('0').trim_end_matches('.');
                    out.extend_from_slice(if t == "-0" { "0" } else { t }.as_bytes());
                }
                None => out.extend_from_slice(token),
            }
            i = end;
        } else {
            out.push(c);
            i += 1;
        }
    }
}

/// `write`, with `slim` processes when asked.
fn write_with(reading: &Reading, view: &View, out: &mut Vec<u8>, slim: bool) {
    let mut o = Obj::open(out);
    o.int("schema", Some(u64::from(SCHEMA)));
    o.key("t");
    float64(o.out, reading.t);
    if view.shows("cpu") {
        o.key("cpu");
        cpu(o.out, &reading.cpu);
    }
    if view.shows("mem") {
        o.key("mem");
        mem(o.out, &reading.mem);
    }
    if view.shows("gpu") {
        o.key("gpus");
        array(o.out, &reading.gpus, gpu);
    }
    if view.shows("sensors") {
        o.key("sensors");
        array(o.out, &reading.sensors, sensor);
    }
    if view.shows("disk") {
        o.key("disks");
        array_of(
            o.out,
            reading.disks.iter().filter(|d| view.shows_disk(&d.mount)),
            disk,
        );
    }
    if view.shows("net") {
        o.key("nets");
        array_of(
            o.out,
            reading.nets.iter().filter(|n| view.shows_net(&n.name)),
            net,
        );
    }
    if view.shows("battery") {
        o.key("battery");
        match &reading.battery {
            Some(b) => battery(o.out, b),
            None => put(o.out, "null"),
        }
    }
    if view.shows("host") {
        o.key("host");
        host(o.out, &reading.host);
    }
    if view.shows("procs") {
        o.key("procs");
        if slim {
            array(o.out, &reading.procs, proc_slim);
        } else {
            array(o.out, &reading.procs, proc_);
        }
    }
    o.close();
    put(out, "\n");
}

/// The rows of `aether port` as one object; `hidden` says some process could not be seen.
pub fn write_ports(rows: &[crate::out::ports::PortRow], hidden: bool, out: &mut Vec<u8>) {
    let mut o = Obj::open(out);
    o.int("schema", Some(u64::from(SCHEMA)));
    o.key("sockets");
    array(o.out, rows, |out, r| {
        let mut o = Obj::open(out);
        o.text("proto", r.proto);
        o.text("ip", r.ip);
        o.text("local", &r.local);
        o.int("port", Some(u64::from(r.port)));
        o.key("remote");
        match &r.remote {
            Some(a) => string(o.out, a),
            None => put(o.out, "null"),
        }
        o.int("remote_port", r.remote_port.map(u64::from));
        o.text("state", r.state);
        o.int("pid", r.pid.map(u64::from));
        for (k, v) in [("process", &r.name), ("user", &r.user)] {
            o.key(k);
            match v {
                Some(t) => string(o.out, t),
                None => put(o.out, "null"),
            }
        }
        o.close();
    });
    o.key("hidden");
    put(o.out, if hidden { "true" } else { "false" });
    o.close();
    put(out, "\n");
}

/// One process for `proc --json`: what `proc` shows, read by the caller.
pub struct ProcJson {
    pub pid: u32,
    /// The name the process has; `None` when it is not known.
    pub name: Option<String>,
    /// The CPU % of the whole machine, as the reading's process list has it.
    pub cpu_pct: Option<f32>,
    pub detail: ProcDetail,
    /// The threads with their CPU %; `None` when the thread list could not be read.
    pub threads: Option<Vec<ThreadRow>>,
}

/// `proc --json`: one object for one process, or a JSON array of them when `array` is set.
pub fn write_procs(procs: &[ProcJson], array: bool, out: &mut Vec<u8>) {
    if array {
        out.push(b'[');
    }
    for (i, p) in procs.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        proc_detail(p, out);
    }
    if array {
        out.push(b']');
    }
    put(out, "\n");
}

fn proc_detail(p: &ProcJson, out: &mut Vec<u8>) {
    let d = &p.detail;
    let mut o = Obj::open(out);
    o.int("schema", Some(u64::from(SCHEMA)));
    o.int("pid", Some(u64::from(p.pid)));
    opt_text(&mut o, "name", p.name.as_deref());
    o.f32("cpu_pct", p.cpu_pct);
    let cmdline = d.cmdline.as_deref().map(mask_secrets);
    opt_text(&mut o, "cmdline", cmdline.as_deref());
    opt_text(&mut o, "exe", d.exe.as_deref());
    opt_text(&mut o, "cwd", d.cwd.as_deref());
    o.key("parents");
    array_of(o.out, d.parents.iter(), parent_each);
    o.key("cpu_s");
    float64(o.out, d.cpu_s);
    o.int(
        "threads",
        p.threads.as_ref().and_then(|r| u64::try_from(r.len()).ok()),
    );
    o.key("thread_list");
    match &p.threads {
        Some(rows) => array(o.out, rows.as_slice(), thread_each),
        None => put(o.out, "null"),
    }
    o.int("open_files", d.open_files.map(u64::from));
    o.close();
}

fn parent_each(out: &mut Vec<u8>, p: &(u32, String)) {
    let mut o = Obj::open(out);
    o.int("pid", Some(u64::from(p.0)));
    o.text("name", &p.1);
    o.close();
}

fn thread_each(out: &mut Vec<u8>, r: &ThreadRow) {
    let mut o = Obj::open(out);
    o.int("tid", Some(u64::from(r.thread.tid)));
    opt_text(&mut o, "name", r.thread.name.as_deref());
    o.key("state");
    match r.thread.state {
        Some(s) => string(o.out, &char::from(s).to_string()),
        None => put(o.out, "null"),
    }
    o.f32("cpu", r.cpu_pct);
    o.close();
}

/// A key with a text value, or `null`.
fn opt_text(o: &mut Obj<'_>, k: &str, v: Option<&str>) {
    o.key(k);
    match v {
        Some(t) => string(o.out, t),
        None => put(o.out, "null"),
    }
}

/// `kill --json`: the target as given, the signal's name, and one result per process: its pid,
/// its name (`null` if not known), `ok`, and the reason it was not ended (`error`, else `null`).
pub fn write_kill(
    target: &str,
    signal: &str,
    rows: &[(u32, String, Option<String>)],
    out: &mut Vec<u8>,
) {
    let mut o = Obj::open(out);
    o.int("schema", Some(u64::from(SCHEMA)));
    o.text("target", target);
    o.text("signal", signal);
    o.key("results");
    array(o.out, rows, kill_result);
    o.close();
    put(out, "\n");
}

fn kill_result(out: &mut Vec<u8>, r: &(u32, String, Option<String>)) {
    let mut o = Obj::open(out);
    o.int("pid", Some(u64::from(r.0)));
    opt_text(&mut o, "name", (!r.1.is_empty()).then_some(r.1.as_str()));
    o.key("ok");
    put(o.out, if r.2.is_none() { "true" } else { "false" });
    opt_text(&mut o, "error", r.2.as_deref());
    o.close();
}

/// `alert --json`: one line as one object. `state` is `fired`, `cleared` or `holds` (for
/// `--once`); `value` is the number the rule looked at, or `null`.
pub fn write_alert(time: u64, rule: &str, state: &str, value: Option<f64>, out: &mut Vec<u8>) {
    let mut o = Obj::open(out);
    o.int("schema", Some(u64::from(SCHEMA)));
    o.int("time", Some(time));
    o.text("rule", rule);
    o.text("state", state);
    o.key("value");
    match value {
        Some(v) => float64(o.out, v),
        None => put(o.out, "null"),
    }
    o.close();
    put(out, "\n");
}

/// Writes raw text into the buffer.
fn put(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// A JSON string, with every character that must be escaped escaped.
pub(crate) fn string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => put(out, "\\\""),
            '\\' => put(out, "\\\\"),
            '\n' => put(out, "\\n"),
            '\r' => put(out, "\\r"),
            '\t' => put(out, "\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => {
                let mut buf = [0u8; 4];
                put(out, c.encode_utf8(&mut buf));
            }
        }
    }
    out.push(b'"');
}

/// A number, or `null` when it is not finite (JSON has no NaN or infinity).
fn float64(out: &mut Vec<u8>, v: f64) {
    if v.is_finite() {
        let _ = write!(out, "{v}");
    } else {
        put(out, "null");
    }
}

fn float32(out: &mut Vec<u8>, v: Option<f32>) {
    match v {
        Some(x) if x.is_finite() => {
            let _ = write!(out, "{x}");
        }
        _ => put(out, "null"),
    }
}

fn int(out: &mut Vec<u8>, v: Option<u64>) {
    match v {
        Some(x) => {
            let _ = write!(out, "{x}");
        }
        None => put(out, "null"),
    }
}

/// A JSON object being written, one key at a time.
struct Obj<'a> {
    out: &'a mut Vec<u8>,
    n: usize,
}

impl Obj<'_> {
    fn open(out: &mut Vec<u8>) -> Obj<'_> {
        out.push(b'{');
        Obj { out, n: 0 }
    }

    /// Writes the separator and `"key":`; the value comes next.
    fn key(&mut self, k: &str) {
        if self.n > 0 {
            self.out.push(b',');
        }
        self.n += 1;
        string(self.out, k);
        self.out.push(b':');
    }

    /// A field with a value that may be missing.
    fn f32(&mut self, k: &str, v: Option<f32>) {
        self.key(k);
        float32(self.out, v);
    }

    fn int(&mut self, k: &str, v: Option<u64>) {
        self.key(k);
        int(self.out, v);
    }

    fn text(&mut self, k: &str, v: &str) {
        self.key(k);
        string(self.out, v);
    }

    fn close(self) {
        self.out.push(b'}');
    }
}

/// Writes a list of items, each by `each`.
fn array<T>(out: &mut Vec<u8>, items: &[T], each: fn(&mut Vec<u8>, &T)) {
    out.push(b'[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        each(out, item);
    }
    out.push(b']');
}

fn cpu(out: &mut Vec<u8>, c: &CpuSample) {
    let mut o = Obj::open(out);
    o.f32("pct", c.pct);
    o.key("threads");
    array_f32(o.out, &c.threads);
    o.key("threads_mhz");
    array_of(o.out, c.threads_mhz.iter(), mhz_each);
    o.int("mhz", c.mhz.map(u64::from));
    o.f32("temp_c", c.temp_c);
    o.f32("power_w", c.power_w);
    o.key("load");
    match c.load {
        Some(l) => array_f32(o.out, &l),
        None => put(o.out, "null"),
    }
    o.close();
}

/// Writes a JSON array from any run of items, one `each` call per item.
fn array_of<'a, T: 'a>(
    out: &mut Vec<u8>,
    items: impl Iterator<Item = &'a T>,
    each: fn(&mut Vec<u8>, &T),
) {
    out.push(b'[');
    for (i, item) in items.enumerate() {
        if i > 0 {
            out.push(b',');
        }
        each(out, item);
    }
    out.push(b']');
}

fn mhz_each(out: &mut Vec<u8>, m: &u32) {
    int(out, Some(u64::from(*m)));
}

fn addr_each<T: AsRef<str>>(out: &mut Vec<u8>, a: &T) {
    string(out, a.as_ref());
}

fn array_f32(out: &mut Vec<u8>, items: &[f32]) {
    out.push(b'[');
    for (i, v) in items.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        float32(out, Some(*v));
    }
    out.push(b']');
}

fn mem(out: &mut Vec<u8>, m: &MemSample) {
    let mut o = Obj::open(out);
    o.int("total", Some(m.total));
    o.int("used", Some(m.used));
    o.int("available", Some(m.available));
    o.int("cached", Some(m.cached));
    o.int("swap_total", Some(m.swap_total));
    o.int("swap_used", Some(m.swap_used));
    o.f32("pressure", m.pressure);
    o.close();
}

fn gpu(out: &mut Vec<u8>, g: &GpuSample) {
    let mut o = Obj::open(out);
    o.text("name", &g.name);
    o.text("vendor", g.vendor);
    o.f32("pct", g.pct);
    o.f32("temp_c", g.temp_c);
    o.f32("power_w", g.power_w);
    o.int("vram_used", g.vram_used);
    o.int("vram_total", g.vram_total);
    o.int("fan_rpm", g.fan_rpm.map(u64::from));
    o.close();
}

fn sensor(out: &mut Vec<u8>, s: &Sensor) {
    let kind = match s.kind {
        SensorKind::Temp => "temp",
        SensorKind::Fan => "fan",
        SensorKind::Power => "power",
    };
    let mut o = Obj::open(out);
    o.text("chip", &s.chip);
    o.text("label", &s.label);
    o.text("kind", kind);
    o.f32("value", Some(s.value));
    o.close();
}

fn disk(out: &mut Vec<u8>, d: &DiskSample) {
    let mut o = Obj::open(out);
    o.text("mount", &d.mount);
    o.text("dev", &d.dev);
    o.int("total", Some(d.total));
    o.int("used", Some(d.used));
    o.int("read_bps", d.read_bps);
    o.int("write_bps", d.write_bps);
    o.f32("busy_pct", d.busy_pct);
    o.close();
}

fn net(out: &mut Vec<u8>, n: &NetSample) {
    let mut o = Obj::open(out);
    o.text("name", &n.name);
    o.int("rx_bps", n.rx_bps);
    o.int("tx_bps", n.tx_bps);
    o.int("link_bps", n.link_bps);
    o.key("addrs");
    array_of(o.out, n.addrs.iter(), addr_each);
    o.close();
}

fn battery(out: &mut Vec<u8>, b: &BatterySample) {
    let mut o = Obj::open(out);
    o.f32("pct", Some(b.pct));
    o.text("state", b.state);
    o.f32("power_w", b.power_w);
    o.int("minutes_left", b.minutes_left.map(u64::from));
    o.close();
}

fn host(out: &mut Vec<u8>, h: &HostSample) {
    let mut o = Obj::open(out);
    o.int("uptime_s", Some(h.uptime_s));
    o.int("threads", Some(u64::from(h.threads)));
    o.close();
}

fn proc_(out: &mut Vec<u8>, p: &Proc) {
    let mut o = Obj::open(out);
    o.int("pid", Some(u64::from(p.pid)));
    o.int("ppid", Some(u64::from(p.ppid)));
    o.int("uid", Some(u64::from(p.uid)));
    o.int("threads", Some(u64::from(p.threads)));
    o.text("name", &p.name);
    o.text("state", &char::from(p.state).to_string());
    o.f32("cpu_pct", p.cpu_pct);
    o.f32("gpu_pct", p.gpu_pct);
    o.int("mem", Some(p.mem));
    o.int("rss", Some(p.rss));
    o.int("started_s", Some(p.started_s));
    o.int("vram", p.vram);
    o.int("read_bps", p.read_bps);
    o.int("write_bps", p.write_bps);
    o.close();
}

/// The part of a process a recording keeps.
fn proc_slim(out: &mut Vec<u8>, p: &Proc) {
    let mut o = Obj::open(out);
    o.int("pid", Some(u64::from(p.pid)));
    o.text("name", &p.name);
    o.f32("cpu_pct", p.cpu_pct);
    o.int("mem", Some(p.mem));
    o.close();
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cli::Sort;

    fn render(r: &Reading) -> String {
        render_with(r, None)
    }

    fn render_with(r: &Reading, only: Option<&[&str]>) -> String {
        let view = View {
            disks: None,
            nets: None,
            by: Sort::Cpu,
            reverse: false,
            steady: false,
            filter: None,
            flat: false,
            tree: false,
            all: false,
            only: only.map(|l| l.iter().map(|s| s.to_string()).collect()),
        };
        let mut out = Vec::new();
        write(r, &view, &mut out);
        String::from_utf8(out).unwrap_or_default()
    }

    #[test]
    fn only_leaves_out_the_parts_it_does_not_name() {
        let text = render_with(&Reading::default(), Some(&["cpu", "mem"]));
        assert!(text.starts_with("{\"schema\":1,\"t\":"));
        assert!(text.contains("\"cpu\":{"));
        assert!(text.contains("\"mem\":{"));
        assert!(!text.contains("\"gpus\""));
        assert!(!text.contains("\"procs\""));
        assert!(!text.contains("\"host\""));
    }

    #[test]
    fn an_empty_reading_is_nulls_and_zeros() {
        assert_eq!(
            render(&Reading::default()),
            "{\"schema\":1,\"t\":0,\"cpu\":{\"pct\":null,\"threads\":[],\"threads_mhz\":[],\"mhz\":null,\"temp_c\":null,\"power_w\":null,\"load\":null},\
             \"mem\":{\"total\":0,\"used\":0,\"available\":0,\"cached\":0,\"swap_total\":0,\
             \"swap_used\":0,\"pressure\":null},\"gpus\":[],\"sensors\":[],\"disks\":[],\
             \"nets\":[],\"battery\":null,\"host\":{\"uptime_s\":0,\"threads\":0},\"procs\":[]}\n"
        );
    }

    #[test]
    fn values_are_written_and_missing_ones_are_null() {
        let p = Proc {
            pid: 42,
            ppid: 1,
            uid: 1000,
            threads: 3,
            name: "a\"b\\c".to_string(),
            state: b'S',
            cpu_pct: Some(f32::NAN),
            gpu_pct: None,
            mem: 2048,
            rss: 4096,
            started_s: 7,
            vram: None,
            read_bps: Some(10),
            write_bps: None,
        };
        let r = Reading {
            t: 1.5,
            cpu: CpuSample {
                pct: Some(12.5),
                threads: vec![],
                mhz: None,
                temp_c: None,
                load: Some([1.0, 0.5, 0.25]),
                threads_mhz: vec![],
                power_w: None,
            },
            sensors: vec![Sensor {
                chip: "k10temp".to_string(),
                label: "Tctl".to_string(),
                kind: SensorKind::Temp,
                value: 61.5,
            }],
            battery: Some(BatterySample {
                pct: 80.0,
                state: "discharging",
                power_w: None,
                minutes_left: Some(42),
            }),
            procs: vec![p],
            ..Reading::default()
        };
        let text = render(&r);
        assert!(
            text.starts_with("{\"schema\":1,\"t\":1.5,\"cpu\":{\"pct\":12.5,"),
            "{text}"
        );
        assert!(text.contains("\"load\":[1,0.5,0.25]"), "{text}");
        assert!(
            text.contains("\"sensors\":[{\"chip\":\"k10temp\",\"label\":\"Tctl\",\"kind\":\"temp\",\"value\":61.5}]"),
            "{text}"
        );
        assert!(
            text.contains("\"battery\":{\"pct\":80,\"state\":\"discharging\",\"power_w\":null,\"minutes_left\":42}"),
            "{text}"
        );
        assert!(
            text.contains("{\"pid\":42,\"ppid\":1,\"uid\":1000,\"threads\":3,\"name\":\"a\\\"b\\\\c\",\"state\":\"S\",\"cpu_pct\":null,\"gpu_pct\":null,"),
            "{text}"
        );
        assert!(
            text.contains("\"vram\":null,\"read_bps\":10,\"write_bps\":null}]"),
            "{text}"
        );
        assert!(text.ends_with("}\n"));
        assert_eq!(text.matches('\n').count(), 1);
    }

    #[test]
    fn control_characters_are_escaped() {
        let mut out = Vec::new();
        string(&mut out, "a\u{1}\tb");
        assert_eq!(
            String::from_utf8(out).unwrap_or_default(),
            "\"a\\u0001\\tb\""
        );
    }

    #[test]
    fn a_recorded_line_has_short_numbers_and_slim_processes() {
        let mut r = Reading::default();
        r.cpu.pct = Some(27.777_779);
        r.cpu.threads = vec![0.5, 11.764_706];
        r.t = 0.350_743_552;
        r.procs.push(Proc {
            pid: 7,
            ppid: 1,
            uid: 0,
            threads: 1,
            name: "a\"1.23456\"".into(),
            state: b'S',
            cpu_pct: Some(1.234_5),
            gpu_pct: None,
            mem: 5,
            rss: 5,
            started_s: 0,
            vram: None,
            read_bps: None,
            write_bps: None,
        });
        let view = View {
            disks: None,
            nets: None,
            by: Sort::Cpu,
            reverse: false,
            steady: false,
            filter: None,
            flat: false,
            tree: false,
            all: false,
            only: None,
        };
        let mut slim = Vec::new();
        write_record(&r, &view, &mut slim, false);
        let text = String::from_utf8(slim).unwrap();
        assert!(text.contains("\"t\":0.35,"), "{text}");
        assert!(text.contains("27.78"), "{text}");
        assert!(text.contains("[0.5,11.76]"), "{text}");
        assert!(
            text.contains(
                "\"procs\":[{\"pid\":7,\"name\":\"a\\\"1.23456\\\"\",\"cpu_pct\":1.23,\"mem\":5}]"
            ),
            "{text}"
        );
        let mut full = Vec::new();
        write_record(&r, &view, &mut full, true);
        let text = String::from_utf8(full).unwrap();
        assert!(text.contains("\"ppid\":1"), "{text}");
        assert!(text.contains("\"cpu_pct\":1.23,"), "{text}");
    }
}
