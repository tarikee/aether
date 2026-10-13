//! The only place with `unsafe`: the kernel calls the readers cannot do with files alone.
//! Each wrapper is safe to call and never panics; a failed call is `None` or an `Err`.
#![allow(unsafe_code)]

use std::ffi::CStr;

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

/// The rows of the terminal on standard output, if it is one.
pub fn term_rows() -> Option<u16> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `TIOCGWINSZ` writes one winsize struct, which we pass.
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    (ok && ws.ws_row > 0).then_some(ws.ws_row)
}
