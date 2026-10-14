//! The only place with `unsafe`: the system calls the readers cannot do with files alone.
//! Each wrapper is safe to call and never panics; a failed call is `None`, `false` or an `Err`.
#![allow(unsafe_code, deprecated)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

// ---- terminal, signals, clock -------------------------------------------------------------

/// Sends `sig` to `pid`. The error is the OS error number.
pub fn signal(pid: u32, sig: i32) -> Result<(), i32> {
    let Ok(pid) = i32::try_from(pid) else {
        return Err(libc::ESRCH);
    };
    // SAFETY: `kill` takes plain integers and touches no memory of ours.
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

/// Sends a signal for `aether kill`: the signal is 1 to 64, and pid 0, pid 1 and this
/// program's own pid are refused whatever is asked.
pub fn send_signal(pid: u32, sig: i32) -> Result<(), crate::sys::EndError> {
    use crate::sys::EndError;
    if pid <= 1 || pid == std::process::id() || !(1..=64).contains(&sig) {
        return Err(EndError::Refused);
    }
    match signal(pid, sig) {
        Ok(()) => Ok(()),
        Err(libc::ESRCH) => Err(EndError::NotFound),
        Err(libc::EPERM) => Err(EndError::NotAllowed),
        Err(_) => Err(EndError::Refused),
    }
}

/// The user id this process runs as.
pub fn uid() -> u32 {
    // SAFETY: `getuid` takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Raises the open-file limit from its soft value toward its hard value (macOS caps the soft
/// limit at `OPEN_MAX`). Errors are ignored.
pub fn raise_files() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes into the struct we pass.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return;
    }
    let want = lim.rlim_max.min(10_240);
    if lim.rlim_cur < want {
        lim.rlim_cur = want;
        // SAFETY: `setrlimit` reads the struct we pass.
        let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    }
}

/// The rows and columns of the terminal on standard output, if it is one.
pub fn term_size() -> Option<(u16, u16)> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `TIOCGWINSZ` writes one winsize struct, which we pass.
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    (ok && ws.ws_row > 0 && ws.ws_col > 0).then_some((ws.ws_row, ws.ws_col))
}

/// Set by SIGINT, SIGTERM and SIGHUP once `catch_stops` has run.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop(_sig: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Makes SIGINT, SIGTERM and SIGHUP set the stop flag, so the caller can put the terminal back
/// before it ends.
pub fn catch_stops() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is allowed in a signal handler.
        unsafe { libc::signal(sig, on_stop as *const () as libc::sighandler_t) };
    }
}

/// Whether a stop signal has come since `catch_stops`.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// The local time of day as (hour, minute, second).
pub fn local_hms() -> Option<(u32, u32, u32)> {
    // SAFETY: time takes a null pointer; localtime_r fills the struct it is given.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some((
        u32::try_from(tm.tm_hour).ok()?,
        u32::try_from(tm.tm_min).ok()?,
        u32::try_from(tm.tm_sec).ok()?,
    ))
}

/// The terminal settings of stdin as they were before the keys took it.
#[derive(Clone, Copy)]
pub struct Settings(libc::termios);

/// Stdin in raw mode for the keys: no echo, no line editing, no signal keys. `None` when stdin
/// is not a terminal.
pub fn raw_input() -> Option<Settings> {
    // SAFETY: an all-zero termios is a valid value to be overwritten by `tcgetattr`.
    let mut now: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `tcgetattr` writes the struct we pass.
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut now) } != 0 {
        return None;
    }
    let saved = Settings(now);
    now.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
    now.c_iflag &= !(libc::IXON | libc::ICRNL);
    now.c_cc[libc::VMIN] = 0;
    now.c_cc[libc::VTIME] = 0;
    // SAFETY: `tcsetattr` reads the struct we pass.
    if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &now) } != 0 {
        return None;
    }
    Some(saved)
}

/// Puts the terminal back as `saved` has it.
pub fn restore_input(saved: &Settings) {
    // SAFETY: `tcsetattr` reads the struct we pass.
    let _ = unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved.0) };
}

