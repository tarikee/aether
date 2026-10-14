//! The processes that use a graphics card, from their fdinfo files.
//!
//! A new process is looked at once: its file descriptors are listed and the ones on /dev/dri
//! are kept open, one per DRM client. Every known process is looked at again for new
//! descriptors, 20 per tick, in turn. A process is reported from its second reading on, since
//! a rate needs two.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::sys::linux::{
    calls,
    reader::{fields, lines, num, read_into},
};
use crate::sys::Root;

/// Our own processes whose descriptors are looked at again on each tick. Another user's cannot
/// be read, so they are looked at once.
const RECHECK_PER_TICK: usize = 4;
const FDINFO_CAP: usize = 4096;

/// What one fdinfo file says: the DRM client, its engine time in ns, its VRAM in bytes and the
/// cycles of each engine class (xe's keys).
struct Fdinfo {
    id: u64,
    ns: u64,
    vram: u64,
    cycles: Vec<Cycles>,
    /// The PCI address of the card the client is on (`drm-pdev`), if the file has it.
    pdev: Option<String>,
}

/// One engine class of a client: its busy and total cycles, each as far as the file showed it.
struct Cycles {
    class: String,
    busy: Option<u64>,
    total: Option<u64>,
}

/// The VRAM keys: amdgpu's, and Intel's resident memory (i915 `local0`, xe `vram0`).
const VRAM_KEYS: &[&[u8]] = &[
    b"drm-memory-vram",
    b"drm-resident-local0",
    b"drm-resident-vram0",
];

/// One DRM client of a process: its descriptor number, its client id, its fdinfo file kept
/// open, and what it showed at its last reading.
struct Client {
    fd: u32,
    id: u64,
    file: File,
    last: Option<Fdinfo>,
}

/// The clients of one process; empty for a process that uses no card.
#[derive(Default)]
struct Proc {
    clients: Vec<Client>,
    /// Whether the process is ours (the user this runs as owns it).
    own: bool,
    /// Descriptors seen not to be on /dev/dri, so they are not read again.
    plain: HashSet<u32>,
}

/// Per-process GPU use, with its files kept open once a process is seen.
pub struct GpuProcs {
    base: PathBuf,
    procs: HashMap<u32, Proc>,
    alive: HashSet<u32>,
    cursor: usize,
    last: Option<Instant>,
    out: Vec<(u32, f32, u64)>,
    /// The processes with a PCI address: (address, percent, VRAM bytes), for the cards' sums.
    shares: Vec<(String, f32, u64)>,
    uid: u32,
    /// The one buffer every fdinfo read goes through.
    buf: Vec<u8>,
}

impl GpuProcs {
    pub fn open(root: &Root) -> GpuProcs {
        GpuProcs {
            base: root.path("proc"),
            procs: HashMap::new(),
            alive: HashSet::new(),
            cursor: 0,
            last: None,
            out: Vec::new(),
            shares: Vec::new(),
            uid: calls::uid(),
            buf: vec![0; FDINFO_CAP],
        }
    }

    /// For the given PIDs: (pid, percent, VRAM bytes). Unreadable processes are left out.
    pub fn read(&mut self, pids: &[u32]) -> &[(u32, f32, u64)] {
        self.read_at(pids, Instant::now())
    }

    /// The last reading's processes that have a PCI address: (address, percent, VRAM bytes).
    pub fn shares(&self) -> &[(String, f32, u64)] {
        &self.shares
    }

