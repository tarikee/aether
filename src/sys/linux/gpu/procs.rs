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

use super::nvidia;
use crate::sys::linux::{
    calls,
    reader::{fields, lines, num, read_into},
};
use crate::sys::Root;

/// One process's GPU use: (pid, percent, VRAM bytes), each `None` when unknown.
pub type ProcUse = (u32, Option<f32>, Option<u64>);

/// Our own processes whose descriptors are looked at again on each tick. Another user's cannot
/// be read, so they are looked at once.
const RECHECK_PER_TICK: usize = 4;
const FDINFO_CAP: usize = 4096;

/// What one fdinfo file says: the DRM client, its engine time in ns and its VRAM in bytes.
struct Fdinfo {
    id: u64,
    ns: u64,
    vram: u64,
}

/// One DRM client of a process: its descriptor number, its client id, its fdinfo file kept
/// open, and the engine time it last showed.
struct Client {
    fd: u32,
    id: u64,
    file: File,
    last_ns: Option<u64>,
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
    out: Vec<ProcUse>,
    uid: u32,
    /// The one buffer every fdinfo read goes through.
    buf: Vec<u8>,
    /// The NVIDIA cards' processes, merged in with the fdinfo ones.
    nv: nvidia::Procs,
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
            uid: calls::uid(),
            buf: vec![0; FDINFO_CAP],
            nv: nvidia::Procs::open(nvidia::session(root)),
        }
    }

    /// For the given PIDs: (pid, percent, VRAM bytes). Processes with no reading are left out.
    pub fn read(&mut self, pids: &[u32]) -> &[ProcUse] {
        self.read_at(pids, Instant::now())
    }

    fn read_at(&mut self, pids: &[u32], now: Instant) -> &[ProcUse] {
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

        let nv = self.nv.read();
        self.out.clear();
        for &pid in pids {
            let Some(p) = self.procs.get_mut(&pid) else {
                continue;
            };
            let mut busy: Option<u64> = None;
            let mut vram = 0u64;
            p.clients.retain_mut(|c| {
                // Dropped when the read fails, or when the number now belongs to another client.
                let Some(info) = parse(read_into(&c.file, buf)).filter(|i| i.id == c.id) else {
                    return false;
                };
                if let (Some(last), Some(_)) = (c.last_ns, wall_ns) {
                    busy = Some(
                        busy.unwrap_or(0)
                            .saturating_add(info.ns.saturating_sub(last)),
                    );
                }
                c.last_ns = Some(info.ns);
                vram = vram.saturating_add(info.vram);
                true
            });
            // The fdinfo reading first; the NVIDIA reading fills in what it does not have.
            let amd = match (busy, wall_ns) {
                (Some(b), Some(w)) => Some(((b as f64 / w as f64 * 100.0).min(100.0) as f32, vram)),
                _ => None,
            };
            let theirs = nv.binary_search_by_key(&pid, |r| r.0).ok().map(|i| nv[i]);
            let pct = amd.map(|a| a.0).or(theirs.and_then(|r| r.1));
            let vram = match amd {
                Some((_, v)) => Some(v),
                None => theirs.and_then(|r| r.2),
            };
            if pct.is_some() || vram.is_some() {
                self.out.push((pid, pct, vram));
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
            last_ns: None,
        });
    }
}

/// Whether the process is ours: its directory belongs to the user this runs as.
fn owns(base: &Path, pid: u32, uid: u32) -> bool {
    fs::metadata(base.join(pid.to_string())).is_ok_and(|m| m.uid() == uid)
}

/// The client id, engine time (every `drm-engine-*` in ns, summed) and VRAM (KiB as bytes) of
/// an fdinfo file. `None` without a client id.
fn parse(buf: &[u8]) -> Option<Fdinfo> {
    let mut id = None;
    let mut ns = 0u64;
    let mut vram = 0u64;
    for line in lines(buf) {
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let (key, rest) = (&line[..colon], &line[colon + 1..]);
        let mut f = fields(rest);
        if key == b"drm-client-id" {
            id = f.next().and_then(num);
        } else if key == b"drm-memory-vram" {
            if let (Some(n), Some(unit)) = (f.next().and_then(num), f.next()) {
                if unit == b"KiB" {
                    vram = vram.saturating_add(n.saturating_mul(1024));
                }
            }
        } else if key.starts_with(b"drm-engine-") && !key.starts_with(b"drm-engine-capacity") {
            if let (Some(n), Some(unit)) = (f.next().and_then(num), f.next()) {
                if unit == b"ns" {
                    ns = ns.saturating_add(n);
                }
            }
        }
    }
    Some(Fdinfo { id: id?, ns, vram })
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

        assert_eq!(
            got,
            vec![
                (1234, Some(30.0), Some(2_097_152)),
                (1500, Some(100.0), Some(4_194_304))
            ]
        );
    }

    #[test]
    fn a_tree_without_processes_gives_nothing() {
        let mut g = GpuProcs::open(&Root::at(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/none"),
        ));
        assert!(g.read(&[1, 2, 3]).is_empty());
    }
}
