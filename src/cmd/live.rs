//! `aether` (live, or once with `--once`): the readings, written out.

use crate::cli::{compare, Keys, Parsed, Sort, View};
use crate::out::{json, text};
use aether::sys::{
    command_line, Battery, Cpu, Disks, Gpus, Host, Mem, Nets, Proc, Procs, Root, Sensors,
};
use aether::Reading;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// The rows of the terminal on standard output, if there is one.
#[cfg(target_os = "linux")]
fn screen_rows() -> Option<usize> {
    aether::sys::linux::calls::term_rows().map(usize::from)
}

#[cfg(not(target_os = "linux"))]
fn screen_rows() -> Option<usize> {
    None
}

/// Moves the cursor home and clears to the end of the screen: one frame replaces the last.
const CLEAR: &[u8] = b"\x1b[H\x1b[J";

/// The wait between the two readings of `--once`, so its rates are numbers.
const ONCE_WAIT: Duration = Duration::from_millis(250);

/// Runs the live view, or one reading with `--once`.
pub fn run(parsed: &Parsed) -> ExitCode {
    let tty = io::stdout().is_terminal();
    let root = Root::system();
    let started = Instant::now();
    let mut sources = Sources::open(&root);
    let mut out = io::stdout().lock();
    let result = frames(
        |r| sources.fill(r, started),
        &mut out,
        parsed,
        tty,
        std::thread::sleep,
    );
    finish(result)
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
struct Sources {
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
    fn open(root: &Root) -> Sources {
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
    fn fill(&mut self, r: &mut Reading, started: Instant) {
        r.t = started.elapsed().as_secs_f64();
        r.cpu.clone_from(self.cpu.read());
        r.mem.clone_from(self.mem.read());
        fill_vec(&mut r.gpus, self.gpus.read());
        fill_vec(&mut r.sensors, self.sensors.read());
        fill_vec(&mut r.disks, self.disks.read());
        fill_vec(&mut r.nets, self.nets.read());
        r.battery = self.battery.read().cloned();
        r.host.clone_from(self.host.read());
        fill_vec(&mut r.procs, self.procs.read());
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

/// Writes the readings: one when `--once`, else one a tick until the output fails.
/// `read` fills a reading; `pause` waits. Every run reads once before its first frame, so
/// the rates have two readings to work from.
fn frames(
    mut read: impl FnMut(&mut Reading),
    out: &mut dyn Write,
    parsed: &Parsed,
    tty: bool,
    mut pause: impl FnMut(Duration),
) -> io::Result<()> {
    let live = !parsed.once;
    let interval = Duration::try_from_secs_f64(parsed.interval_s).unwrap_or(Duration::MAX);
    let mut reading = Reading::default();
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let root = Root::system();
    let mut commands = Commands::new();
    let mut steady = Steady::default();
    let mut tick = 0u64;
    // A first reading, then a wait, so the rates have two readings to work from.
    read(&mut reading);
    // `--once` waits as long as `-i` asked, or 250 ms when `-i` was not given.
    let once_wait = if parsed.interval_given {
        interval
    } else {
        ONCE_WAIT
    };
    pause(if live { interval } else { once_wait });
    loop {
        let started = Instant::now();
        read(&mut reading);
        if let Some(needle) = parsed.view.filter.as_deref() {
            tick += 1;
            reading
                .procs
                .retain(|p| keeps(p, needle, &mut commands, tick, &root));
            commands.retain(|_, e| e.0 == tick);
        }
        let smooth = parsed.view.steady && parsed.view.by.smooths();
        if smooth {
            steady.update(&reading.procs, parsed.view.by);
        }
        let by = parsed.view.by;
        let rank = |p: &Proc| if smooth { steady.of(p) } else { key(p, by) };
        order(&mut reading.procs, &parsed.view, &rank);
        buf.clear();
        if live && tty && !parsed.json {
            buf.extend_from_slice(CLEAR);
        }
        if parsed.json {
            json::write(&reading, &parsed.view, &mut buf);
        } else {
            let screen = if tty && !parsed.view.all {
                screen_rows()
            } else {
                None
            };
            text::write(&reading, &parsed.view, &mut buf, screen, &rank);
        }
        out.write_all(&buf)?;
        out.flush()?;
        if !live {
            return Ok(());
        }
        pause(interval.saturating_sub(started.elapsed()));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cli::parse;

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
        let r = frames(|_| reads += 1, &mut out, &p, true, |d| pauses.push(d));
        assert!(r.is_ok());
        // A first reading, the wait, then the reading that is printed.
        assert_eq!(reads, 2);
        assert_eq!(pauses, vec![ONCE_WAIT]);
        assert_eq!(count(&out.out, CLEAR), 0);
    }

    #[test]
    fn live_warms_up_then_paces_each_frame_and_clears_on_a_terminal() {
        let p = parse(&args("-i 2")).unwrap();
        let mut reads = 0;
        let mut pauses = Vec::new();
        let mut out = Limited::new(3, io::ErrorKind::Other);
        let r = frames(|_| reads += 1, &mut out, &p, true, |d| pauses.push(d));
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
        let _ = frames(|_| {}, &mut out, &p, false, |_| {});
        assert_eq!(count(&out.out, CLEAR), 0);
        assert_eq!(out.flushes, 3);
    }

    #[test]
    fn json_is_never_cleared() {
        let p = parse(&args("--json")).unwrap();
        let mut out = Limited::new(1, io::ErrorKind::Other);
        let _ = frames(|_| {}, &mut out, &p, true, |_| {});
        assert_eq!(count(&out.out, CLEAR), 0);
    }

    #[test]
    fn a_closed_pipe_is_a_quiet_end() {
        let p = parse(&args("--json")).unwrap();
        let mut reads = 0;
        let mut out = Limited::new(0, io::ErrorKind::BrokenPipe);
        let r = frames(|_| reads += 1, &mut out, &p, false, |_| {});
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