    fn read_at(&mut self, pids: &[u32], now: Instant) -> &[(u32, f32, u64)] {
        let wall_ns = self
            .last
            .and_then(|t| u64::try_from(now.saturating_duration_since(t).as_nanos()).ok())
            .filter(|&w| w > 0);
        self.last = Some(now);

        // A process that is gone is forgotten; its number may come back as a new process.
        self.alive.clear();
        self.alive.extend(pids.iter().copied());
        let alive = &self.alive;
        self.procs.retain(|pid, _| alive.contains(pid));

        // A new process is looked at once; then up to 4 of our own known processes per tick, in
        // turn, for new descriptors. Another user's descriptors cannot be read: not looked at again.
        let buf: &mut [u8] = &mut self.buf;
        for &pid in pids {
            if !self.procs.contains_key(&pid) {
                let mut p = Proc {
                    own: owns(&self.base, pid, self.uid),
                    ..Proc::default()
                };
                discover(&self.base, pid, &mut p, buf);
                self.procs.insert(pid, p);
            }
        }
        let n = pids.len();
        let (mut picked, mut k) = (0, 0);
        while picked < RECHECK_PER_TICK && k < n {
            let pid = pids[(self.cursor + k) % n];
            k += 1;
            if let Some(p) = self.procs.get_mut(&pid).filter(|p| p.own) {
                discover(&self.base, pid, p, buf);
                picked += 1;
            }
        }
        if n > 0 {
            self.cursor = (self.cursor + k) % n;
        }

        self.out.clear();
        self.shares.clear();
        for &pid in pids {
            let Some(p) = self.procs.get_mut(&pid) else {
                continue;
            };
            let mut busy: Option<u64> = None;
            let mut classes: Vec<(String, u64, u64)> = Vec::new();
            let mut vram = 0u64;
            let mut pdev: Option<String> = None;
            p.clients.retain_mut(|c| {
                // Dropped when the read fails, or when the number now belongs to another client.
                let Some(info) = parse(read_into(&c.file, buf)).filter(|i| i.id == c.id) else {
                    return false;
                };
                if let (Some(last), Some(_)) = (c.last.as_ref(), wall_ns) {
                    busy = Some(
                        busy.unwrap_or(0)
                            .saturating_add(info.ns.saturating_sub(last.ns)),
                    );
                    class_deltas(&info.cycles, &last.cycles, &mut classes);
                }
                vram = vram.saturating_add(info.vram);
                if pdev.is_none() {
                    pdev = info.pdev.clone();
                }
                c.last = Some(info);
                true
            });
            // With xe's cycles the process's use is the busiest engine class's share; otherwise
            // the summed engine time over the elapsed time.
            let ratio = classes
                .iter()
                .filter(|c| c.2 > 0)
                .map(|c| (c.1 as f64 / c.2 as f64 * 100.0).min(100.0) as f32)
                .reduce(f32::max);
            let pct = ratio.or_else(|| {
                busy.zip(wall_ns)
                    .map(|(b, w)| (b as f64 / w as f64 * 100.0).min(100.0) as f32)
            });
            if let Some(pct) = pct {
                self.out.push((pid, pct, vram));
                if let Some(pdev) = pdev {
                    self.shares.push((pdev, pct, vram));
                }
            }
        }
        &self.out
    }
}

/// Adds the process's new DRM clients: the descriptors on /dev/dri, one per client id, each
/// fdinfo file kept open. Descriptors already kept are not looked at again. A process that
/// cannot be read (gone, or another user's) keeps what it has.
fn discover(base: &Path, pid: u32, p: &mut Proc, buf: &mut [u8]) {
    let dir = base.join(pid.to_string());
    let Ok(fds) = fs::read_dir(dir.join("fd")) else {
        return;
    };
    let listed: Vec<u32> = fds
        .flatten()
        .filter_map(|fd| fd.file_name().to_str().and_then(|s| s.parse().ok()))
        .collect();
    // The directory's order is not fixed, so the lowest descriptor is the one kept for a client
    // that two descriptors share, on every machine.
    let mut listed = listed;
    listed.sort_unstable();
    // A number no longer listed was closed, so a later descriptor with it is looked at again.
    p.plain.retain(|fd| listed.contains(fd));
    for num in listed {
        if p.plain.contains(&num) || p.clients.iter().any(|c| c.fd == num) {
            continue;
        }
        let on_drm = fs::read_link(dir.join("fd").join(num.to_string()))
            .is_ok_and(|target| target.to_string_lossy().starts_with("/dev/dri/"));
        if !on_drm {
            p.plain.insert(num);
            continue;
        }
        let Ok(file) = File::open(dir.join("fdinfo").join(num.to_string())) else {
            continue;
        };
        let Some(info) = parse(read_into(&file, buf)) else {
            continue;
        };
        if p.clients.iter().any(|c| c.id == info.id) {
            continue;
        }
        p.clients.push(Client {
            fd: num,
            id: info.id,
            file,
            last: None,
        });
    }
}

/// Whether the process is ours: its directory belongs to the user this runs as.
fn owns(base: &Path, pid: u32, uid: u32) -> bool {
    fs::metadata(base.join(pid.to_string())).is_ok_and(|m| m.uid() == uid)
}

