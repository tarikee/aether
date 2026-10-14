//! Every process (proc/PID), with what each uses; one process in full; ending a process.
//!
//! A process keeps its `stat`, `statm` and `io` files open while it lives, as long as the
//! open-file budget (the soft limit less a reserve) allows. Past the budget its files are opened,
//! read and closed on each tick, so a reading is never lost to the limit.

use std::fs::{self, File};
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::calls;
use super::reader::{fields, lines, num, read_into, value_of};
use super::GpuProcs;
use crate::sys::{EndError, Proc, ProcDetail, Root, Thread};

/// The one buffer every per-process read goes through (the largest file is `stat`).
const SCRATCH: usize = 1024;
/// File descriptors kept back from the open-file limit for everything else.
const RESERVE: usize = 128;
/// The budget when the open-file limit cannot be read.
const FALLBACK_BUDGET: usize = 512;
const MAX_PARENTS: usize = 64;
const WAIT_STEP: Duration = Duration::from_millis(50);
/// 60 steps of 50 ms: the 3 s a process is given to go.
const WAIT_STEPS: u32 = 60;

/// The process source. Each process's files stay open while it lives, within the budget.
pub struct Procs {
    proc_dir: PathBuf,
    /// Sorted by pid. `list[i]` and `slots[i]` describe the same process.
    list: Vec<Proc>,
    slots: Vec<Slot>,
    page: u64,
    cpus: f64,
    budget: usize,
    /// File descriptors held open for the processes now.
    kept: usize,
    clock: Instant,
    /// The readings so far; an idle process is looked at on the ticks its pid picks.
    tick: u64,
    gpu: GpuProcs,
    pids: Vec<u32>,
    /// The one scratch buffer for every per-process read.
    buf: Vec<u8>,
}

/// One of a process's files: the open file once it is held, and whether the system refused it
/// (another user's process), so that it is never tried again.
#[derive(Default)]
struct Held {
    file: Option<File>,
    denied: bool,
}

/// What is kept for one process between readings.
struct Slot {
    pid: u32,
    stat: Held,
    statm: Held,
    io: Held,
    stat_path: PathBuf,
    statm_path: PathBuf,
    io_path: PathBuf,
    /// The start time seen first; another one later means the pid was reused.
    start: Option<u64>,
    seen: bool,
    /// The CPU ticks last read and when.
    cpu_prev: Option<(u64, f64)>,
    /// The bytes read and written last read and when.
    io_prev: Option<(u64, u64, f64)>,
    /// Whether the CPU time moved at the last look; an active process is read in full.
    active: bool,
    /// Looks at this process since its CPU time last moved.
    looks: u32,
    /// A kernel thread (parent 2, or pid 2): its statm and io are never opened.
    kernel: bool,
    /// The shared pages from the last statm read.
    shared: Option<u64>,
    /// Looks since the last statm and io reads.
    statm_age: u32,
    io_age: u32,
    /// Reads of stat, statm and io so far (tests only).
    #[cfg(test)]
    reads: [u32; 3],
}

#[derive(Debug, PartialEq, Eq)]
enum Refresh {
    Ok,
    Gone,
    Replaced,
}

impl Procs {
    pub fn open(root: &Root) -> Procs {
        Procs {
            proc_dir: root.path("proc"),
            list: Vec::new(),
            slots: Vec::new(),
            page: calls::page_size(),
            cpus: cpu_count(root),
            budget: keep_budget(root),
            kept: 0,
            clock: Instant::now(),
            tick: 0,
            gpu: GpuProcs::open(root),
            pids: Vec::new(),
            buf: vec![0; SCRATCH],
        }
    }

    /// The processes of the last reading that have a PCI address, for the cards' sums.
    pub fn gpu_shares(&self) -> &[(String, f32, u64)] {
        self.gpu.shares()
    }

    pub fn read(&mut self) -> &[Proc] {
        let now = self.clock.elapsed().as_secs_f64();
        self.read_at(now)
    }

    /// One reading at the time `now` (seconds on the source's clock).
    fn read_at(&mut self, now: f64) -> &[Proc] {
        self.tick = self.tick.wrapping_add(1);
        for s in &mut self.slots {
            s.seen = false;
        }
        if let Ok(dir) = fs::read_dir(&self.proc_dir) {
            for entry in dir.flatten() {
                let name = entry.file_name();
                if let Some(pid) = name.to_str().and_then(parse_pid) {
                    self.visit(pid, now);
                }
            }
        }
        // A process the listing no longer has has ended.
        let mut i = 0;
        while i < self.slots.len() {
            if self.slots[i].seen {
                i += 1;
            } else {
                self.remove(i);
            }
        }
        self.pids.clear();
        self.pids.extend(self.list.iter().map(|p| p.pid));
        let gpus = self.gpu.read(&self.pids);
        merge_gpu(&mut self.list, gpus);
        &self.list
    }

