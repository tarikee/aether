//! `aether check`: what this machine and OS offer, and what is missing.

use crate::cli::Parsed;
use crate::cmd::live::finish;
use aether::sys::{sources, Built, Root};
use std::io::{self, Write};
use std::process::ExitCode;

/// The files the sources read, with what they tell a person.
const FILES: &[(&str, &str)] = &[
    ("CPU counters", "proc/stat"),
    ("CPU clock", "sys/devices/system/cpu/cpufreq"),
    ("memory", "proc/meminfo"),
    ("memory pressure", "proc/pressure/memory"),
    ("uptime", "proc/uptime"),
    ("mounts", "proc/mounts"),
    ("disk counters", "proc/diskstats"),
    ("network counters", "proc/net/dev"),
    ("sensors (hwmon)", "sys/class/hwmon"),
    ("graphics cards (drm)", "sys/class/drm"),
    ("battery", "sys/class/power_supply"),
    ("per-process I/O", "proc/self/io"),
    ("GPU per process", "proc/self/fdinfo"),
    ("device names", "usr/share/hwdata/pci.ids"),
    ("user names", "etc/passwd"),
    (
        "CPU clock per thread",
        "sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq",
    ),
    (
        "CPU power (RAPL)",
        "sys/class/powercap/intel-rapl:0/energy_uj",
    ),
    ("ZFS cache", "proc/spl/kstat/zfs/arcstats"),
];

/// Prints what can be read here.
pub fn run(_parsed: &Parsed) -> ExitCode {
    let root = Root::system();
    let mut out = io::stdout().lock();
    finish(write_report(&mut out, &root))
}

/// Writes the sources (built or not) and the files they read (found or missing).
fn write_report(out: &mut dyn Write, root: &Root) -> io::Result<()> {
    writeln!(out, "sources")?;
    for (name, built) in sources() {
        let state = match built {
            Built::Yes => "built",
            Built::NotYet => "not built for this OS yet",
        };
        writeln!(out, "  {name:<12} {state}")?;
    }
    writeln!(out, "files")?;
    for (label, rel) in FILES {
        let path = root.path(rel);
        let state = if std::fs::metadata(&path).is_err() {
            "missing"
        } else if std::fs::File::open(&path).is_err() {
            "unreadable"
        } else {
            "found"
        };
        writeln!(out, "  {label:<22} {state:<8} {rel}")?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_with_one_file_reports_it_found_and_the_rest_missing() {
        let dir = std::env::temp_dir().join(format!("aether-check-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("proc")).unwrap();
        std::fs::write(dir.join("proc/stat"), "cpu 1 2 3 4\n").unwrap();
        let mut out: Vec<u8> = Vec::new();
        write_report(&mut out, &Root::at(&dir)).unwrap();
        let text = String::from_utf8(out).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.starts_with("sources\n"));
        let line = |rel: &str| {
            text.lines()
                .find(|l| l.ends_with(rel))
                .unwrap_or("")
                .to_string()
        };
        assert!(
            line("proc/stat").contains("CPU counters") && line("proc/stat").contains(" found ")
        );
        assert!(
            line("proc/meminfo").contains("memory") && line("proc/meminfo").contains(" missing ")
        );
    }
}