/// The client id, engine time (every `drm-engine-*` in ns, summed), the cycles of each engine
/// class (xe) and VRAM (the `VRAM_KEYS`, KiB or MiB, as bytes) of an fdinfo file. `None` without
/// a client id.
fn parse(buf: &[u8]) -> Option<Fdinfo> {
    let mut id = None;
    let mut ns = 0u64;
    let mut vram = 0u64;
    let mut cycles: Vec<Cycles> = Vec::new();
    let mut pdev = None;
    for line in lines(buf) {
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let (key, rest) = (&line[..colon], &line[colon + 1..]);
        let mut f = fields(rest);
        if key == b"drm-client-id" {
            id = f.next().and_then(num);
        } else if key == b"drm-pdev" {
            pdev = f.next().map(|v| String::from_utf8_lossy(v).into_owned());
        } else if VRAM_KEYS.contains(&key) {
            vram = vram.saturating_add(memory(&mut f).unwrap_or(0));
        } else if key.starts_with(b"drm-engine-") && !key.starts_with(b"drm-engine-capacity") {
            if let (Some(n), Some(unit)) = (f.next().and_then(num), f.next()) {
                if unit == b"ns" {
                    ns = ns.saturating_add(n);
                }
            }
        } else if let Some(class) = key.strip_prefix(b"drm-total-cycles-") {
            if let Some(n) = f.next().and_then(num) {
                cycles_entry(&mut cycles, class).total = Some(n);
            }
        } else if let Some(class) = key.strip_prefix(b"drm-cycles-") {
            if let Some(n) = f.next().and_then(num) {
                cycles_entry(&mut cycles, class).busy = Some(n);
            }
        }
    }
    Some(Fdinfo {
        id: id?,
        ns,
        vram,
        cycles,
        pdev,
    })
}

/// A value with its unit, KiB or MiB, as bytes. `None` for another unit.
fn memory<'a>(rest: &mut impl Iterator<Item = &'a [u8]>) -> Option<u64> {
    let n = num(rest.next()?)?;
    match rest.next()? {
        b"KiB" => Some(n.saturating_mul(1024)),
        b"MiB" => Some(n.saturating_mul(1024 * 1024)),
        _ => None,
    }
}

/// The entry of an engine class in `list`, added when it is new.
fn cycles_entry<'a>(list: &'a mut Vec<Cycles>, class: &[u8]) -> &'a mut Cycles {
    let at = match list.iter().position(|c| c.class.as_bytes() == class) {
        Some(at) => at,
        None => {
            list.push(Cycles {
                class: String::from_utf8_lossy(class).into_owned(),
                busy: None,
                total: None,
            });
            list.len() - 1
        }
    };
    &mut list[at]
}