    fn visit(&mut self, pid: u32, now: f64) {
        match self.slots.binary_search_by_key(&pid, |s| s.pid) {
            Ok(i) => match self.refresh(i, now) {
                Refresh::Ok => {}
                Refresh::Gone => self.remove(i),
                Refresh::Replaced => {
                    self.remove(i);
                    self.insert(i, pid, now);
                }
            },
            Err(i) => self.insert(i, pid, now),
        }
    }

    fn insert(&mut self, i: usize, pid: u32, now: f64) {
        let dir = self.proc_dir.join(pid.to_string());
        let Ok(meta) = fs::metadata(&dir) else {
            return;
        };
        let slot = Slot {
            pid,
            stat: Held::default(),
            statm: Held::default(),
            io: Held::default(),
            stat_path: dir.join("stat"),
            statm_path: dir.join("statm"),
            io_path: dir.join("io"),
            start: None,
            seen: true,
            cpu_prev: None,
            io_prev: None,
            active: true,
            looks: 0,
            kernel: false,
            shared: None,
            statm_age: 0,
            io_age: 0,
            #[cfg(test)]
            reads: [0; 3],
        };
        let proc = Proc {
            pid,
            ppid: 0,
            uid: meta.uid(),
            threads: 0,
            name: String::new(),
            state: 0,
            cpu_pct: None,
            gpu_pct: None,
            mem: 0,
            rss: 0,
            started_s: 0,
            vram: None,
            read_bps: None,
            write_bps: None,
        };
        self.slots.insert(i, slot);
        self.list.insert(i, proc);
        if !matches!(self.refresh(i, now), Refresh::Ok) {
            self.remove(i);
        }
    }

    fn remove(&mut self, i: usize) {
        let slot = self.slots.remove(i);
        self.list.remove(i);
        self.kept = self.kept.saturating_sub(held(&slot));
    }

    /// Reads one process's files into its sample. A process that was active at its last look is
    /// read in full on every tick. An idle one is looked at on the ticks where
    /// `(tick + pid) % 3 == 0`. Resident memory and CPU come from `stat` on each look; statm (the
    /// shared pages) on every third look of a busy process and every fifth of an idle one; io on
    /// every second look of a busy process and every fifth of an idle one. A kernel thread gets
    /// `stat` only.
    fn refresh(&mut self, i: usize, now: f64) -> Refresh {
        let (cpus, budget, page, tick) = (self.cpus, self.budget, self.page, self.tick);
        let kept = &mut self.kept;
        let buf: &mut [u8] = &mut self.buf;
        let (Some(slot), Some(proc)) = (self.slots.get_mut(i), self.list.get_mut(i)) else {
            return Refresh::Gone;
        };
        slot.seen = true;
        let was_active = slot.active;
        if !was_active && !(tick + u64::from(slot.pid)).is_multiple_of(3) {
            return Refresh::Ok;
        }

        let raw = fetch(&mut slot.stat, kept, budget, &slot.stat_path, buf);
        let Some(st) = parse_stat(raw) else {
            return Refresh::Gone;
        };
        #[cfg(test)]
        {
            slot.reads[0] += 1;
        }
        match slot.start {
            Some(s) if s != st.start => return Refresh::Replaced,
            Some(_) => {}
            None => slot.start = Some(st.start),
        }
        proc.name.clear();
        proc.name.push_str(&String::from_utf8_lossy(st.name));
        proc.ppid = st.ppid;
        proc.state = st.state;
        proc.threads = st.threads;
        proc.started_s = st.start / calls::ticks_per_second();
        slot.kernel = st.ppid == 2 || slot.pid == 2;
        let changed = slot.cpu_prev.is_none_or(|(t0, _)| st.ticks != t0);
        proc.cpu_pct = slot
            .cpu_prev
            .and_then(|(t0, at0)| cpu_percent(st.ticks.saturating_sub(t0), now - at0, cpus));
        slot.cpu_prev = Some((st.ticks, now));
        slot.active = changed;
        slot.looks = if changed {
            0
        } else {
            slot.looks.wrapping_add(1)
        };
        let busy = changed || was_active;
        // Resident pages come from stat on every look; the shared part is kept from statm.
        let rss = st.rss;
        proc.rss = rss.saturating_mul(page);
        slot.statm_age = slot.statm_age.saturating_add(1);
        slot.io_age = slot.io_age.saturating_add(1);
        if !slot.kernel && (slot.shared.is_none() || slot.statm_age >= if busy { 3 } else { 5 }) {
            let raw = fetch(&mut slot.statm, kept, budget, &slot.statm_path, buf);
            #[cfg(test)]
            {
                slot.reads[1] += 1;
            }
            let Some((_, shared)) = parse_statm(raw) else {
                return Refresh::Gone;
            };
            slot.shared = Some(shared);
            slot.statm_age = 0;
        }
        proc.mem = rss
            .saturating_sub(slot.shared.unwrap_or(0))
            .saturating_mul(page);

        // A rate is kept from the last two looks; the second reading of a process has none yet.
        let io_due = slot.io_prev.is_none() || proc.read_bps.is_none();
        if !slot.kernel && (io_due || slot.io_age >= if busy { 2 } else { 5 }) {
            let raw = fetch(&mut slot.io, kept, budget, &slot.io_path, buf);
            #[cfg(test)]
            {
                slot.reads[2] += 1;
            }
            slot.io_age = 0;
            match parse_io(raw) {
                Some((read, written)) => {
                    let (rb, wb) = match slot.io_prev {
                        Some((r0, w0, t0)) if now > t0 => (
                            Some(per_second(read.saturating_sub(r0), now - t0)),
                            Some(per_second(written.saturating_sub(w0), now - t0)),
                        ),
                        _ => (None, None),
                    };
                    proc.read_bps = rb;
                    proc.write_bps = wb;
                    slot.io_prev = Some((read, written, now));
                }
                None => {
                    proc.read_bps = None;
                    proc.write_bps = None;
                    slot.io_prev = None;
                }
            }
        }
        Refresh::Ok
    }

