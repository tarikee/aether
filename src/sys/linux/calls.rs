//! The only place with `unsafe`: the kernel calls the readers cannot do with files alone.
//! Each wrapper is safe to call and never panics; a failed call is `None` or an `Err`.
#![allow(unsafe_code)]

use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};

/// Total and used bytes of the file system at `path`, by `statvfs`: total = blocks x frsize,
/// used = (blocks - free blocks) x frsize. `None` if the call fails.
pub fn space(path: &CStr) -> Option<(u64, u64)> {
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated path and `st` is a struct statvfs we own.
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize: u64 = st.f_frsize;
    let total = st.f_blocks.checked_mul(frsize)?;
    let used = st.f_blocks.saturating_sub(st.f_bfree).checked_mul(frsize)?;
    Some((total, used))
}

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

/// The page size in bytes (4096 if the system does not say).
pub fn page_size() -> u64 {
    sysconf(libc::_SC_PAGESIZE, 4096)
}

/// The user id this process runs as.
pub fn uid() -> u32 {
    // SAFETY: `getuid` takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Clock ticks per second as the kernel counts them (100 if the system does not say).
pub fn ticks_per_second() -> u64 {
    sysconf(libc::_SC_CLK_TCK, 100)
}

fn sysconf(name: libc::c_int, fallback: u64) -> u64 {
    // SAFETY: `sysconf` reads no memory of ours.
    let value = unsafe { libc::sysconf(name) };
    u64::try_from(value)
        .ok()
        .filter(|v| *v > 0)
        .unwrap_or(fallback)
}

/// Raises the open-file limit from its soft value to its hard value. Errors are ignored: the
/// readers fall back to opening and closing files when the limit is low.
pub fn raise_files() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes into the struct we pass.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return;
    }
    if lim.rlim_cur < lim.rlim_max {
        lim.rlim_cur = lim.rlim_max;
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
    Some((tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32))
}

/// Whether a stop signal has come since `catch_stops`.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
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
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) };
        out.push((name.to_string_lossy().into_owned(), text));
    }
    // SAFETY: `head` came from getifaddrs and is freed exactly once.
    unsafe { libc::freeifaddrs(head) };
}

/// The terminal settings of stdin as they were before the keys took it.
#[derive(Clone, Copy)]
pub struct Settings(libc::termios);

/// Stdin in raw mode for the keys: no echo, no line editing, no signal keys. `None` when stdin
/// is not a terminal.
pub fn raw_input() -> Option<Settings> {
    let mut now: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut now) } != 0 {
        return None;
    }
    let saved = Settings(now);
    now.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
    now.c_iflag &= !(libc::IXON | libc::ICRNL);
    now.c_cc[libc::VMIN] = 0;
    now.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &now) } != 0 {
        return None;
    }
    Some(saved)
}

/// Puts the terminal back as `saved` has it.
pub fn restore_input(saved: &Settings) {
    let _ = unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved.0) };
}

/// Waits up to `ms` for input on stdin; false at the timeout, or when a signal ends the wait.
pub fn input_ready(ms: i32) -> bool {
    let mut fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut fd, 1, ms) > 0 }
}

/// Reads what is waiting on stdin: `Some(0)` at its end, `None` on an error (a signal's too).
pub fn read_input(buf: &mut [u8]) -> Option<usize> {
    let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
    usize::try_from(n).ok()
}