/// Adds to `acc` each engine class's busy and total cycles since the last reading, for the
/// classes both readings showed.
fn class_deltas(now: &[Cycles], before: &[Cycles], acc: &mut Vec<(String, u64, u64)>) {
    for cy in now {
        let (Some(busy), Some(total)) = (cy.busy, cy.total) else {
            continue;
        };
        let Some(old) = before.iter().find(|o| o.class == cy.class) else {
            continue;
        };
        let (Some(old_busy), Some(old_total)) = (old.busy, old.total) else {
            continue;
        };
        let db = busy.saturating_sub(old_busy);
        let dt = total.saturating_sub(old_total);
        match acc.iter_mut().find(|a| a.0 == cy.class) {
            Some(a) => {
                a.1 = a.1.saturating_add(db);
                a.2 = a.2.saturating_add(dt);
            }
            None => acc.push((cy.class.clone(), db, dt)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    const LATER_1234: &str = "drm-client-id:\t42\ndrm-engine-gfx:\t201000000 ns\n\
        drm-engine-compute:\t100500000 ns\ndrm-engine-capacity-gfx:\t1\ndrm-memory-vram:\t2048 KiB\n";
    const LATER_1500: &str = "drm-client-id:\t77\ndrm-engine-gfx:\t1202000000 ns\n\
        drm-memory-vram:\t4096 KiB\n";

    const XE_BEFORE: &str = "drm-driver:\txe\ndrm-client-id:\t9\ndrm-cycles-rcs:\t1000\n\
        drm-total-cycles-rcs:\t10000\ndrm-cycles-bcs:\t0\ndrm-resident-vram0:\t1024 KiB\n";
    const XE_AFTER: &str = "drm-driver:\txe\ndrm-client-id:\t9\ndrm-cycles-rcs:\t4000\n\
        drm-total-cycles-rcs:\t20000\ndrm-cycles-bcs:\t500\ndrm-total-cycles-bcs:\t20000\n\
        drm-resident-vram0:\t1024 KiB\n";

    /// Copies a fixture tree, symlinks included, so a reading can be changed in the copy.
    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let dst = to.join(e.file_name());
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                copy_tree(&e.path(), &dst);
            } else if ft.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(e.path()).unwrap(), &dst).unwrap();
            } else {
                fs::copy(e.path(), &dst).unwrap();
            }
        }
    }

    #[test]
    fn fdinfo_sums_engines_and_reads_vram_in_kib() {
        let info = parse(LATER_1234.as_bytes()).expect("has a client id");
        assert_eq!((info.id, info.ns, info.vram), (42, 301_500_000, 2_097_152));
        assert!(parse(b"drm-engine-gfx:\t5 ns\n").is_none());
    }

    #[test]
    fn a_rate_needs_two_readings_and_is_capped_at_100() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sensors");
        let tmp = std::env::temp_dir().join(format!("aether-gpu-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        copy_tree(&src, &tmp);

        let pids = [1234, 1500, 1700, 1900];
        let mut g = GpuProcs::open(&Root::at(&tmp));
        let t0 = Instant::now();
        assert!(g.read_at(&pids, t0).is_empty());

        fs::write(tmp.join("proc/1234/fdinfo/7"), LATER_1234).unwrap();
        fs::write(tmp.join("proc/1500/fdinfo/4"), LATER_1500).unwrap();
        let got = g.read_at(&pids, t0 + Duration::from_secs(1)).to_vec();
        let _ = fs::remove_dir_all(&tmp);

        assert_eq!(got, vec![(1234, 30.0, 2_097_152), (1500, 100.0, 4_194_304)]);
    }

    #[test]
    fn xe_cycles_and_intel_vram_are_read() {
        let info = parse(XE_BEFORE.as_bytes()).expect("has a client id");
        assert_eq!((info.id, info.ns, info.vram), (9, 0, 1_048_576));
        let rcs = info.cycles.iter().find(|c| c.class == "rcs").unwrap();
        assert_eq!((rcs.busy, rcs.total), (Some(1000), Some(10000)));
        let bcs = info.cycles.iter().find(|c| c.class == "bcs").unwrap();
        assert_eq!((bcs.busy, bcs.total), (Some(0), None));
        let i915 =
            parse(b"drm-client-id:\t3\ndrm-total-local0:\t8 MiB\ndrm-resident-local0:\t2 MiB\n");
        assert_eq!(i915.map(|i| i.vram), Some(2_097_152));
    }

    #[test]
    fn a_process_with_xe_cycles_is_the_busiest_class_share() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sensors");
        let tmp = std::env::temp_dir().join(format!("aether-gpu-xe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        copy_tree(&src, &tmp);
        let fd = tmp.join("proc/4242/fd");
        fs::create_dir_all(&fd).unwrap();
        fs::create_dir_all(tmp.join("proc/4242/fdinfo")).unwrap();
        std::os::unix::fs::symlink("/dev/dri/card1", fd.join("3")).unwrap();
        fs::write(tmp.join("proc/4242/fdinfo/3"), XE_BEFORE).unwrap();

        let pids = [4242];
        let mut g = GpuProcs::open(&Root::at(&tmp));
        let t0 = Instant::now();
        assert!(g.read_at(&pids, t0).is_empty());

        fs::write(tmp.join("proc/4242/fdinfo/3"), XE_AFTER).unwrap();
        let got = g.read_at(&pids, t0 + Duration::from_secs(1)).to_vec();
        let _ = fs::remove_dir_all(&tmp);

        // rcs: 3000 of 10000 cycles busy. bcs has no earlier total, so it is not counted.
        assert_eq!(got, vec![(4242, 30.0, 1_048_576)]);
    }

    #[test]
    fn a_tree_without_processes_gives_nothing() {
        let mut g = GpuProcs::open(&Root::at(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/none"),
        ));
        assert!(g.read(&[1, 2, 3]).is_empty());
    }
}
