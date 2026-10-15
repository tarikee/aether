//! The values of `aether --json`, as a table: what each is, its unit, when it is `null` and which
//! systems can give it. A program builds its settings from this, so a test keeps the table in
//! step with what the JSON writer prints.

use crate::out::json::{string, SCHEMA};

/// One value of the JSON output.
pub struct Field {
    /// The path in the JSON: `a.b` for a value in an object, `a[]` for each item of a list.
    pub key: &'static str,
    /// `%`, `bytes`, `bytes/s`, ...; `None` for text and for ids.
    pub unit: Option<&'static str>,
    pub about: &'static str,
    pub null_when: &'static str,
    /// Of `linux`, `windows` and `macos`: the systems that can give it.
    pub systems: &'static [&'static str],
}

const ALL: &[&str] = &["linux", "windows", "macos"];
const LINUX: &[&str] = &["linux"];
const LINUX_WINDOWS: &[&str] = &["linux", "windows"];
const LINUX_MACOS: &[&str] = &["linux", "macos"];

const fn f(
    key: &'static str,
    unit: Option<&'static str>,
    about: &'static str,
    null_when: &'static str,
    systems: &'static [&'static str],
) -> Field {
    Field {
        key,
        unit,
        about,
        null_when,
        systems,
    }
}

const NEVER: &str = "never";
const NO_BATTERY: &str = "there is no battery (the whole battery is null)";
const FIRST: &str = "the first reading, and when the counter cannot be read";

