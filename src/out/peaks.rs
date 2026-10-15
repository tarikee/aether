//! The summary of a run of readings: for each value the highest, when it was, and the average,
//! kept as running maxima and sums, so the memory used does not grow with the run.

use crate::out::json;
use crate::out::text::{bytes, rate};
use crate::out::width::{cells, cut as cut_end, cut_middle};
use aether::sys::{Proc, SensorKind};
use aether::Reading;

/// How many processes the summary lists, per kind.
const TOP: usize = 5;

/// The widest line of the text summary, in cells.
const WIDTH: usize = 100;

/// The longest GPU or interface name in the text summary.
const NAME: usize = 32;

/// How many processes a recorded line keeps, per kind.
const KEPT: usize = 10;

/// The highest value of one series, when it came (seconds since the start) and the average.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stat {
    pub max: f64,
    pub at: f64,
    sum: f64,
    pub n: u64,
}

impl Stat {
    fn add(&mut self, t: f64, v: f64) {
        if self.n == 0 || v > self.max {
            self.max = v;
            self.at = t;
        }
        self.sum += v;
        self.n += 1;
    }

    fn put(&mut self, t: f64, v: Option<f64>) {
        if let Some(v) = v {
            self.add(t, v);
        }
    }

    pub fn avg(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.sum / self.n as f64
        }
    }
}

/// A process that was among the highest, with its highest value.
#[derive(Clone, Debug, PartialEq)]
pub struct Top {
    pub pid: u32,
    pub name: String,
    pub value: f64,
    pub at: f64,
}

/// A device and its series (a GPU's use, VRAM and temperature; a disk's read and write; an
/// interface's receive and send).
#[derive(Clone, Debug, PartialEq)]
struct Group {
    name: String,
    s: [Stat; 3],
}

fn group<'a>(list: &'a mut Vec<Group>, name: &str) -> &'a mut Group {
    let i = match list.iter().position(|g| g.name == name) {
        Some(i) => i,
        None => {
            list.push(Group {
                name: name.to_string(),
                s: [Stat::default(); 3],
            });
            list.len() - 1
        }
    };
    &mut list[i]
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Peaks {
    pub readings: u64,
    first: Option<f64>,
    last: f64,
    pub cpu: Stat,
    pub mem: Stat,
    pub swap: Stat,
    pub hottest: Stat,
    gpus: Vec<Group>,
    disks: Vec<Group>,
    nets: Vec<Group>,
    pub top_cpu: Vec<Top>,
    pub top_mem: Vec<Top>,
}

/// Keeps the `TOP` processes with the highest value ever seen. A process that fell out had less
/// than the five that stayed, and those only grow, so it could not have come back.
fn top(list: &mut Vec<Top>, p: &Proc, v: f64, t: f64) {
    if v <= 0.0 {
        return;
    }
    if let Some(e) = list.iter_mut().find(|e| e.pid == p.pid && e.name == p.name) {
        if v > e.value {
            e.value = v;
            e.at = t;
        }
        return;
    }
    let new = Top {
        pid: p.pid,
        name: p.name.clone(),
        value: v,
        at: t,
    };
    if list.len() < TOP {
        list.push(new);
    } else if let Some(m) = list.iter_mut().min_by(|a, b| a.value.total_cmp(&b.value)) {
        if v > m.value {
            *m = new;
        }
    }
}