/// Waits up to `ms` for input on stdin; false at the timeout, or when a signal ends the wait.
pub fn input_ready(ms: i32) -> bool {
    let mut fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll` reads and writes the one pollfd we pass.
    unsafe { libc::poll(&mut fd, 1, ms) > 0 }
}

/// Reads what is waiting on stdin: `Some(0)` at its end, `None` on an error (a signal's too).
pub fn read_input(buf: &mut [u8]) -> Option<usize> {
    // SAFETY: `read` writes at most `buf.len()` bytes into `buf`.
    let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
    usize::try_from(n).ok()
}

// ---- sysctl ---------------------------------------------------------------------------------

/// A number from `sysctlbyname` (4 or 8 bytes wide).
fn sysctl_number(name: &CStr) -> Option<u64> {
    let mut raw = [0u8; 8];
    let mut len = raw.len();
    // SAFETY: the buffer and its length describe `raw`; no new value is set.
    let r = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            raw.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    match (r, len) {
        (0, 8) => Some(u64::from_ne_bytes(raw)),
        (0, 4) => Some(u64::from(u32::from_ne_bytes([
            raw[0], raw[1], raw[2], raw[3],
        ]))),
        _ => None,
    }
}

/// Physical memory in bytes.
pub fn memory_size() -> Option<u64> {
    sysctl_number(c"hw.memsize").filter(|&m| m > 0)
}

/// The number of logical processors.
pub fn thread_count() -> u32 {
    sysctl_number(c"hw.logicalcpu")
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// The page size the virtual memory counters use.
pub fn page_size() -> u64 {
    // SAFETY: `sysconf` reads no memory of ours.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(v).ok().filter(|&p| p > 0).unwrap_or(16384)
}

/// Swap in use: (total, used) bytes.
pub fn swap_usage() -> Option<(u64, u64)> {
    // SAFETY: an all-zero xsw_usage is valid and is overwritten by the call.
    let mut x: libc::xsw_usage = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::xsw_usage>();
    // SAFETY: the buffer and its length describe `x`; no new value is set.
    let r = unsafe {
        libc::sysctlbyname(
            c"vm.swapusage".as_ptr(),
            std::ptr::addr_of_mut!(x).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (r == 0).then_some((x.xsu_total, x.xsu_used))
}

/// When the machine started, in seconds since 1970.
pub fn boot_time() -> Option<u64> {
    // SAFETY: an all-zero timeval is valid and is overwritten by the call.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::timeval>();
    // SAFETY: the buffer and its length describe `tv`; no new value is set.
    let r = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            std::ptr::addr_of_mut!(tv).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (r == 0).then(|| u64::try_from(tv.tv_sec).unwrap_or(0))
}

/// The seconds since 1970.
pub fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The 1, 5 and 15 minute load averages.
pub fn load_average() -> Option<[f32; 3]> {
    let mut l = [0f64; 3];
    // SAFETY: `getloadavg` writes at most 3 doubles into the array.
    if unsafe { libc::getloadavg(l.as_mut_ptr(), 3) } != 3 {
        return None;
    }
    Some([l[0] as f32, l[1] as f32, l[2] as f32])
}

// ---- mach: CPU and memory counters ----------------------------------------------------------

fn host() -> libc::host_t {
    static HOST: OnceLock<libc::host_t> = OnceLock::new();
    *HOST.get_or_init(|| {
        // SAFETY: takes no arguments; the port is kept for the life of the program.
        unsafe { libc::mach_host_self() }
    })
}

/// The ticks of every logical processor as [user, system, idle, nice], in thread order.
pub fn cpu_ticks(out: &mut Vec<[u32; 4]>) -> bool {
    let mut count: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = std::ptr::null_mut();
    let mut size: libc::mach_msg_type_number_t = 0;
    // SAFETY: the three out-parameters are ours; the kernel allocates `info`, freed below.
    let kr = unsafe {
        libc::host_processor_info(
            host(),
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut count,
            &mut info,
            &mut size,
        )
    };
    if kr != 0 || info.is_null() {
        return false;
    }
    out.clear();
    // SAFETY: the kernel returned `count` entries of 4 unsigned ints at `info`.
    let ticks = unsafe { std::slice::from_raw_parts(info.cast::<u32>(), count as usize * 4) };
    out.extend(ticks.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]));
    // SAFETY: `info` is the region the kernel gave us, of `size` ints; it is released once.
    unsafe {
        libc::vm_deallocate(
            libc::mach_task_self(),
            info as libc::vm_address_t,
            size as libc::vm_size_t * std::mem::size_of::<libc::integer_t>(),
        );
    }
    true
}

/// Page counts of the virtual memory: what Activity Monitor adds up.
#[derive(Clone, Copy, Debug, Default)]
pub struct VmPages {
    pub wired: u64,
    pub compressed: u64,
    /// Pages of programs and their data (not file cache), without purgeable ones.
    pub app: u64,
    /// File cache and purgeable pages.
    pub cached: u64,
}

pub fn vm_pages() -> Option<VmPages> {
    // SAFETY: an all-zero vm_statistics64 is valid and is overwritten by the call.
    let mut s: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: the buffer is `s` and `count` is its size in ints.
    let kr = unsafe {
        libc::host_statistics64(
            host(),
            libc::HOST_VM_INFO64,
            std::ptr::addr_of_mut!(s).cast(),
            &mut count,
        )
    };
    if kr != 0 {
        return None;
    }
    let internal = u64::from(s.internal_page_count);
    let purgeable = u64::from(s.purgeable_count);
    Some(VmPages {
        wired: u64::from(s.wire_count),
        compressed: u64::from(s.compressor_page_count),
        app: internal.saturating_sub(purgeable),
        cached: u64::from(s.external_page_count) + purgeable,
    })
}

/// (numerator, denominator) turning the kernel's time units into nanoseconds.
fn timebase() -> (u64, u64) {
    static BASE: OnceLock<(u64, u64)> = OnceLock::new();
    *BASE.get_or_init(|| {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `mach_timebase_info` writes the struct we pass.
        let kr = unsafe { libc::mach_timebase_info(&mut tb) };
        if kr == 0 && tb.numer > 0 && tb.denom > 0 {
            (u64::from(tb.numer), u64::from(tb.denom))
        } else {
            (1, 1)
        }
    })
}

/// Kernel time units to nanoseconds.
pub fn ticks_to_ns(ticks: u64) -> u64 {
    let (n, d) = timebase();
    (u128::from(ticks) * u128::from(n) / u128::from(d)) as u64
}

// ---- mounts ---------------------------------------------------------------------------------

/// One mounted file system.
#[derive(Clone, Debug)]
pub struct Mount {
    pub from: String,
    pub on: String,
    pub fstype: String,
    pub total: u64,
    pub used: u64,
    pub local: bool,
    /// Not marked `nobrowse`: the system's hidden volumes (VM, Preboot, xarts...) are.
    pub browse: bool,
}

fn c_text(raw: &[c_char]) -> String {
    let bytes: Vec<u8> = raw
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Every mounted file system with its space (`getfsstat`, without waiting for slow ones).
pub fn mounts(out: &mut Vec<Mount>) -> bool {
    // SAFETY: a null buffer asks only for the count.
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    let Ok(count) = usize::try_from(n) else {
        return false;
    };
    if count == 0 {
        return false;
    }
    // SAFETY: an all-zero statfs is valid and is overwritten by the call.
    let mut list: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; count + 4];
    let bytes = list.len() * std::mem::size_of::<libc::statfs>();
    // SAFETY: the buffer holds `bytes` bytes of statfs structs.
    let n = unsafe {
        libc::getfsstat(
            list.as_mut_ptr(),
            c_int::try_from(bytes).unwrap_or(c_int::MAX),
            libc::MNT_NOWAIT,
        )
    };
    let Ok(got) = usize::try_from(n) else {
        return false;
    };
    out.clear();
    for s in list.iter().take(got) {
        let bsize = u64::from(s.f_bsize);
        out.push(Mount {
            from: c_text(&s.f_mntfromname),
            on: c_text(&s.f_mntonname),
            fstype: c_text(&s.f_fstypename),
            total: s.f_blocks.saturating_mul(bsize),
            used: s.f_blocks.saturating_sub(s.f_bfree).saturating_mul(bsize),
            local: s.f_flags & libc::MNT_LOCAL as u32 != 0,
            browse: s.f_flags & libc::MNT_DONTBROWSE as u32 == 0,
        });
    }
    true
}

// ---- network interfaces ---------------------------------------------------------------------

/// The counters of an interface as `getifaddrs` hands them out (32 bits wide, so they wrap).
#[repr(C)]
struct IfData {
    ifi_type: u8,
    ifi_typelen: u8,
    ifi_physical: u8,
    ifi_addrlen: u8,
    ifi_hdrlen: u8,
    ifi_recvquota: u8,
    ifi_xmitquota: u8,
    ifi_unused1: u8,
    ifi_mtu: u32,
    ifi_metric: u32,
    ifi_baudrate: u32,
    ifi_ipackets: u32,
    ifi_ierrors: u32,
    ifi_opackets: u32,
    ifi_oerrors: u32,
    ifi_collisions: u32,
    ifi_ibytes: u32,
    ifi_obytes: u32,
}

/// One interface's link entry: name, whether it is Ethernet-like and up, its speed and counters.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub name: [u8; 16],
    pub ether: bool,
    pub up: bool,
    pub baud: u32,
    pub rx: u32,
    pub tx: u32,
}

impl Link {
    pub fn name(&self) -> &str {
        let n = self.name.iter().position(|&b| b == 0).unwrap_or(16);
        std::str::from_utf8(&self.name[..n]).unwrap_or("")
    }
}

/// Every interface's link entry. `out` keeps its memory between readings.
pub fn links(out: &mut Vec<Link>) -> bool {
    out.clear();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list that stays valid until freeifaddrs.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return false;
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list returned above, which is not freed yet.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() || ifa.ifa_data.is_null() {
            continue;
        }
        // SAFETY: a non-null ifa_addr points at a sockaddr; its family says which kind.
        if i32::from(unsafe { (*ifa.ifa_addr).sa_family }) != libc::AF_LINK {
            continue;
        }
        // SAFETY: for a link entry, ifa_data points at the interface's if_data.
        let d = unsafe { &*ifa.ifa_data.cast::<IfData>() };
        let mut name = [0u8; 16];
        // SAFETY: ifa_name is a NUL-terminated string owned by the list.
        let raw = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_bytes();
        let n = raw.len().min(15);
        name[..n].copy_from_slice(&raw[..n]);
        out.push(Link {
            name,
            ether: d.ifi_type == 6,
            up: ifa.ifa_flags & libc::IFF_UP as u32 != 0,
            baud: d.ifi_baudrate,
            rx: d.ifi_ibytes,
            tx: d.ifi_obytes,
        });
    }
    // SAFETY: `head` came from getifaddrs and is freed exactly once.
    unsafe { libc::freeifaddrs(head) };
    true
}

/// Every interface's addresses as (interface, address). Link-local IPv6 is left out.
pub fn addresses(out: &mut Vec<(String, String)>) {
    out.clear();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list that stays valid until freeifaddrs.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return;
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list returned above, which is not freed yet.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
            continue;
        }
        // SAFETY: a non-null ifa_addr points at a sockaddr; its family says which kind.
        let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
        let text = if family == libc::AF_INET {
            // SAFETY: family AF_INET means the storage holds a sockaddr_in.
            let a = unsafe { std::ptr::read_unaligned(ifa.ifa_addr.cast::<libc::sockaddr_in>()) };
            std::net::Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr)).to_string()
        } else if family == libc::AF_INET6 {
            // SAFETY: family AF_INET6 means the storage holds a sockaddr_in6.
            let a = unsafe { std::ptr::read_unaligned(ifa.ifa_addr.cast::<libc::sockaddr_in6>()) };
            let ip = std::net::Ipv6Addr::from(a.sin6_addr.s6_addr);
            if ip.segments()[0] & 0xffc0 == 0xfe80 {
                continue;
            }
            ip.to_string()
        } else {
            continue;
        };
        // SAFETY: ifa_name is a NUL-terminated string owned by the list.
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) };
        out.push((name.to_string_lossy().into_owned(), text));
    }
    // SAFETY: `head` came from getifaddrs and is freed exactly once.
    unsafe { libc::freeifaddrs(head) };
}

// ---- users ----------------------------------------------------------------------------------

/// The account names by uid, from the directory service (the password file alone lacks the
/// people who log in).
pub fn user_names() -> HashMap<u32, String> {
    let mut names = HashMap::new();
    // SAFETY: the passwd entry returned by getpwent is read before the next call.
    unsafe {
        libc::setpwent();
        loop {
            let p = libc::getpwent();
            if p.is_null() {
                break;
            }
            if !(*p).pw_name.is_null() {
                let name = CStr::from_ptr((*p).pw_name).to_string_lossy().into_owned();
                names.entry((*p).pw_uid).or_insert(name);
            }
        }
        libc::endpwent();
    }
    names
}

// ---- processes ------------------------------------------------------------------------------

const PROC_PIDLISTTHREADIDS: c_int = 28;
const PROC_PIDTHREADID64INFO: c_int = 15;
const PROC_PIDFDSOCKETINFO: c_int = 3;

/// Every process id, newest call's answer; returns how many. `buf` keeps its memory.
pub fn list_pids(buf: &mut Vec<i32>) -> usize {
    // SAFETY: a null buffer asks only for the count.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(n) = usize::try_from(n) else {
        return 0;
    };
    let want = n + 64;
    if buf.len() < want {
        buf.resize(want, 0);
    }
    let bytes = c_int::try_from(buf.len() * 4).unwrap_or(c_int::MAX);
    // SAFETY: the buffer holds `bytes` bytes.
    let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes) };
    usize::try_from(got).unwrap_or(0).min(buf.len())
}

/// Fills `T` from `proc_pidinfo`; `None` unless the whole struct came back.
fn pid_info<T>(pid: u32, flavor: c_int, arg: u64) -> Option<T> {
    let pid = c_int::try_from(pid).ok()?;
    let size = c_int::try_from(std::mem::size_of::<T>()).ok()?;
    // SAFETY: only used with plain C structs, for which all-zero is a valid value.
    let mut v: T = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is `v`, of `size` bytes.
    let n = unsafe { libc::proc_pidinfo(pid, flavor, arg, std::ptr::addr_of_mut!(v).cast(), size) };
    (n == size).then_some(v)
}

/// A process's task counters (CPU time in kernel units, resident size, threads).
pub fn task_info(pid: u32) -> Option<libc::proc_taskinfo> {
    pid_info(pid, libc::PROC_PIDTASKINFO, 0)
}

/// A process's BSD info: name, parent, owner, start time, status.
pub fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    pid_info(pid, libc::PROC_PIDTBSDINFO, 0)
}

/// BSD info and task counters in one call; `None` when the task part cannot be read (a
/// process this program may not look into), when `bsd_info` still gives the first part.
pub fn task_all(pid: u32) -> Option<(libc::proc_bsdinfo, libc::proc_taskinfo)> {
    let all: libc::proc_taskallinfo = pid_info(pid, libc::PROC_PIDTASKALLINFO, 0)?;
    Some((all.pbsd, all.ptinfo))
}

/// A process as the kernel's table gives it to anybody, whoever owns it.
#[derive(Clone, Copy, Debug)]
pub struct Kp {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    /// When it started, in microseconds since 1970.
    pub start: u64,
    /// The kernel's state: 1 idle, 2 running, 3 sleeping, 4 stopped, 5 zombie.
    pub stat: u8,
    comm: [u8; 16],
}

impl Kp {
    /// The name the kernel keeps, cut to 15 bytes.
    pub fn comm(&self) -> String {
        let n = self.comm.iter().position(|&b| b == 0).unwrap_or(16);
        String::from_utf8_lossy(&self.comm[..n]).into_owned()
    }
}

/// Size and field places of `struct kinfo_proc`, checked against the C header.
const KP_SIZE: usize = 648;
const KP_START: usize = 0;
const KP_STAT: usize = 36;
const KP_PID: usize = 40;
const KP_COMM: usize = 243;
const KP_UID: usize = 420;
const KP_PPID: usize = 560;

/// Reads record `i` of a `kinfo_proc` table.
pub fn kinfo_at(buf: &[u8], i: usize) -> Option<Kp> {
    let r = buf.get(i * KP_SIZE..(i + 1) * KP_SIZE)?;
    let w = |at: usize| u32::from_ne_bytes([r[at], r[at + 1], r[at + 2], r[at + 3]]);
    let sec = u64::from_ne_bytes([r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]]);
    let usec = u64::from(w(KP_START + 8));
    let mut comm = [0u8; 16];
    comm.copy_from_slice(&r[KP_COMM..KP_COMM + 16]);
    Some(Kp {
        pid: w(KP_PID),
        ppid: w(KP_PPID),
        uid: w(KP_UID),
        start: sec.saturating_mul(1_000_000).saturating_add(usec),
        stat: r[KP_STAT],
        comm,
    })
}

/// Fills `buf` with the table of every process (`KERN_PROC_ALL`) and returns how many records.
pub fn kinfo_all(buf: &mut Vec<u8>) -> usize {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL];
    for _ in 0..4 {
        let mut len: usize = 0;
        // SAFETY: a null buffer asks only for the size.
        let r = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 {
            return 0;
        }
        // Room for processes that start between the two calls.
        len += len / 8 + 16 * KP_SIZE;
        buf.resize(len, 0);
        // SAFETY: the buffer holds `len` bytes.
        let r = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r == 0 {
            return len / KP_SIZE;
        }
    }
    0
}

/// One process from the kernel's table, whoever owns it.
pub fn kinfo_pid(pid: u32) -> Option<Kp> {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        c_int::try_from(pid).ok()?,
    ];
    let mut buf = [0u8; KP_SIZE];
    let mut len = KP_SIZE;
    // SAFETY: the buffer holds KP_SIZE bytes.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 || len < KP_SIZE {
        return None;
    }
    kinfo_at(&buf, 0).filter(|k| k.pid == pid)
}

/// The name from a BSD info (the long name when there is one).
pub fn bsd_name(b: &libc::proc_bsdinfo) -> String {
    let long = c_text(&b.pbi_name);
    if long.is_empty() {
        c_text(&b.pbi_comm)
    } else {
        long
    }
}

/// What a process has used: footprint, bytes read and written, CPU time in kernel units.
#[derive(Clone, Copy, Debug)]
pub struct Usage {
    pub footprint: u64,
    pub read: u64,
    pub written: u64,
}

pub fn usage(pid: u32) -> Option<Usage> {
    let pid = c_int::try_from(pid).ok()?;
    // SAFETY: an all-zero rusage_info_v2 is valid and is overwritten by the call.
    let mut ri: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is `ri`; the call fills a rusage_info_v2 for this flavor.
    let r = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            std::ptr::addr_of_mut!(ri).cast::<libc::rusage_info_t>(),
        )
    };
    (r == 0).then_some(Usage {
        footprint: ri.ri_phys_footprint,
        read: ri.ri_diskio_bytesread,
        written: ri.ri_diskio_byteswritten,
    })
}

/// The program file of a process.
pub fn exe_path(pid: u32) -> Option<String> {
    let pid = c_int::try_from(pid).ok()?;
    let mut buf = [0u8; 4096];
    // SAFETY: the buffer holds 4096 bytes.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
    Some(String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned())
}

/// The folder a process works in.
pub fn cwd_path(pid: u32) -> Option<String> {
    let v: libc::proc_vnodepathinfo = pid_info(pid, libc::PROC_PIDVNODEPATHINFO, 0)?;
    // SAFETY: `vip_path` is 1024 contiguous chars, declared as an array of 32 arrays of 32.
    let flat =
        unsafe { std::slice::from_raw_parts(v.pvi_cdir.vip_path.as_ptr().cast::<c_char>(), 1024) };
    let s = c_text(flat);
    (!s.is_empty()).then_some(s)
}

/// How many files a process has open.
pub fn open_file_count(pid: u32) -> Option<u32> {
    let pid = c_int::try_from(pid).ok()?;
    // SAFETY: a null buffer asks only for the size.
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    let size = usize::try_from(n).ok().filter(|&n| n > 0)?;
    u32::try_from(size / std::mem::size_of::<libc::proc_fdinfo>()).ok()
}

/// The arguments of a process, joined by spaces, from `KERN_PROCARGS2`.
pub fn command_line(pid: u32) -> Option<String> {
    let pid = c_int::try_from(pid).ok()?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut len: usize = 0;
    // SAFETY: a null buffer asks only for the size.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 || len < 8 {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: the buffer holds `len` bytes.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 {
        return None;
    }
    buf.truncate(len);
    Some(join_args(&buf))
}

/// The text of a `KERN_PROCARGS2` answer: a count, the program path, padding, then the
/// arguments each ended by NUL.
pub fn join_args(buf: &[u8]) -> String {
    let Some(head) = buf.get(..4) else {
        return String::new();
    };
    let argc = i32::from_ne_bytes([head[0], head[1], head[2], head[3]]).max(0) as usize;
    let rest = &buf[4..];
    // Skip the program path, then the NULs after it.
    let after = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let rest = &rest[after..];
    let start = rest.iter().position(|&b| b != 0).unwrap_or(rest.len());
    let args: Vec<String> = rest[start..]
        .split(|&b| b == 0)
        .take(argc)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    args.join(" ")
}

/// The thread ids of a process.
pub fn thread_ids(pid: u32, out: &mut Vec<u64>) -> bool {
    let Ok(pid) = c_int::try_from(pid) else {
        return false;
    };
    let mut cap = 256usize;
    loop {
        out.clear();
        out.resize(cap, 0);
        let bytes = c_int::try_from(cap * 8).unwrap_or(c_int::MAX);
        // SAFETY: the buffer holds `bytes` bytes.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDLISTTHREADIDS,
                0,
                out.as_mut_ptr().cast(),
                bytes,
            )
        };
        let Ok(n) = usize::try_from(n) else {
            out.clear();
            return false;
        };
        if n == 0 {
            out.clear();
            return false;
        }
        if n < cap * 8 || cap >= 65_536 {
            out.truncate(n / 8);
            return true;
        }
        cap *= 4;
    }
}

/// One thread's name, state and CPU time.
pub struct ThreadInfo {
    pub name: String,
    pub state: u8,
    pub cpu_ns: u64,
}

pub fn thread_info(pid: u32, tid: u64) -> Option<ThreadInfo> {
    let t: libc::proc_threadinfo = pid_info(pid, PROC_PIDTHREADID64INFO, tid)?;
    let state = match t.pth_run_state {
        libc::TH_STATE_RUNNING => b'R',
        libc::TH_STATE_STOPPED => b'T',
        libc::TH_STATE_UNINTERRUPTIBLE => b'D',
        libc::TH_STATE_HALTED => b'Z',
        _ => b'S',
    };
    Some(ThreadInfo {
        name: c_text(&t.pth_name),
        state,
        cpu_ns: ticks_to_ns(t.pth_user_time.saturating_add(t.pth_system_time)),
    })
}

// ---- sockets --------------------------------------------------------------------------------

/// One socket of a process, from `PROC_PIDFDSOCKETINFO`.
#[derive(Clone, Debug)]
pub struct RawSocket {
    pub udp: bool,
    pub v6: bool,
    pub local: std::net::IpAddr,
    pub port: u16,
    pub remote: std::net::IpAddr,
    pub remote_port: u16,
    /// The kernel's TCP state number (0 closed, 1 listen, 4 established, ...).
    pub state: u32,
}

/// Size and field places of the kernel's `socket_fdinfo`, checked against the C header.
const SOCKET_FDINFO: usize = 792;
const OFF_PROTOCOL: usize = 180;
const OFF_FAMILY: usize = 184;
const OFF_KIND: usize = 256;
const OFF_FPORT: usize = 264;
const OFF_LPORT: usize = 268;
const OFF_VFLAG: usize = 288;
const OFF_FADDR: usize = 296;
const OFF_LADDR: usize = 312;
const OFF_TCP_STATE: usize = 344;

fn word(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_ne_bytes([s[0], s[1], s[2], s[3]]))
}

fn address(b: &[u8], at: usize, v6: bool) -> std::net::IpAddr {
    let mut a = [0u8; 16];
    if let Some(s) = b.get(at..at + 16) {
        a.copy_from_slice(s);
    }
    if v6 {
        std::net::IpAddr::V6(std::net::Ipv6Addr::from(a))
    } else {
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(a[12], a[13], a[14], a[15]))
    }
}

/// Reads the TCP and UDP sockets a process has open, calling `f` for each.
pub fn process_sockets(pid: u32, fds: &mut Vec<libc::proc_fdinfo>, mut f: impl FnMut(RawSocket)) {
    let Ok(cpid) = c_int::try_from(pid) else {
        return;
    };
    // SAFETY: a null buffer asks only for the size.
    let n = unsafe { libc::proc_pidinfo(cpid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    let Ok(bytes) = usize::try_from(n) else {
        return;
    };
    let each = std::mem::size_of::<libc::proc_fdinfo>();
    if bytes < each {
        return;
    }
    let cap = bytes / each + 16;
    // SAFETY: proc_fdinfo is a plain C struct; all-zero is valid.
    fds.resize(cap, unsafe { std::mem::zeroed() });
    // SAFETY: the buffer holds `cap` structs.
    let got = unsafe {
        libc::proc_pidinfo(
            cpid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            c_int::try_from(cap * each).unwrap_or(c_int::MAX),
        )
    };
    let Ok(got) = usize::try_from(got) else {
        return;
    };
    let mut raw = [0u8; SOCKET_FDINFO];
    for fd in fds.iter().take(got / each) {
        if fd.proc_fdtype != libc::PROX_FDTYPE_SOCKET as u32 {
            continue;
        }
        // SAFETY: the buffer is `raw`, of SOCKET_FDINFO bytes.
        let n = unsafe {
            libc::proc_pidfdinfo(
                cpid,
                fd.proc_fd,
                PROC_PIDFDSOCKETINFO,
                raw.as_mut_ptr().cast::<c_void>(),
                SOCKET_FDINFO as c_int,
            )
        };
        if n != SOCKET_FDINFO as c_int {
            continue;
        }
        let family = word(&raw, OFF_FAMILY) as i32;
        let kind = word(&raw, OFF_KIND);
        let proto = word(&raw, OFF_PROTOCOL) as i32;
        let tcp = kind == 2;
        let udp = kind == 1 && proto == libc::IPPROTO_UDP;
        if !(tcp || udp) || !(family == libc::AF_INET || family == libc::AF_INET6) {
            continue;
        }
        let v6 = raw.get(OFF_VFLAG).is_some_and(|&v| v & 2 != 0) && family == libc::AF_INET6;
        let port = |at| u16::from_be((word(&raw, at) & 0xffff) as u16);
        f(RawSocket {
            udp,
            v6,
            local: address(&raw, OFF_LADDR, v6),
            port: port(OFF_LPORT),
            remote: address(&raw, OFF_FADDR, v6),
            remote_port: port(OFF_FPORT),
            state: if tcp { word(&raw, OFF_TCP_STATE) } else { 0 },
        });
    }
}

// ---- IOKit and CoreFoundation: GPU, battery, disk statistics, temperature sensors --------------

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFRelease(cf: *const c_void);
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        s: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberGetValue(n: *const c_void, kind: isize, out: *mut c_void) -> u8;
    fn CFNumberCreate(alloc: *const c_void, kind: isize, value: *const c_void) -> *const c_void;
    fn CFBooleanGetValue(b: *const c_void) -> u8;
    fn CFDictionaryGetValue(d: *const c_void, key: *const c_void) -> *const c_void;
    fn CFDictionaryCreate(
        alloc: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFArrayGetCount(a: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(port: u32, matching: *mut c_void, it: *mut u32) -> c_int;
    fn IOIteratorNext(it: u32) -> u32;
    fn IOObjectRelease(object: u32) -> c_int;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: *const c_void,
        alloc: *const c_void,
        options: u32,
    ) -> *const c_void;
    fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, it: *mut u32) -> c_int;
    // The temperature sensors of Apple silicon are HID services; these calls are not in the
    // public headers but are exported by the framework.
    fn IOHIDEventSystemClientCreate(alloc: *const c_void) -> *const c_void;
    fn IOHIDEventSystemClientSetMatching(client: *const c_void, matching: *const c_void) -> c_int;
    fn IOHIDEventSystemClientCopyServices(client: *const c_void) -> *const c_void;
    fn IOHIDServiceClientCopyProperty(service: *const c_void, key: *const c_void) -> *const c_void;
    fn IOHIDServiceClientCopyEvent(
        service: *const c_void,
        kind: i64,
        options: i32,
        timeout: i64,
    ) -> *const c_void;
    fn IOHIDEventGetFloatValue(event: *const c_void, field: i32) -> f64;
}

const UTF8: u32 = 0x0800_0100;
const NUMBER_I64: isize = 4;

/// An owned CoreFoundation object, released when dropped.
struct Cf(*const c_void);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Create or Copy call and is released once.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string(s: &CStr) -> Cf {
    // SAFETY: `s` is a valid NUL-terminated string.
    Cf(unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), UTF8) })
}

fn cf_i64(v: *const c_void) -> Option<i64> {
    if v.is_null() {
        return None;
    }
    let mut n: i64 = 0;
    // SAFETY: the type is checked first; the value is written into `n`.
    let ok = unsafe {
        CFGetTypeID(v) == CFNumberGetTypeID()
            && CFNumberGetValue(v, NUMBER_I64, std::ptr::addr_of_mut!(n).cast()) != 0
    };
    ok.then_some(n)
}

fn cf_bool(v: *const c_void) -> Option<bool> {
    // SAFETY: the type is checked before the value is read.
    (!v.is_null() && unsafe { CFGetTypeID(v) == CFBooleanGetTypeID() })
        // SAFETY: `v` is a CFBoolean, as just checked.
        .then(|| unsafe { CFBooleanGetValue(v) != 0 })
}

fn cf_text(v: *const c_void) -> Option<String> {
    if v.is_null() {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    // SAFETY: the type is checked first; the buffer holds 256 chars.
    let ok = unsafe {
        CFGetTypeID(v) == CFStringGetTypeID()
            && CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, UTF8) != 0
    };
    ok.then(|| c_text(&buf))
}

/// A node of the I/O registry, released when dropped.
pub struct Entry(u32);

impl Drop for Entry {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the object came from an iterator and is released once.
            unsafe { IOObjectRelease(self.0) };
        }
    }
}

impl Entry {
    /// The value of a property, as a CoreFoundation object (null when there is none).
    fn property(&self, key: &CStr) -> Cf {
        let k = cf_string(key);
        // SAFETY: the entry and the key are valid; the result is owned by the caller.
        Cf(unsafe { IORegistryEntryCreateCFProperty(self.0, k.0, std::ptr::null(), 0) })
    }

    pub fn number(&self, key: &CStr) -> Option<i64> {
        cf_i64(self.property(key).0)
    }

    pub fn flag(&self, key: &CStr) -> Option<bool> {
        cf_bool(self.property(key).0)
    }

    pub fn text(&self, key: &CStr) -> Option<String> {
        cf_text(self.property(key).0)
    }

    /// A number inside a dictionary property.
    pub fn nested_number(&self, dict: &CStr, key: &CStr) -> Option<i64> {
        let d = self.property(dict);
        if d.0.is_null() {
            return None;
        }
        let k = cf_string(key);
        // SAFETY: `d` is a dictionary property (a missing key gives null); the value is borrowed
        // from `d`, which outlives the read.
        cf_i64(unsafe { CFDictionaryGetValue(d.0, k.0) })
    }

    /// The registry nodes below this one.
    fn children(&self) -> Vec<Entry> {
        let mut it = 0u32;
        // SAFETY: `it` receives an iterator that is released below.
        let kr = unsafe { IORegistryEntryGetChildIterator(self.0, c"IOService".as_ptr(), &mut it) };
        let mut out = Vec::new();
        if kr != 0 {
            return out;
        }
        let it = Entry(it);
        loop {
            // SAFETY: `it` is a live iterator.
            let next = unsafe { IOIteratorNext(it.0) };
            if next == 0 {
                break;
            }
            out.push(Entry(next));
        }
        out
    }
}

/// Every registry node of a class (and its subclasses).
pub fn services(class: &CStr) -> Vec<Entry> {
    // SAFETY: the name is a valid string; the matching dictionary is consumed by the next call.
    let matching = unsafe { IOServiceMatching(class.as_ptr()) };
    if matching.is_null() {
        return Vec::new();
    }
    let mut it = 0u32;
    // SAFETY: port 0 is the default main port; `it` receives an iterator that is released below.
    if unsafe { IOServiceGetMatchingServices(0, matching, &mut it) } != 0 {
        return Vec::new();
    }
    let it = Entry(it);
    let mut out = Vec::new();
    loop {
        // SAFETY: `it` is a live iterator.
        let next = unsafe { IOIteratorNext(it.0) };
        if next == 0 {
            break;
        }
        out.push(Entry(next));
    }
    out
}

/// A graphics accelerator: its name and its use now.
pub struct Accelerator {
    entry: Entry,
    pub name: String,
}

pub fn accelerators() -> Vec<Accelerator> {
    services(c"IOAccelerator")
        .into_iter()
        .map(|entry| {
            let name = entry.text(c"model").unwrap_or_else(|| "GPU".to_string());
            Accelerator { entry, name }
        })
        .collect()
}

impl Accelerator {
    /// The share of the time the device was busy, 0 to 100.
    pub fn utilization(&self) -> Option<f32> {
        let n = self
            .entry
            .nested_number(c"PerformanceStatistics", c"Device Utilization %")?;
        Some(n.clamp(0, 100) as f32)
    }
}

/// What the battery's registry node says.
pub struct BatteryReading {
    pub percent: f32,
    pub charging: bool,
    pub full: bool,
    pub external: bool,
    pub watts: Option<f32>,
    pub minutes: Option<u32>,
}

pub struct SmartBattery(Entry);

pub fn smart_battery() -> Option<SmartBattery> {
    services(c"AppleSmartBattery")
        .into_iter()
        .next()
        .map(SmartBattery)
}

impl SmartBattery {
    pub fn read(&self) -> Option<BatteryReading> {
        let e = &self.0;
        if e.flag(c"BatteryInstalled") == Some(false) {
            return None;
        }
        let cur = e.number(c"CurrentCapacity")? as f32;
        let max = e.number(c"MaxCapacity").filter(|&m| m > 0)? as f32;
        let amps = e
            .number(c"InstantAmperage")
            .or_else(|| e.number(c"Amperage"));
        let volts = e.number(c"Voltage");
        let watts = amps
            .zip(volts)
            .map(|(a, v)| (a as f64 * v as f64 / 1e6).abs() as f32);
        let minutes = e
            .number(c"TimeRemaining")
            .and_then(|m| u32::try_from(m).ok())
            .filter(|&m| m < 65_535);
        Some(BatteryReading {
            percent: (cur / max * 100.0).clamp(0.0, 100.0),
            charging: e.flag(c"IsCharging") == Some(true),
            full: e.flag(c"FullyCharged") == Some(true),
            external: e.flag(c"ExternalConnected") == Some(true),
            watts,
            minutes,
        })
    }
}

/// A block storage driver: the whole disks below it (`disk0`, and the APFS container `disk3`
/// that lives on it) and the bytes it has read and written.
pub struct BlockDriver {
    entry: Entry,
    pub whole: Vec<String>,
}

pub fn block_drivers() -> Vec<BlockDriver> {
    fn walk(e: &Entry, depth: u32, out: &mut Vec<String>) {
        if e.flag(c"Whole") == Some(true) {
            if let Some(name) = e.text(c"BSD Name") {
                out.push(name);
            }
        }
        if depth < 8 {
            for c in e.children() {
                walk(&c, depth + 1, out);
            }
        }
    }
    services(c"IOBlockStorageDriver")
        .into_iter()
        .map(|entry| {
            let mut whole = Vec::new();
            walk(&entry, 0, &mut whole);
            BlockDriver { entry, whole }
        })
        .collect()
}

impl BlockDriver {
    /// Bytes read and written since the driver started.
    pub fn bytes(&self) -> Option<(u64, u64)> {
        let r = self.entry.nested_number(c"Statistics", c"Bytes (Read)")?;
        let w = self.entry.nested_number(c"Statistics", c"Bytes (Write)")?;
        Some((u64::try_from(r).ok()?, u64::try_from(w).ok()?))
    }
}

/// The temperature sensors: HID services of the usage page 0xff00, usage 5.
pub struct Thermometers {
    _client: Cf,
    services: Cf,
    pub names: Vec<String>,
}

impl Thermometers {
    pub fn open() -> Option<Thermometers> {
        // SAFETY: a null allocator is the default one.
        let client = Cf(unsafe { IOHIDEventSystemClientCreate(std::ptr::null()) });
        if client.0.is_null() {
            return None;
        }
        let key_page = cf_string(c"PrimaryUsagePage");
        let key_usage = cf_string(c"PrimaryUsage");
        let (page, usage) = (0xff00i32, 5i32);
        // SAFETY: the numbers are copied from the pointers given.
        let (n_page, n_usage) = unsafe {
            (
                Cf(CFNumberCreate(
                    std::ptr::null(),
                    3,
                    std::ptr::addr_of!(page).cast(),
                )),
                Cf(CFNumberCreate(
                    std::ptr::null(),
                    3,
                    std::ptr::addr_of!(usage).cast(),
                )),
            )
        };
        let keys = [key_page.0, key_usage.0];
        let values = [n_page.0, n_usage.0];
        // SAFETY: two keys and two values with the standard type callbacks, which are statics
        // of the framework used by address only.
        let matching = Cf(unsafe {
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                2,
                std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
                std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
            )
        });
        if matching.0.is_null() {
            return None;
        }
        // SAFETY: the client and the matching dictionary are valid.
        unsafe { IOHIDEventSystemClientSetMatching(client.0, matching.0) };
        // SAFETY: the client is valid; the array is owned by the caller.
        let services = Cf(unsafe { IOHIDEventSystemClientCopyServices(client.0) });
        if services.0.is_null() {
            return None;
        }
        let product = cf_string(c"Product");
        // SAFETY: the array is valid.
        let count = unsafe { CFArrayGetCount(services.0) };
        let names = (0..count)
            .map(|i| {
                // SAFETY: `i` is below the count; the service is borrowed from the array.
                let s = unsafe { CFArrayGetValueAtIndex(services.0, i) };
                // SAFETY: `s` and `product` are valid; the result is owned here.
                let name = Cf(unsafe { IOHIDServiceClientCopyProperty(s, product.0) });
                cf_text(name.0).unwrap_or_default()
            })
            .collect();
        Some(Thermometers {
            _client: client,
            services,
            names,
        })
    }

    /// The temperature of sensor `i` in degrees, when it gives a sane one (above 0, below 150).
    pub fn celsius(&self, i: usize) -> Option<f32> {
        // SAFETY: the array is valid; an index past its end gives null, which is checked.
        let s = unsafe { CFArrayGetValueAtIndex(self.services.0, isize::try_from(i).ok()?) };
        if s.is_null() {
            return None;
        }
        // SAFETY: 15 is the temperature event type; the event is owned here.
        let ev = Cf(unsafe { IOHIDServiceClientCopyEvent(s, 15, 0, 0) });
        if ev.0.is_null() {
            return None;
        }
        // SAFETY: the field 15 << 16 is the temperature level of a temperature event.
        let v = unsafe { IOHIDEventGetFloatValue(ev.0, 15 << 16) };
        (v.is_finite() && v > 0.0 && v < 150.0).then_some(v as f32)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn arguments_come_after_the_program_path_and_its_padding() {
        let mut b = 3i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/bin/echo\0\0\0echo\0hello\0world\0HOME=/x\0");
        assert_eq!(join_args(&b), "echo hello world");
        assert_eq!(join_args(&[1, 0]), "");
    }

    #[test]
    fn this_process_is_known_to_the_kernel() {
        let pid = std::process::id();
        assert!(task_info(pid).is_some_and(|t| t.pti_threadnum > 0));
        let b = bsd_info(pid).unwrap();
        assert_eq!(b.pbi_pid, pid);
        let (b2, t2) = task_all(pid).unwrap();
        assert_eq!((b2.pbi_pid, t2.pti_threadnum > 0), (pid, true));
        assert!(!bsd_name(&b).is_empty());
        assert!(command_line(pid).is_some_and(|c| !c.is_empty()));
        assert!(exe_path(pid).is_some());
        assert!(usage(pid).is_some_and(|u| u.footprint > 0));
    }

    #[test]
    fn the_process_table_holds_this_process_and_launchd() {
        let mut buf = Vec::new();
        let n = kinfo_all(&mut buf);
        let all: Vec<Kp> = (0..n).filter_map(|i| kinfo_at(&buf, i)).collect();
        let me = std::process::id();
        let mine = all.iter().find(|k| k.pid == me).unwrap();
        assert_eq!(mine.uid, uid());
        assert!(mine.start > 1_600_000_000_000_000);
        assert!(!mine.comm().is_empty());
        let init = all.iter().find(|k| k.pid == 1).unwrap();
        assert_eq!(
            (init.comm().as_str(), init.ppid, init.uid),
            ("launchd", 0, 0)
        );
        assert_eq!(kinfo_pid(1).map(|k| k.pid), Some(1));
        assert!(kinfo_pid(u32::MAX).is_none());
    }

    #[test]
    fn a_struct_that_does_not_fit_is_refused() {
        assert!(task_info(u32::MAX).is_none());
        assert!(task_all(u32::MAX).is_none());
        assert!(bsd_info(4_000_000).is_none());
    }
}