/// The `--only` name that reads a value of a part: the part's own name, except that the process
/// list has two finer names. `procs.gpu` reads `procs[].gpu_pct` and `procs[].vram` (the GPU use
/// per process); `procs.base` reads the rest of the process list. `reading` is always read, so
/// it has none.
pub fn only_name(part: &'static str, key: &str) -> Option<&'static str> {
    match part {
        "reading" => None,
        "procs" if key == "procs[].gpu_pct" || key == "procs[].vram" => Some("procs.gpu"),
        "procs" => Some("procs.base"),
        _ => Some(part),
    }
}

/// The parts, in the order of the JSON: `reading` is what every object has, then the names
/// `--only` takes (`cli::PARTS`), each with its values.
pub const PARTS: &[(&str, &[Field])] = &[
    (
        "reading",
        &[f("t", Some("s"), "Seconds since aether started.", NEVER, ALL)],
    ),
    (
        "cpu",
        &[
            f(
                "cpu.pct",
                Some("%"),
                "Share of all CPU time in use, the whole machine.",
                "the first reading, or the counters cannot be read",
                ALL,
            ),
            f(
                "cpu.threads[]",
                Some("%"),
                "Share of one thread's time in use, one item per thread.",
                "never; the list is empty on the first reading",
                ALL,
            ),
            f(
                "cpu.threads_mhz[]",
                Some("MHz"),
                "Clock of one thread, one item per thread.",
                "never; the list is empty when the clocks cannot be read",
                LINUX_WINDOWS,
            ),
            f(
                "cpu.mhz",
                Some("MHz"),
                "Average clock of the threads.",
                "the clock cannot be read",
                LINUX_WINDOWS,
            ),
            f(
                "cpu.temp_c",
                Some("°C"),
                "CPU temperature.",
                "the system has no CPU temperature sensor that can be read",
                LINUX_MACOS,
            ),
            f(
                "cpu.power_w",
                Some("W"),
                "Power drawn by the CPU package since the last reading.",
                "the first reading, or there is no energy counter that can be read (it may need root)",
                LINUX,
            ),
            f(
                "cpu.load[]",
                Some("count"),
                "Load average over 1, 5 and 15 minutes (three items).",
                "the whole list is null when the load cannot be read",
                LINUX_MACOS,
            ),
        ],
    ),
    (
        "mem",
        &[
            f("mem.total", Some("bytes"), "All memory.", NEVER, ALL),
            f(
                "mem.used",
                Some("bytes"),
                "Memory in use (total minus available).",
                NEVER,
                ALL,
            ),
            f(
                "mem.available",
                Some("bytes"),
                "Memory a program can still get without swapping.",
                NEVER,
                ALL,
            ),
            f(
                "mem.cached",
                Some("bytes"),
                "Memory holding files for reuse.",
                NEVER,
                ALL,
            ),
            f(
                "mem.swap_total",
                Some("bytes"),
                "All swap or page file space; 0 when there is none.",
                NEVER,
                ALL,
            ),
            f(
                "mem.swap_used",
                Some("bytes"),
                "Swap or page file space in use.",
                NEVER,
                ALL,
            ),
            f(
                "mem.pressure",
                Some("%"),
                "Share of the last 10 seconds in which some task waited for memory.",
                "the kernel has no memory pressure file",
                LINUX,
            ),
        ],
    ),
    (
        "gpu",
        &[
            f("gpus[].name", None, "The card's name.", NEVER, ALL),
            f(
                "gpus[].vendor",
                None,
                "The maker: amd, intel, nvidia, apple or another name.",
                NEVER,
                ALL,
            ),
            f(
                "gpus[].pct",
                Some("%"),
                "Share of the card's time in use.",
                "the card or its driver does not say (Intel cards), or the first reading",
                ALL,
            ),
            f(
                "gpus[].temp_c",
                Some("°C"),
                "Card temperature.",
                "the card has no temperature that can be read",
                LINUX,
            ),
            f(
                "gpus[].power_w",
                Some("W"),
                "Power the card draws.",
                "the card does not say",
                LINUX,
            ),
            f(
                "gpus[].vram_used",
                Some("bytes"),
                "Video memory in use.",
                "the card does not say (Intel cards)",
                LINUX_WINDOWS,
            ),
            f(
                "gpus[].vram_total",
                Some("bytes"),
                "All video memory.",
                "the card does not say",
                LINUX_WINDOWS,
            ),
            f(
                "gpus[].fan_rpm",
                Some("rpm"),
                "Speed of the card's fan.",
                "the card has no fan that can be read (NVIDIA cards never give it)",
                LINUX,
            ),
        ],
    ),
    (
        "sensors",
        &[
            f(
                "sensors[].chip",
                None,
                "The chip the sensor belongs to.",
                NEVER,
                LINUX_MACOS,
            ),
            f(
                "sensors[].label",
                None,
                "The sensor's name.",
                NEVER,
                LINUX_MACOS,
            ),
            f(
                "sensors[].kind",
                None,
                "What it measures: temp (°C), fan (rpm) or power (W).",
                NEVER,
                LINUX_MACOS,
            ),
            f(
                "sensors[].value",
                None,
                "The reading, in the unit of its kind; macOS gives temperatures only.",
                NEVER,
                LINUX_MACOS,
            ),
        ],
    ),
    (
        "disk",
        &[
            f(
                "disks[].mount",
                None,
                "Where the disk is mounted (a drive on Windows).",
                NEVER,
                ALL,
            ),
            f("disks[].dev", None, "The device's name.", NEVER, ALL),
            f(
                "disks[].total",
                Some("bytes"),
                "Size of the file system.",
                NEVER,
                ALL,
            ),
            f(
                "disks[].used",
                Some("bytes"),
                "Space in use on it.",
                NEVER,
                ALL,
            ),
            f(
                "disks[].read_bps",
                Some("bytes/s"),
                "Rate of reading from the disk.",
                FIRST,
                ALL,
            ),
            f(
                "disks[].write_bps",
                Some("bytes/s"),
                "Rate of writing to the disk.",
                FIRST,
                ALL,
            ),
            f(
                "disks[].busy_pct",
                Some("%"),
                "Share of the time the disk was busy.",
                "the first reading, or the disk has no busy counter",
                LINUX_WINDOWS,
            ),
        ],
    ),
    (
        "net",
        &[
            f("nets[].name", None, "The interface's name.", NEVER, ALL),
            f(
                "nets[].rx_bps",
                Some("bytes/s"),
                "Rate of receiving.",
                FIRST,
                ALL,
            ),
            f(
                "nets[].tx_bps",
                Some("bytes/s"),
                "Rate of sending.",
                FIRST,
                ALL,
            ),
            f(
                "nets[].link_bps",
                Some("bits/s"),
                "Speed of the link.",
                "the system does not say (wireless and virtual interfaces)",
                ALL,
            ),
            f(
                "nets[].addrs[]",
                None,
                "An IPv4 or IPv6 address of the interface (not link-local).",
                "never; the list is empty when none is read",
                ALL,
            ),
        ],
    ),
    (
        "battery",
        &[
            f(
                "battery.pct",
                Some("%"),
                "Charge left.",
                NO_BATTERY,
                ALL,
            ),
            f(
                "battery.state",
                None,
                "charging, discharging, full, not charging or unknown.",
                NO_BATTERY,
                ALL,
            ),
            f(
                "battery.power_w",
                Some("W"),
                "Power going in or out of the battery.",
                "there is no battery, or the system does not say",
                LINUX_MACOS,
            ),
            f(
                "battery.minutes_left",
                Some("min"),
                "Minutes of charge left at the present draw.",
                "there is no battery, it is not discharging, or the system does not say",
                ALL,
            ),
        ],
    ),
    (
        "host",
        &[
            f("host.uptime_s", Some("s"), "Time since boot.", NEVER, ALL),
            f(
                "host.threads",
                Some("count"),
                "Threads the CPU has.",
                NEVER,
                ALL,
            ),
        ],
    ),
    (
        "procs",
        &[
            f("procs[].pid", None, "The process id.", NEVER, ALL),
            f(
                "procs[].ppid",
                None,
                "The id of its parent.",
                NEVER,
                ALL,
            ),
            f(
                "procs[].uid",
                None,
                "The id of its owner.",
                NEVER,
                ALL,
            ),
            f(
                "procs[].threads",
                Some("count"),
                "Threads the process has.",
                NEVER,
                ALL,
            ),
            f("procs[].name", None, "The program's name.", NEVER, ALL),
            f(
                "procs[].state",
                None,
                "One letter for what it is doing (R running, S sleeping, ...).",
                NEVER,
                ALL,
            ),
            f(
                "procs[].cpu_pct",
                Some("%"),
                "Share of the whole machine's CPU the process uses.",
                "the first reading of the process, or it cannot be read",
                ALL,
            ),
            f(
                "procs[].gpu_pct",
                Some("%"),
                "Share of a card's time the process uses.",
                "no card shows it (only AMD cards on Linux)",
                LINUX,
            ),
            f(
                "procs[].mem",
                Some("bytes"),
                "Memory only this process holds (resident without the shared part on Linux).",
                NEVER,
                ALL,
            ),
            f(
                "procs[].rss",
                Some("bytes"),
                "Memory the process has in RAM, shared parts included.",
                NEVER,
                ALL,
            ),
            f(
                "procs[].started_s",
                Some("s"),
                "Seconds after boot at which it started.",
                NEVER,
                ALL,
            ),
            f(
                "procs[].vram",
                Some("bytes"),
                "Video memory the process holds.",
                "no card shows it (only AMD cards on Linux)",
                LINUX,
            ),
            f(
                "procs[].read_bps",
                Some("bytes/s"),
                "Rate of reading from disk.",
                "the first reading of the process, or it is not yours to see",
                ALL,
            ),
            f(
                "procs[].write_bps",
                Some("bytes/s"),
                "Rate of writing to disk.",
                "the first reading of the process, or it is not yours to see",
                ALL,
            ),
        ],
    ),
];

/// `fields --json`: the schema and the parts with their values, as one line.
pub fn write_json(out: &mut Vec<u8>) {
    out.extend_from_slice(format!("{{\"schema\":{SCHEMA},\"parts\":[").as_bytes());
    for (i, (name, fields)) in PARTS.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(b"{\"name\":");
        string(out, name);
        out.extend_from_slice(b",\"fields\":[");
        for (j, field) in fields.iter().enumerate() {
            if j > 0 {
                out.push(b',');
            }
            out.extend_from_slice(b"{\"key\":");
            string(out, field.key);
            out.extend_from_slice(b",\"unit\":");
            match field.unit {
                Some(u) => string(out, u),
                None => out.extend_from_slice(b"null"),
            }
            out.extend_from_slice(b",\"about\":");
            string(out, field.about);
            out.extend_from_slice(b",\"null_when\":");
            string(out, field.null_when);
            out.extend_from_slice(b",\"systems\":[");
            for (k, s) in field.systems.iter().enumerate() {
                if k > 0 {
                    out.push(b',');
                }
                string(out, s);
            }
            out.extend_from_slice(b"],\"only\":");
            match only_name(name, field.key) {
                Some(o) => string(out, o),
                None => out.extend_from_slice(b"null"),
            }
            out.push(b'}');
        }
        out.extend_from_slice(b"]}");
    }
    out.extend_from_slice(b"]}\n");
}