    /// The budget of kept files; the tests set it.
    #[cfg(test)]
    fn set_budget(&mut self, n: usize) {
        self.budget = n;
    }

    /// The reads of one process's stat, statm and io so far (tests only).
    #[cfg(test)]
    fn reads_of(&self, pid: u32) -> [u32; 3] {
        self.slots
            .iter()
            .find(|s| s.pid == pid)
            .map_or([0; 3], |s| s.reads)
    }
}

/// The GPU use of each process, from the GPU source's reading. A process it does not list has
/// no GPU reading (`None`), so a process that uses no GPU is never shown as 0.
fn merge_gpu(list: &mut [Proc], gpu: &[(u32, Option<f32>, Option<u64>)]) {
    for p in list.iter_mut() {
        p.gpu_pct = None;
        p.vram = None;
    }
    for &(pid, pct, vram) in gpu {
        if let Ok(i) = list.binary_search_by_key(&pid, |p| p.pid) {
            if let Some(p) = list.get_mut(i) {
                p.gpu_pct = pct;
                p.vram = vram;
            }
        }
    }
}

/// The file's contents now, read in place into `buf`. A kept file is read from its open file;
/// without one (over the budget) it is opened for this read only and closed after. A file the
/// system refused is not tried again. An empty result means the file is gone or refused.
fn fetch<'a>(
    held: &mut Held,
    kept: &mut usize,
    budget: usize,
    path: &Path,
    buf: &'a mut [u8],
) -> &'a [u8] {
    if held.denied {
        return &[];
    }
    if let Some(f) = held.file.as_ref() {
        return read_into(f, buf);
    }
    match File::open(path) {
        Ok(f) if *kept < budget => {
            *kept += 1;
            held.file = Some(f);
            match held.file.as_ref() {
                Some(f) => read_into(f, buf),
                None => &[],
            }
        }
        Ok(f) => read_into(&f, buf),
        Err(e) => {
            held.denied = e.kind() == ErrorKind::PermissionDenied;
            &[]
        }
    }
}

fn held(slot: &Slot) -> usize {
    [
        slot.stat.file.is_some(),
        slot.statm.file.is_some(),
        slot.io.file.is_some(),
    ]
    .iter()
    .filter(|h| **h)
    .count()
}

fn per_second(delta: u64, seconds: f64) -> u64 {
    (delta as f64 / seconds) as u64
}

fn cpu_percent(delta_ticks: u64, seconds: f64, cpus: f64) -> Option<f32> {
    if seconds <= 0.0 {
        return None;
    }
    Some((delta_ticks as f64 / calls::ticks_per_second() as f64 / seconds / cpus * 100.0) as f32)
}

fn parse_pid(name: &str) -> Option<u32> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    name.parse().ok()
}