impl Peaks {
    /// Folds one reading in.
    pub fn add(&mut self, r: &Reading) {
        let t = r.t;
        self.readings += 1;
        self.first.get_or_insert(t);
        self.last = t;
        self.cpu.put(t, r.cpu.pct.map(f64::from));
        if r.mem.total > 0 {
            self.mem.add(t, r.mem.used as f64);
        }
        if r.mem.swap_total > 0 {
            self.swap.add(t, r.mem.swap_used as f64);
        }
        self.hottest.put(t, hottest(r));
        for g in &r.gpus {
            let e = group(&mut self.gpus, &g.name);
            e.s[0].put(t, g.pct.map(f64::from));
            e.s[1].put(t, g.vram_used.map(|v| v as f64));
            e.s[2].put(t, g.temp_c.map(f64::from));
        }
        for d in &r.disks {
            let e = group(&mut self.disks, &d.mount);
            e.s[0].put(t, d.read_bps.map(|v| v as f64));
            e.s[1].put(t, d.write_bps.map(|v| v as f64));
        }
        for n in &r.nets {
            let e = group(&mut self.nets, &n.name);
            e.s[0].put(t, n.rx_bps.map(|v| v as f64));
            e.s[1].put(t, n.tx_bps.map(|v| v as f64));
        }
        for p in &r.procs {
            if let Some(c) = p.cpu_pct {
                top(&mut self.top_cpu, p, f64::from(c), t);
            }
            top(&mut self.top_mem, p, p.mem as f64, t);
        }
    }

    /// Seconds from the first reading to the last.
    pub fn span(&self) -> f64 {
        self.first.map_or(0.0, |f| (self.last - f).max(0.0))
    }

    /// The summary for people.
    pub fn text(&self) -> String {
        let mut rows: Vec<[String; 4]> = Vec::new();
        let first = self.first.unwrap_or(0.0);
        let mut row = |label: String, s: &Stat, f: &dyn Fn(f64) -> String| {
            if s.n > 0 {
                rows.push([label, f(s.max), clock(s.at - first), f(s.avg())]);
            }
        };
        let pct = |v: f64| format!("{v:.1}%");
        let byt = |v: f64| bytes(v as u64);
        let rt = |v: f64| rate(Some(v as u64));
        row("CPU".to_string(), &self.cpu, &pct);
        row("memory used".to_string(), &self.mem, &byt);
        row("swap used".to_string(), &self.swap, &byt);
        for g in &self.gpus {
            row(format!("GPU {} use", gpu_name(&g.name)), &g.s[0], &pct);
            row(format!("GPU {} VRAM", gpu_name(&g.name)), &g.s[1], &byt);
            row(
                format!("GPU {} temperature", gpu_name(&g.name)),
                &g.s[2],
                &|v| format!("{v:.1} °C"),
            );
        }
        row("hottest temperature".to_string(), &self.hottest, &|v| {
            format!("{v:.1} °C")
        });
        for d in &self.disks {
            row(
                format!("disk {} read", cut_middle(&d.name, NAME)),
                &d.s[0],
                &rt,
            );
            row(
                format!("disk {} write", cut_middle(&d.name, NAME)),
                &d.s[1],
                &rt,
            );
        }
        for n in &self.nets {
            row(
                format!("net {} receive", cut_middle(&n.name, NAME)),
                &n.s[0],
                &rt,
            );
            row(
                format!("net {} send", cut_middle(&n.name, NAME)),
                &n.s[1],
                &rt,
            );
        }
        let mut out = format!("{} readings over {}\n", self.readings, clock(self.span()));
        let head = ["", "highest", "at", "average"].map(str::to_string);
        rows.insert(0, head);
        let mut w: Vec<usize> = (0..4)
            .map(|c| rows.iter().map(|r| cells(&r[c])).max().unwrap_or(0))
            .collect();
        // A label that still does not leave room for the numbers is cut in the middle.
        w[0] = w[0].min(WIDTH.saturating_sub(w[1] + w[2] + w[3] + 6));
        for r in &mut rows {
            r[0] = cut_middle(&r[0], w[0]);
        }
        for r in &rows {
            out.push_str(&format!(
                "{:<w0$}  {:>w1$}  {:>w2$}  {:>w3$}\n",
                r[0],
                r[1],
                r[2],
                r[3],
                w0 = w[0],
                w1 = w[1],
                w2 = w[2],
                w3 = w[3]
            ));
        }
        for (title, list, f) in [
            ("most CPU", &self.top_cpu, &pct as &dyn Fn(f64) -> String),
            ("most memory", &self.top_mem, &byt as &dyn Fn(f64) -> String),
        ] {
            if list.is_empty() {
                continue;
            }
            out.push_str(&format!("\n{title}\n"));
            let mut l: Vec<&Top> = list.iter().collect();
            l.sort_by(|a, b| b.value.total_cmp(&a.value));
            for e in l {
                let head = format!("  {:>10}  at {}  ", f(e.value), clock(e.at - first));
                let tail = format!(" (pid {})", e.pid);
                let room = WIDTH.saturating_sub(cells(&head) + cells(&tail));
                out.push_str(&format!("{head}{}{tail}\n", cut_middle(&e.name, room)));
            }
        }
        out
    }