/// `fields`: the table for people.
pub fn write_text(out: &mut Vec<u8>) {
    let width = PARTS
        .iter()
        .flat_map(|(_, fields)| fields.iter())
        .map(|f| f.key.chars().count())
        .max()
        .unwrap_or(0);
    for (name, fields) in PARTS {
        out.extend_from_slice(format!("{name}\n").as_bytes());
        for field in *fields {
            let line = format!(
                "  {:<width$}  {:<7}  {}\n      null when: {}; systems: {}; only: {}\n",
                field.key,
                field.unit.unwrap_or("text"),
                field.about,
                field.null_when,
                field.systems.join(", "),
                only_name(name, field.key).unwrap_or("none"),
            );
            out.extend_from_slice(line.as_bytes());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cli::{Sort, View};
    use crate::out::{json, jsonin};
    use aether::sys::{
        BatterySample, CpuSample, DiskSample, GpuSample, HostSample, MemSample, NetSample, Proc,
        Sensor, SensorKind,
    };
    use aether::Reading;
    use std::collections::BTreeSet;

    /// A reading with made-up values: every optional value there, one item in every list.
    fn full() -> Reading {
        Reading {
            t: 1.5,
            cpu: CpuSample {
                pct: Some(10.0),
                threads: vec![10.0],
                mhz: Some(3000),
                temp_c: Some(50.0),
                load: Some([1.0, 0.5, 0.25]),
                threads_mhz: vec![3000],
                power_w: Some(15.0),
            },
            mem: MemSample {
                total: 8,
                used: 4,
                available: 4,
                cached: 1,
                swap_total: 2,
                swap_used: 1,
                pressure: Some(0.5),
            },
            gpus: vec![GpuSample {
                name: "card".into(),
                vendor: "amd",
                pct: Some(5.0),
                temp_c: Some(40.0),
                power_w: Some(20.0),
                vram_used: Some(1),
                vram_total: Some(2),
                fan_rpm: Some(900),
            }],
            sensors: vec![Sensor {
                chip: "chip".into(),
                label: "temp1".into(),
                kind: SensorKind::Temp,
                value: 30.0,
            }],
            disks: vec![DiskSample {
                mount: "/".into(),
                dev: "sda1".into(),
                total: 10,
                used: 5,
                read_bps: Some(1),
                write_bps: Some(2),
                busy_pct: Some(3.0),
            }],
            nets: vec![NetSample {
                name: "eth0".into(),
                rx_bps: Some(1),
                tx_bps: Some(2),
                link_bps: Some(1_000_000_000),
                addrs: vec!["10.0.0.2".into()],
            }],
            battery: Some(BatterySample {
                pct: 80.0,
                state: "discharging",
                power_w: Some(7.0),
                minutes_left: Some(90),
            }),
            host: HostSample {
                uptime_s: 100,
                threads: 8,
            },
            procs: vec![Proc {
                pid: 1,
                ppid: 0,
                uid: 0,
                threads: 1,
                name: "init".into(),
                state: b'S',
                cpu_pct: Some(1.0),
                gpu_pct: Some(2.0),
                mem: 3,
                rss: 4,
                started_s: 5,
                vram: Some(6),
                read_bps: Some(7),
                write_bps: Some(8),
            }],
        }
    }

    fn written(r: &Reading) -> String {
        let view = View {
            by: Sort::Cpu,
            reverse: false,
            steady: false,
            filter: None,
            flat: false,
            tree: false,
            all: false,
            limit: None,
            only: None,
            disks: None,
            nets: None,
        };
        let mut out = Vec::new();
        json::write(r, &view, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_table_lists_exactly_what_the_json_prints() {
        let printed: BTreeSet<String> = jsonin::key_paths(written(&full()).trim_end())
            .unwrap()
            .into_iter()
            .filter(|k| k != "schema")
            .collect();
        let listed: BTreeSet<String> = PARTS
            .iter()
            .flat_map(|(_, fields)| fields.iter())
            .map(|f| f.key.to_string())
            .collect();
        let missing: Vec<_> = printed.difference(&listed).collect();
        let extra: Vec<_> = listed.difference(&printed).collect();
        assert!(
            missing.is_empty(),
            "printed but not in the table: {missing:?}"
        );
        assert!(extra.is_empty(), "in the table but not printed: {extra:?}");
        let count = PARTS.iter().map(|(_, f)| f.len()).sum::<usize>();
        assert_eq!(count, listed.len(), "a key is listed twice");
    }

    #[test]
    fn the_parts_are_the_ones_only_takes_and_every_entry_is_filled_in() {
        // The table has the process list once; `--only` takes its two finer names too.
        let names: Vec<&str> = PARTS.iter().skip(1).map(|(n, _)| *n).collect();
        let whole: Vec<&str> = crate::cli::PARTS
            .iter()
            .copied()
            .filter(|p| !p.starts_with("procs."))
            .collect();
        assert_eq!(names, whole);
        for (_, fields) in PARTS {
            for field in *fields {
                assert!(!field.about.is_empty() && !field.null_when.is_empty());
                assert!(!field.systems.is_empty(), "{}", field.key);
                assert!(field.systems.iter().all(|s| ALL.contains(s)));
            }
        }
    }

    #[test]
    fn the_json_form_starts_with_the_schema_and_is_one_line() {
        let mut out = Vec::new();
        write_json(&mut out);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("{\"schema\":1,\"parts\":[{\"name\":\"reading\""));
        assert_eq!(text.matches('\n').count(), 1);
        assert!(text.contains("\"key\":\"gpus[].vram_used\",\"unit\":\"bytes\""));
        assert!(text.contains("\"only\":\"procs.gpu\""));
        assert!(text.contains("\"only\":\"procs.base\""));
        assert!(text.contains("\"only\":null"));
    }

    #[test]
    fn every_only_name_in_the_table_is_accepted_by_the_parser() {
        for (part, fields) in PARTS {
            for field in *fields {
                match only_name(part, field.key) {
                    Some(name) => {
                        let line = vec!["--only".to_string(), name.to_string()];
                        assert!(crate::cli::parse(&line).is_ok(), "{name} for {}", field.key);
                    }
                    None => assert_eq!(*part, "reading", "{}", field.key),
                }
            }
        }
    }

    #[test]
    fn the_process_gpu_values_are_procs_gpu_and_the_rest_procs_base() {
        assert_eq!(only_name("procs", "procs[].gpu_pct"), Some("procs.gpu"));
        assert_eq!(only_name("procs", "procs[].vram"), Some("procs.gpu"));
        assert_eq!(only_name("procs", "procs[].rss"), Some("procs.base"));
        assert_eq!(only_name("cpu", "cpu.pct"), Some("cpu"));
        assert_eq!(only_name("reading", "t"), None);
    }
}