/// The fields of `proc/PID/stat` that are read. The name is between the first `(` and the last
/// `)`, because a name can hold brackets of its own.
struct Stat<'a> {
    name: &'a [u8],
    state: u8,
    ppid: u32,
    threads: u32,
    ticks: u64,
    start: u64,
    /// Resident pages (field 24).
    rss: u64,
}

fn parse_stat(buf: &[u8]) -> Option<Stat<'_>> {
    let open = buf.iter().position(|&b| b == b'(')?;
    let close = buf.iter().rposition(|&b| b == b')')?;
    if close <= open {
        return None;
    }
    let name = &buf[open + 1..close];
    let rest = buf.get(close + 1..)?;
    // Token 0 is field 3 (state); field n is token n - 3.
    let mut tok: [&[u8]; 24] = [&[]; 24];
    for (slot, t) in tok.iter_mut().zip(fields(rest)) {
        *slot = t;
    }
    Some(Stat {
        name,
        state: *tok[0].first()?,
        ppid: u32::try_from(num(tok[1])?).ok()?,
        threads: u32::try_from(num(tok[17])?).ok()?,
        ticks: num(tok[11])?.checked_add(num(tok[12])?)?,
        start: num(tok[19])?,
        rss: num(tok[21])?,
    })
}

/// Resident and shared pages from `proc/PID/statm`.
fn parse_statm(buf: &[u8]) -> Option<(u64, u64)> {
    let mut f = fields(buf);
    let _size = f.next()?;
    let resident = num(f.next()?)?;
    let shared = num(f.next()?)?;
    Some((resident, shared))
}

/// Bytes read and written from `proc/PID/io`; `None` when the file is not readable.
fn parse_io(buf: &[u8]) -> Option<(u64, u64)> {
    if buf.is_empty() {
        return None;
    }
    Some((value_of(buf, "read_bytes")?, value_of(buf, "write_bytes")?))
}

/// The number of hardware threads, from the per-thread lines of `proc/stat`.
fn cpu_count(root: &Root) -> f64 {
    let n = fs::read_to_string(root.path("proc/stat"))
        .map(|text| {
            text.lines()
                .filter(|l| {
                    l.strip_prefix("cpu")
                        .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
                })
                .count()
        })
        .unwrap_or(0);
    n.max(1) as f64
}

/// How many files may be kept open: the soft limit less a reserve.
fn keep_budget(root: &Root) -> usize {
    let key: &[u8] = b"Max open files";
    let soft = fs::read(root.path("proc/self/limits"))
        .ok()
        .and_then(|buf| {
            lines(&buf).find_map(|l| {
                l.strip_prefix(key)
                    .and_then(|rest| fields(rest).next())
                    .and_then(num)
            })
        });
    match soft {
        Some(n) => usize::try_from(n)
            .unwrap_or(usize::MAX)
            .saturating_sub(RESERVE),
        None => FALLBACK_BUDGET,
    }
}

/// One process in full, for `aether proc`. Files the process does not allow are `None`.
/// The command line of a process: its arguments, NUL bytes as spaces, the first 256 bytes only.
/// None when it cannot be read (another user's process, or one that has gone).
pub fn command_line(root: &Root, pid: u32) -> Option<String> {
    use std::io::Read;
    let file = File::open(root.path(&format!("proc/{pid}/cmdline"))).ok()?;
    let mut bytes = Vec::with_capacity(256);
    file.take(256).read_to_end(&mut bytes).ok()?;
    Some(
        String::from_utf8_lossy(&bytes)
            .replace('\0', " ")
            .trim_end()
            .to_string(),
    )
}

pub fn detail(root: &Root, pid: u32) -> Option<ProcDetail> {
    let dir = root.path(&format!("proc/{pid}"));
    let raw = fs::read(dir.join("stat")).ok()?;
    let st = parse_stat(&raw)?;
    let cmdline = fs::read(dir.join("cmdline"))
        .ok()
        .map(|b| {
            String::from_utf8_lossy(&b)
                .replace('\0', " ")
                .trim_end()
                .to_string()
        })
        .filter(|s| !s.is_empty());
    let open_files = fs::read_dir(dir.join("fd"))
        .ok()
        .map(|d| u32::try_from(d.flatten().count()).unwrap_or(u32::MAX));
    Some(ProcDetail {
        cmdline,
        exe: link_text(&dir.join("exe")),
        cwd: link_text(&dir.join("cwd")),
        parents: parents(root, st.ppid, pid),
        cpu_s: st.ticks as f64 / calls::ticks_per_second() as f64,
        open_files,
    })
}