    /// The summary as one JSON object and a newline.
    pub fn json(&self) -> String {
        let mut o: Vec<u8> = Vec::new();
        let stat = |o: &mut Vec<u8>, s: &Stat| {
            if s.n == 0 {
                o.extend_from_slice(b"null");
            } else {
                o.extend_from_slice(
                    format!(
                        "{{\"max\":{},\"at\":{},\"avg\":{}}}",
                        num(s.max),
                        num(s.at),
                        num(s.avg())
                    )
                    .as_bytes(),
                );
            }
        };
        o.extend_from_slice(
            format!(
                "{{\"schema\":{},\"readings\":{},\"seconds\":{}",
                crate::out::json::SCHEMA,
                self.readings,
                num(self.span())
            )
            .as_bytes(),
        );
        for (k, s) in [
            ("cpu", &self.cpu),
            ("mem_used", &self.mem),
            ("swap_used", &self.swap),
            ("hottest_c", &self.hottest),
        ] {
            o.extend_from_slice(format!(",\"{k}\":").as_bytes());
            stat(&mut o, s);
        }
        for (k, list, names) in [
            ("gpus", &self.gpus, ["pct", "vram_used", "temp_c"]),
            ("disks", &self.disks, ["read_bps", "write_bps", ""]),
            ("nets", &self.nets, ["rx_bps", "tx_bps", ""]),
        ] {
            let name_key = match k {
                "disks" => "mount",
                _ => "name",
            };
            o.extend_from_slice(format!(",\"{k}\":[").as_bytes());
            for (i, g) in list.iter().enumerate() {
                if i > 0 {
                    o.push(b',');
                }
                o.extend_from_slice(format!("{{\"{name_key}\":").as_bytes());
                json::string(&mut o, &g.name);
                for (j, n) in names.iter().enumerate().filter(|(_, n)| !n.is_empty()) {
                    o.extend_from_slice(format!(",\"{n}\":").as_bytes());
                    stat(&mut o, &g.s[j]);
                }
                o.push(b'}');
            }
            o.push(b']');
        }
        for (k, list) in [("top_cpu", &self.top_cpu), ("top_mem", &self.top_mem)] {
            o.extend_from_slice(format!(",\"{k}\":[").as_bytes());
            let mut l: Vec<&Top> = list.iter().collect();
            l.sort_by(|a, b| b.value.total_cmp(&a.value));
            for (i, e) in l.iter().enumerate() {
                if i > 0 {
                    o.push(b',');
                }
                o.extend_from_slice(format!("{{\"pid\":{},\"name\":", e.pid).as_bytes());
                json::string(&mut o, &e.name);
                o.extend_from_slice(
                    format!(",\"value\":{},\"at\":{}}}", num(e.value), num(e.at)).as_bytes(),
                );
            }
            o.push(b']');
        }
        o.extend_from_slice(b"}\n");
        String::from_utf8_lossy(&o).into_owned()
    }
}

/// A GPU's name for the text summary: past `NAME` cells, a trailing ` [...]` goes first, then
/// the end is cut.
fn gpu_name(name: &str) -> String {
    let mut n = name;
    if cells(n) > NAME {
        if let (Some(at), true) = (n.rfind(" ["), n.ends_with(']')) {
            n = n[..at].trim_end();
        }
    }
    cut_end(n, NAME)
}

/// A JSON number; the values here are always finite.
fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".to_string()
    }
}

