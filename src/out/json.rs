//! One JSON object per reading, every process included. A missing value is `null`.

use crate::cli::View;
use aether::sys::{
    BatterySample, CpuSample, DiskSample, GpuSample, HostSample, MemSample, NetSample, Proc,
    Sensor, SensorKind,
};
use aether::Reading;
use std::io::Write;

/// Appends one reading as one JSON object and a newline to `out`. `t` is always there; each
/// other part is there when `--only` names it (the same names as the text form).
pub fn write(reading: &Reading, view: &View, out: &mut Vec<u8>) {
    let mut o = Obj::open(out);
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
        array(o.out, &reading.procs, proc_);
    }
    o.close();
    put(out, "\n");
}

/// Writes raw text into the buffer.
fn put(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// A JSON string, with every character that must be escaped escaped.
fn string(out: &mut Vec<u8>, s: &str) {
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
        assert!(text.starts_with("{\"t\":"));
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
            "{\"t\":0,\"cpu\":{\"pct\":null,\"threads\":[],\"threads_mhz\":[],\"mhz\":null,\"temp_c\":null,\"power_w\":null,\"load\":null},\
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
            text.starts_with("{\"t\":1.5,\"cpu\":{\"pct\":12.5,"),
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
}