/// The threads of one process, from `proc/PID/task/TID/stat`. A thread that has gone while the
/// list was read is left out; `None` when the task list itself cannot be read.
pub fn thread_list(root: &Root, pid: u32) -> Option<Vec<Thread>> {
    let tasks = fs::read_dir(root.path(&format!("proc/{pid}/task"))).ok()?;
    let mut threads = Vec::new();
    for entry in tasks.flatten() {
        let Some(tid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(st) = fs::read(entry.path().join("stat"))
            .ok()
            .and_then(|raw| parse_stat(&raw).map(|st| (st.name.to_vec(), st.state, st.ticks)))
        else {
            continue;
        };
        threads.push(Thread {
            tid,
            name: Some(String::from_utf8_lossy(&st.0).into_owned()),
            state: Some(st.1),
            cpu_s: Some(st.2 as f64 / calls::ticks_per_second() as f64),
        });
    }
    Some(threads)
}

fn link_text(path: &Path) -> Option<String> {
    fs::read_link(path)
        .ok()
        .map(|t| t.to_string_lossy().into_owned())
}

/// The parents of a process, nearest first, up to init.
fn parents(root: &Root, ppid: u32, self_pid: u32) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut cur = ppid;
    while cur > 0 && cur != self_pid && out.len() < MAX_PARENTS {
        let Ok(raw) = fs::read(root.path(&format!("proc/{cur}/stat"))) else {
            break;
        };
        let Some(st) = parse_stat(&raw) else {
            break;
        };
        out.push((cur, String::from_utf8_lossy(st.name).into_owned()));
        cur = st.ppid;
    }
    out
}

/// Whether a process is still there and not a zombie.
fn alive(pid: u32) -> bool {
    fs::read(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|raw| parse_stat(&raw).map(|st| st.state != b'Z'))
        .unwrap_or(false)
}

/// Sends a signal; a failure is `NotAllowed` only if the process is still there.
fn signal(pid: u32, sig: i32) -> Result<(), EndError> {
    if calls::signal(pid, sig).is_ok() || !alive(pid) {
        Ok(())
    } else {
        Err(EndError::NotAllowed)
    }
}

/// Looks every 50 ms for up to 3 s whether the process has gone.
fn wait_gone(pid: u32) -> bool {
    for _ in 0..WAIT_STEPS {
        if !alive(pid) {
            return true;
        }
        sleep(WAIT_STEP);
    }
    !alive(pid)
}