/// Seconds since the start as h:mm:ss.
fn clock(t: f64) -> String {
    let s = t.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// The hottest temperature in a reading: the CPU's, the GPUs' and the temperature sensors'.
pub fn hottest(r: &Reading) -> Option<f64> {
    r.cpu
        .temp_c
        .into_iter()
        .chain(r.gpus.iter().filter_map(|g| g.temp_c))
        .chain(
            r.sensors
                .iter()
                .filter(|s| s.kind == SensorKind::Temp)
                .map(|s| s.value),
        )
        .map(f64::from)
        .filter(|v| v.is_finite())
        .max_by(f64::total_cmp)
}

/// Cuts the process list to the 10 with the highest CPU (those using any) and the 10 with the
/// most memory, in their order; a recorded reading would be about 80 kB with every process.
pub fn cut(procs: &mut Vec<Proc>) {
    if procs.len() <= KEPT {
        return;
    }
    let mut keep = vec![false; procs.len()];
    let mut idx: Vec<usize> = (0..procs.len()).collect();
    idx.sort_by(|&a, &b| {
        let c = |i: usize| procs[i].cpu_pct.map_or(-1.0, f64::from);
        c(b).total_cmp(&c(a))
    });
    for &i in idx.iter().take(KEPT) {
        keep[i] = procs[i].cpu_pct.is_some_and(|c| c > 0.0);
    }
    idx.sort_by(|&a, &b| procs[b].mem.cmp(&procs[a].mem));
    for &i in idx.iter().take(KEPT) {
        keep[i] = true;
    }
    let mut i = 0;
    procs.retain(|_| {
        i += 1;
        keep[i - 1]
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use aether::sys::{DiskSample, MemSample};

    fn proc(pid: u32, cpu: f32, mem: u64) -> Proc {
        Proc {
            pid,
            ppid: 1,
            uid: 0,
            threads: 1,
            name: format!("p{pid}"),
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

    fn at(t: f64, cpu: Option<f32>, used: u64) -> Reading {
        let mut r = Reading {
            t,
            ..Reading::default()
        };
        r.cpu.pct = cpu;
        r.mem = MemSample {
            total: 100,
            used,
            ..MemSample::default()
        };
        r
    }

    #[test]
    fn maximum_time_and_average() {
        let mut p = Peaks::default();
        p.add(&at(1.0, None, 10));
        p.add(&at(2.0, Some(40.0), 30));
        p.add(&at(3.0, Some(80.0), 20));
        p.add(&at(4.0, Some(60.0), 20));
        assert_eq!(p.readings, 4);
        assert_eq!((p.cpu.max, p.cpu.at, p.cpu.n), (80.0, 3.0, 3));
        assert_eq!(p.cpu.avg(), 60.0);
        assert_eq!((p.mem.max, p.mem.at, p.mem.avg()), (30.0, 2.0, 20.0));
        assert_eq!(p.span(), 3.0);
        assert_eq!(p.swap.n, 0);
    }

    #[test]
    fn hottest_takes_every_temperature() {
        let mut r = at(1.0, None, 1);
        assert_eq!(hottest(&r), None);
        r.cpu.temp_c = Some(50.0);
        r.sensors.push(aether::sys::Sensor {
            chip: "c".into(),
            label: "l".into(),
            kind: SensorKind::Temp,
            value: 70.0,
        });
        r.sensors.push(aether::sys::Sensor {
            chip: "c".into(),
            label: "fan".into(),
            kind: SensorKind::Fan,
            value: 9000.0,
        });
        assert_eq!(hottest(&r), Some(70.0));
    }

    #[test]
    fn five_processes_are_kept_with_their_highest_moment() {
        let mut p = Peaks::default();
        for t in 0..10u32 {
            let mut r = at(f64::from(t), Some(1.0), 1);
            // Process 1 peaks at t=2; the others rise with time.
            r.procs.push(proc(1, if t == 2 { 90.0 } else { 1.0 }, 5));
            for pid in 2..9 {
                r.procs
                    .push(proc(pid, pid as f32 + t as f32, 100 * u64::from(pid)));
            }
            p.add(&r);
        }
        assert_eq!(p.top_cpu.len(), 5);
        let one = p.top_cpu.iter().find(|e| e.pid == 1).unwrap();
        assert_eq!((one.value, one.at), (90.0, 2.0));
        assert!(p.top_cpu.iter().any(|e| e.pid == 8 && e.value == 17.0));
        assert!(p.top_mem.iter().all(|e| e.pid >= 4));
    }

    #[test]
    fn rates_are_summarised_by_device() {
        let mut p = Peaks::default();
        for (t, v) in [(1.0, Some(100)), (2.0, Some(300)), (3.0, None)] {
            let mut r = at(t, None, 1);
            r.disks.push(DiskSample {
                mount: "/".into(),
                dev: "sda".into(),
                total: 1,
                used: 1,
                read_bps: v,
                write_bps: None,
                busy_pct: None,
            });
            p.add(&r);
        }
        let text = p.text();
        assert!(text.contains("disk / read"), "{text}");
        assert!(!text.contains("disk / write"), "{text}");
        let j = p.json();
        assert!(
            j.contains("\"mount\":\"/\",\"read_bps\":{\"max\":300,\"at\":2,\"avg\":200}"),
            "{j}"
        );
        assert!(j.contains("\"write_bps\":null"), "{j}");
    }

    #[test]
    fn long_names_are_cut_and_every_line_fits() {
        let mut p = Peaks::default();
        for t in 0..3u32 {
            let mut r = at(f64::from(t), Some(5.0), 1);
            r.gpus.push(aether::sys::GpuSample {
                name: "Ellesmere [Radeon RX 470/480/570/570X/580/580X/590]".into(),
                temp_c: Some(55.0),
                pct: Some(3.0),
                vram_used: Some(1 << 30),
                vendor: "amd",
                power_w: None,
                vram_total: None,
                fan_rpm: None,
            });
            r.disks.push(DiskSample {
                mount: "/run/media/tarike/0b3a1f2c-5d4e-4f60-8a9b-c1d2e3f4a5b6".into(),
                dev: "sda1".into(),
                total: 1,
                used: 1,
                read_bps: Some(1 << 20),
                write_bps: Some(5),
                busy_pct: None,
            });
            r.procs.push(proc(9, 50.0, 7));
            r.procs[0].name = "n".repeat(120);
            p.add(&r);
        }
        let text = p.text();
        for l in text.lines() {
            assert!(l.chars().count() <= 100, "{l}");
        }
        assert!(text.contains("GPU Ellesmere use"), "{text}");
        assert!(text.contains("55.0 °C"), "{text}");
        assert!(text.contains("disk /run/media…"), "{text}");
        assert!(!text.contains(" C\n"), "{text}");
        assert_eq!(gpu_name("Short [x]"), "Short [x]");
        assert_eq!(cells(&gpu_name(&"g".repeat(50))), 32);
    }

    #[test]
    fn the_cut_keeps_the_highest_ten_of_each() {
        let mut procs: Vec<Proc> = (0..100u32)
            .map(|i| proc(i, if i < 30 { i as f32 } else { 0.0 }, u64::from(100 - i)))
            .collect();
        cut(&mut procs);
        let pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
        // CPU: 20..30; memory: 0..10; both lists in their first order.
        let want: Vec<u32> = (0..10).chain(20..30).collect();
        assert_eq!(pids, want);
        // Idle processes are not kept for their CPU.
        let mut idle: Vec<Proc> = (0..40u32).map(|i| proc(i, 0.0, 1)).collect();
        idle[39].cpu_pct = Some(3.0);
        cut(&mut idle);
        assert!(idle.iter().any(|p| p.pid == 39));
        assert!(idle.len() <= 11);
        let mut few: Vec<Proc> = (0..5).map(|i| proc(i, 0.0, 1)).collect();
        cut(&mut few);
        assert_eq!(few.len(), 5);
    }
}