/// Ends a process: TERM, then up to 3 s for it to go; with `force`, KILL after that.
/// Pid 0, pid 1 and this program's own pid are refused. Always the real machine's processes.
pub fn end(pid: u32, force: bool) -> Result<(), EndError> {
    if pid <= 1 || pid == std::process::id() {
        return Err(EndError::Refused);
    }
    if !alive(pid) {
        return Err(EndError::NotFound);
    }
    signal(pid, libc::SIGTERM)?;
    if wait_gone(pid) {
        return Ok(());
    }
    if !force {
        return Err(EndError::StillRunning);
    }
    signal(pid, libc::SIGKILL)?;
    if wait_gone(pid) {
        Ok(())
    } else {
        Err(EndError::StillRunning)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn threads_come_from_the_task_stat_files() {
        let base = std::env::temp_dir().join(format!("aether-threads-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let stats = [
            (
                7u32,
                "7 (main app) S 1 7 7 0 -1 0 0 0 0 0 150 50 0 0 20 0 4 0 1 0 0 0 0 0 0",
            ),
            (
                8,
                "8 (worker) R 1 7 7 0 -1 0 0 0 0 0 20 5 0 0 20 0 4 0 1 0 0 0 0 0 0",
            ),
        ];
        for (tid, line) in stats {
            let dir = base.join(format!("proc/42/task/{tid}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("stat"), line).unwrap();
        }
        let mut threads = thread_list(&Root::at(&base), 42).unwrap();
        let _ = fs::remove_dir_all(&base);
        threads.sort_by_key(|t| t.tid);
        let per_s = calls::ticks_per_second() as f64;
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].name.as_deref(), Some("main app"));
        assert_eq!(threads[0].state, Some(b'S'));
        assert_eq!(threads[0].cpu_s, Some(200.0 / per_s));
        assert_eq!(threads[1].name.as_deref(), Some("worker"));
        assert_eq!(threads[1].cpu_s, Some(25.0 / per_s));
        assert!(thread_list(&Root::at(&base), 42).is_none());
    }

    fn fixture(name: &str) -> Root {
        Root::at(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/procs")
                .join(name),
        )
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).expect("directory");
        for e in fs::read_dir(from).expect("listing") {
            let e = e.expect("entry");
            let ft = e.file_type().expect("file type");
            if ft.is_symlink() {
                continue;
            }
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if ft.is_dir() {
                copy_tree(&src, &dst);
            } else {
                fs::copy(&src, &dst).expect("copy");
            }
        }
    }

    fn find(list: &[Proc], pid: u32) -> &Proc {
        list.iter().find(|p| p.pid == pid).expect("listed")
    }

    fn blank(pid: u32) -> Proc {
        Proc {
            pid,
            ppid: 0,
            uid: 0,
            threads: 0,
            name: String::new(),
            state: 0,
            cpu_pct: None,
            gpu_pct: None,
            mem: 0,
            rss: 0,
            started_s: 0,
            vram: None,
            read_bps: None,
            write_bps: None,
        }
    }

    #[test]
    fn parses_a_stat_line_whose_name_has_brackets() {
        let st = parse_stat(
            b"1002 (web (main)) S 1000 1002 1002 0 -1 4194560 100 0 0 0 50 0 0 0 20 0 30 0 1200 10000000 4000",
        )
        .expect("parses");
        assert_eq!(st.rss, 4000);
        assert_eq!(st.name, &b"web (main)"[..]);
        assert_eq!(st.state, b'S');
        assert_eq!(st.ppid, 1000);
        assert_eq!(st.threads, 30);
        assert_eq!(st.ticks, 50);
        assert_eq!(st.start, 1200);
        assert!(parse_stat(b"1002 (cut").is_none());
    }

    #[test]
    fn reads_the_tree_and_skips_a_process_that_vanished() {
        let mut procs = Procs::open(&fixture("before"));
        let list = procs.read_at(10.0).to_vec();
        let pids: Vec<u32> = list.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![1, 1000, 1001, 1002]);
        let p = find(&list, 1001);
        assert_eq!(p.name, "sleep");
        assert_eq!(p.ppid, 1000);
        assert_eq!(p.threads, 1);
        assert_eq!(p.state, b'R');
        assert_eq!(p.rss, 1000 * 4096);
        assert_eq!(p.mem, 600 * 4096);
        assert_eq!(p.started_s, 9);
        assert_eq!(p.cpu_pct, None);
        assert_eq!(p.read_bps, None);
        assert_eq!(find(&list, 1002).name, "web (main)");
        assert_eq!(find(&list, 1).read_bps, None);
    }

    #[test]
    fn rates_come_from_two_readings() {
        let work = std::env::temp_dir().join(format!("aether-procs-rates-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        copy_tree(fixture("before").base(), &work);
        let mut procs = Procs::open(&Root::at(&work));
        let first = procs.read_at(10.0).to_vec();
        assert!(first
            .iter()
            .all(|p| p.cpu_pct.is_none() && p.read_bps.is_none()));

        // Two seconds later: 600 more ticks (6 s of CPU) on a 4-thread machine is 75%.
        copy_tree(fixture("after").base(), &work);
        let second = procs.read_at(12.0).to_vec();
        let p = find(&second, 1001);
        assert_eq!(p.cpu_pct, Some(75.0));
        assert_eq!(p.read_bps, Some(2_048_000));
        assert_eq!(p.write_bps, Some(1_024_000));
        assert_eq!(p.state, b'R');
        assert_eq!(find(&second, 1000).cpu_pct, Some(0.0));
        assert_eq!(procs.kept, 11);
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn files_beyond_the_budget_read_the_same() {
        let root = fixture("before");
        let mut kept = Procs::open(&root);
        let a = kept.read_at(10.0).to_vec();
        let mut none = Procs::open(&root);
        none.set_budget(0);
        let b = none.read_at(10.0).to_vec();
        assert_eq!(a, b);
        assert_eq!(none.kept, 0);
    }

    #[test]
    fn an_nvidia_reading_without_percent_still_shows_its_vram() {
        let mut list = vec![blank(10), blank(20)];
        merge_gpu(&mut list, &[(10, None, Some(4096)), (20, Some(12.5), None)]);
        assert_eq!((list[0].gpu_pct, list[0].vram), (None, Some(4096)));
        assert_eq!((list[1].gpu_pct, list[1].vram), (Some(12.5), None));
    }

    #[test]
    fn gpu_use_merges_by_pid() {
        let mut list = vec![blank(10), blank(20), blank(30)];
        merge_gpu(
            &mut list,
            &[(20, Some(42.5), Some(1024)), (99, Some(1.0), Some(1))],
        );
        assert_eq!(list[0].gpu_pct, None);
        assert_eq!(list[0].vram, None);
        assert_eq!(list[1].gpu_pct, Some(42.5));
        assert_eq!(list[1].vram, Some(1024));
        assert_eq!(list[2].vram, None);
    }

    /// Writes one process's three files as they stand at a tick: `ticks` of CPU, `resident`
    /// pages, and `read` bytes read from disk.
    fn write_pid(root: &Path, pid: u32, ticks: u64, resident: u64, read: u64) {
        write_pid_under(root, pid, 1000, ticks, resident, read);
    }

    /// As `write_pid`, with the parent given (2 makes a kernel thread). Its shared pages are 100.
    fn write_pid_under(root: &Path, pid: u32, ppid: u32, ticks: u64, resident: u64, read: u64) {
        let dir = root.join("proc").join(pid.to_string());
        fs::create_dir_all(&dir).expect("pid directory");
        let stat = format!(
            "{pid} (sleep) S {ppid} {pid} {pid} 0 -1 4194560 100 0 0 0 {ticks} 0 0 0 20 0 1 0 1200 10000000 {resident}\n"
        );
        fs::write(dir.join("stat"), stat).expect("stat");
        fs::write(dir.join("statm"), format!("1000 {resident} 100 0 0 0 0\n")).expect("statm");
        let io = format!("rchar: 0\nwchar: 0\nread_bytes: {read}\nwrite_bytes: 0\n");
        fs::write(dir.join("io"), io).expect("io");
    }

    /// A copy of the `before` fixture (4 hardware threads) to write processes into.
    fn scratch(name: &str) -> PathBuf {
        let work = std::env::temp_dir().join(format!("aether-procs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        copy_tree(fixture("before").base(), &work);
        work
    }

    #[test]
    fn an_idle_process_is_looked_at_on_its_own_tick_only() {
        const PID: u32 = 2000;
        let p = u64::from(PID);
        let work = scratch("idle");
        let mut procs = Procs::open(&Root::at(&work));
        write_pid(&work, PID, 500, 100, 0);
        procs.read_at(1.0);
        procs.read_at(2.0);
        // The first reading reads all three files; the second reads stat and io (the first rate).
        assert_eq!(procs.reads_of(PID), [2, 1, 2]);
        // statm and io are read on every fifth look: the first, the sixth, the eleventh...
        let mut looks = 2u32;
        for tick in 3..=14u64 {
            write_pid(&work, PID, 500, 100 + tick, 0);
            let before = procs.reads_of(PID);
            procs.read_at(tick as f64);
            let after = procs.reads_of(PID);
            let delta = [0, 1, 2].map(|k| after[k] - before[k]);
            if (tick + p).is_multiple_of(3) {
                looks += 1;
                let due = [1, u32::from(looks % 5 == 1), u32::from(looks % 5 == 2)];
                assert_eq!(delta, due, "tick {tick} is its tick");
            } else {
                assert_eq!(delta, [0, 0, 0], "tick {tick} is off its tick");
            }
        }
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn a_process_that_starts_using_cpu_becomes_active_and_gets_all_three_reads() {
        const PID: u32 = 2000;
        let p = u64::from(PID);
        let work = scratch("active");
        let mut procs = Procs::open(&Root::at(&work));
        write_pid(&work, PID, 500, 100, 0);
        procs.read_at(1.0);
        procs.read_at(2.0);
        // Looks since the first one that finds it moved: statm on every third, io on every second.
        let mut first_look: Option<u64> = None;
        for tick in 3..=12u64 {
            // The CPU time moves on every tick from here on.
            write_pid(&work, PID, 500 + 100 * tick, 100, 0);
            let before = procs.reads_of(PID);
            procs.read_at(tick as f64);
            let after = procs.reads_of(PID);
            let delta = [0, 1, 2].map(|k| after[k] - before[k]);
            match first_look {
                None if (tick + p).is_multiple_of(3) => {
                    first_look = Some(0);
                    assert_eq!(delta, [1, 0, 0], "the look that finds it moved");
                }
                Some(n) => {
                    let n = n + 1;
                    first_look = Some(n);
                    let want = [1, u32::from(n % 3 == 1), u32::from(n % 2 == 1)];
                    assert_eq!(delta, want, "active on tick {tick}, look {n}");
                }
                None => assert_eq!(delta, [0, 0, 0], "tick {tick} is off its tick"),
            }
        }
        assert!(first_look.is_some());
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn a_percent_after_three_ticks_is_the_average_over_them() {
        const PID: u32 = 2000;
        let p = u64::from(PID);
        let work = scratch("average");
        let mut procs = Procs::open(&Root::at(&work));
        write_pid(&work, PID, 500, 100, 0);
        procs.read_at(1.0);
        procs.read_at(2.0);
        let look = (3u64..).find(|t| (t + p).is_multiple_of(3)).unwrap_or(3);
        for tick in 3..=look {
            write_pid(&work, PID, 500, 100, 0);
            procs.read_at(tick as f64);
        }
        // 300 ticks (3 s of CPU) in the three seconds up to the next look, on 4 threads: 25%.
        for tick in look + 1..=look + 3 {
            write_pid(&work, PID, 800, 100, 0);
            let list = procs.read_at(tick as f64).to_vec();
            if tick == look + 3 {
                assert_eq!(find(&list, PID).cpu_pct, Some(25.0));
            }
        }
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn a_kernel_thread_gets_stat_only() {
        const PID: u32 = 2000;
        let work = scratch("kernel");
        let mut procs = Procs::open(&Root::at(&work));
        for tick in 1..=6u64 {
            write_pid_under(&work, PID, 2, 500 + 10 * tick, 0, 0);
            procs.read_at(tick as f64);
        }
        assert_eq!(
            procs.reads_of(PID),
            [6, 0, 0],
            "stat on every reading, nothing else"
        );
        let p = find(procs.read_at(7.0), PID).clone();
        assert_eq!((p.mem, p.rss), (0, 0));
        assert_eq!((p.read_bps, p.write_bps), (None, None));
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn resident_memory_follows_stat_when_statm_is_not_read() {
        const PID: u32 = 2000;
        let work = scratch("rss");
        let mut procs = Procs::open(&Root::at(&work));
        // statm says 300 resident and 100 shared pages, read on the first look only.
        write_pid(&work, PID, 500, 300, 0);
        procs.read_at(1.0);
        let page = calls::page_size();
        write_pid(&work, PID, 600, 300, 0);
        procs.read_at(2.0);
        // Stat now says 500 resident; the look is the third since statm was read, not the third.
        write_pid(&work, PID, 700, 500, 0);
        let before = procs.reads_of(PID)[1];
        let p = find(procs.read_at(3.0), PID).clone();
        assert_eq!(
            procs.reads_of(PID)[1],
            before,
            "statm not read on this look"
        );
        assert_eq!(p.rss, 500 * page, "resident from stat");
        assert_eq!(
            p.mem,
            400 * page,
            "resident from stat less the shared pages kept"
        );
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn detail_reads_one_process() {
        let root = fixture("before");
        let d = detail(&root, 1001).expect("detail");
        assert_eq!(d.cmdline.as_deref(), Some("sleep 300"));
        assert_eq!(d.exe.as_deref(), Some("/usr/bin/sleep"));
        assert_eq!(d.cwd.as_deref(), Some("/srv/work"));
        assert_eq!(
            d.parents,
            vec![(1000, "bash".to_string()), (1, "systemd".to_string())]
        );
        assert_eq!(d.open_files, Some(3));
        assert_eq!(d.cpu_s, 12.0);
        assert!(detail(&root, 1010).is_none());
    }

    #[test]
    fn end_refuses_init_and_itself_and_finds_nothing_at_a_missing_pid() {
        assert_eq!(end(1, false), Err(EndError::Refused));
        assert_eq!(end(std::process::id(), true), Err(EndError::Refused));
        assert_eq!(end(4_294_967_000, false), Err(EndError::NotFound));
    }

    #[test]
    fn end_ends_a_sleep_it_starts() {
        let mut child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("sleep starts");
        assert_eq!(end(child.id(), false), Ok(()));
        let _ = child.wait();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod command_line_tests {
    use super::*;

    #[test]
    fn arguments_are_joined_and_cut_at_256_bytes() {
        let base = std::env::temp_dir().join(format!("aether-cmdline-{}", std::process::id()));
        let dir = base.join("proc/42");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cmdline"), b"python3\0-m\0http.server\0").unwrap();
        let root = Root::at(&base);
        assert_eq!(
            command_line(&root, 42).as_deref(),
            Some("python3 -m http.server")
        );
        fs::write(dir.join("cmdline"), vec![b'a'; 300]).unwrap();
        assert_eq!(command_line(&root, 42).map(|s| s.len()), Some(256));
        assert_eq!(command_line(&root, 43), None);
        let _ = fs::remove_dir_all(&base);
    }
}
