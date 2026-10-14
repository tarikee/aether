//! The only place with `unsafe`: the kernel calls the readers cannot do with files alone.
//! Each wrapper is safe to call and never panics; a failed call is `None` or an `Err`.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

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
    Some((
        u32::try_from(tm.tm_hour).ok()?,
        u32::try_from(tm.tm_min).ok()?,
        u32::try_from(tm.tm_sec).ok()?,
    ))
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

// NVIDIA: NVML, loaded at run time and never linked. Written from NVML's documented interface.

/// NVML's answers that the readers act on.
const NVML_OK: c_int = 0;
const NVML_NOT_FOUND: c_int = 6;
const NVML_INSUFFICIENT_SIZE: c_int = 7;

/// An NVML device handle, kept as an address so that it can be shared between threads.
pub type NvDevice = usize;

/// `nvmlUtilization_t`: percent busy of the GPU and of its memory.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct NvUtilization {
    pub gpu: c_uint,
    pub memory: c_uint,
}

/// `nvmlMemory_t`: the card's VRAM in bytes.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct NvMemory {
    pub total: u64,
    pub free: u64,
    pub used: u64,
}

/// `nvmlProcessInfo_v1_t`: one process on a card. `used_gpu_memory` is `u64::MAX` when unknown.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct NvProcInfo {
    pub pid: c_uint,
    pub used_gpu_memory: u64,
    pub gpu_instance_id: c_uint,
    pub compute_instance_id: c_uint,
}

/// `nvmlProcessUtilizationSample_t`: one process's use of the card, in percent.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct NvSample {
    pub pid: c_uint,
    pub time_stamp: u64,
    pub sm_util: c_uint,
    pub mem_util: c_uint,
    pub enc_util: c_uint,
    pub dec_util: c_uint,
}

type InitFn = unsafe extern "C" fn() -> c_int;
type CountFn = unsafe extern "C" fn(*mut c_uint) -> c_int;
type HandleFn = unsafe extern "C" fn(c_uint, *mut *mut c_void) -> c_int;
type NameFn = unsafe extern "C" fn(*mut c_void, *mut c_char, c_uint) -> c_int;
type UtilFn = unsafe extern "C" fn(*mut c_void, *mut NvUtilization) -> c_int;
type MemoryFn = unsafe extern "C" fn(*mut c_void, *mut NvMemory) -> c_int;
type TempFn = unsafe extern "C" fn(*mut c_void, c_int, *mut c_uint) -> c_int;
type PowerFn = unsafe extern "C" fn(*mut c_void, *mut c_uint) -> c_int;
type ProcsFn = unsafe extern "C" fn(*mut c_void, *mut c_uint, *mut NvProcInfo) -> c_int;
type SamplesFn = unsafe extern "C" fn(*mut c_void, *mut NvSample, *mut c_uint, u64) -> c_int;

/// An open NVML library with its functions, for the whole run.
pub struct Nvml {
    /// The library handle, kept open (as an address) for the run.
    _lib: usize,
    count: CountFn,
    handle: HandleFn,
    name: NameFn,
    utilization: UtilFn,
    memory: MemoryFn,
    temperature: TempFn,
    power: PowerFn,
    compute: ProcsFn,
    graphics: ProcsFn,
    /// Missing on some drivers: then the per-process percent is unknown.
    samples: Option<SamplesFn>,
}

/// The NVML session of this run: loaded on first use, and never tried again. `None` when the
/// library or one of its functions is missing, or NVML does not start.
pub fn nvml() -> Option<&'static Nvml> {
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    NVML.get_or_init(|| open_lib(c"libnvidia-ml.so.1")).as_ref()
}

/// Opens the library `name` and starts NVML from it. The library is closed again on failure.
fn open_lib(name: &CStr) -> Option<Nvml> {
    // SAFETY: `name` is NUL-terminated; a null result is handled below.
    let lib = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
    if lib.is_null() {
        return None;
    }
    let nvml = from_lib(lib);
    if nvml.is_none() {
        // SAFETY: `lib` is the open handle from dlopen above, closed once.
        unsafe { libc::dlclose(lib) };
    }
    nvml
}

/// The function `name` of the open library `lib`, as the function pointer type `F`.
fn func<F: Copy>(lib: *mut c_void, name: &CStr) -> Option<F> {
    if std::mem::size_of::<F>() != std::mem::size_of::<*mut c_void>() {
        return None;
    }
    // SAFETY: `lib` is an open library handle and `name` is NUL-terminated.
    let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller names a function pointer type `F`, the same size as the address `p`,
    // and the symbol is that function. Both are pointer-sized, so the bytes of `p` are its value.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) })
}

fn from_lib(lib: *mut c_void) -> Option<Nvml> {
    let init: InitFn = func(lib, c"nvmlInit_v2")?;
    // SAFETY: nvmlInit_v2 takes no arguments.
    if unsafe { init() } != NVML_OK {
        return None;
    }
    Some(Nvml {
        _lib: lib as usize,
        count: func(lib, c"nvmlDeviceGetCount_v2")?,
        handle: func(lib, c"nvmlDeviceGetHandleByIndex_v2")?,
        name: func(lib, c"nvmlDeviceGetName")?,
        utilization: func(lib, c"nvmlDeviceGetUtilizationRates")?,
        memory: func(lib, c"nvmlDeviceGetMemoryInfo")?,
        temperature: func(lib, c"nvmlDeviceGetTemperature")?,
        power: func(lib, c"nvmlDeviceGetPowerUsage")?,
        compute: func(lib, c"nvmlDeviceGetComputeRunningProcesses_v3")?,
        graphics: func(lib, c"nvmlDeviceGetGraphicsRunningProcesses_v3")?,
        samples: func(lib, c"nvmlDeviceGetProcessUtilization"),
    })
}

impl Nvml {
    /// How many devices NVML knows.
    pub fn device_count(&self) -> Option<u32> {
        let mut n: c_uint = 0;
        // SAFETY: NVML writes one unsigned int, which we pass.
        (unsafe { (self.count)(&mut n) } == NVML_OK).then_some(n)
    }

    /// The handle of device `index`.
    pub fn device(&self, index: u32) -> Option<NvDevice> {
        let mut h: *mut c_void = std::ptr::null_mut();
        // SAFETY: NVML writes one device handle, which we pass.
        if unsafe { (self.handle)(index, &mut h) } != NVML_OK || h.is_null() {
            return None;
        }
        Some(h as NvDevice)
    }

    /// The device's name, as NVML has it (at most 96 bytes).
    pub fn name(&self, dev: NvDevice) -> Option<String> {
        let mut buf = [0 as c_char; 96];
        // SAFETY: NVML writes at most `buf.len()` bytes into `buf`.
        if unsafe { (self.name)(dev as *mut c_void, buf.as_mut_ptr(), buf.len() as c_uint) }
            != NVML_OK
        {
            return None;
        }
        let bytes: Vec<u8> = buf
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as u8)
            .collect();
        Some(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// The device's percent busy, GPU and memory.
    pub fn utilization(&self, dev: NvDevice) -> Option<NvUtilization> {
        let mut u = NvUtilization::default();
        // SAFETY: NVML writes one NvUtilization, which we pass.
        (unsafe { (self.utilization)(dev as *mut c_void, &mut u) } == NVML_OK).then_some(u)
    }

    /// The device's VRAM.
    pub fn memory(&self, dev: NvDevice) -> Option<NvMemory> {
        let mut m = NvMemory::default();
        // SAFETY: NVML writes one NvMemory, which we pass.
        (unsafe { (self.memory)(dev as *mut c_void, &mut m) } == NVML_OK).then_some(m)
    }

    /// The GPU's temperature in degrees Celsius.
    pub fn temperature(&self, dev: NvDevice) -> Option<u32> {
        let mut t: c_uint = 0;
        // SAFETY: NVML writes one unsigned int, which we pass; sensor 0 is the GPU.
        (unsafe { (self.temperature)(dev as *mut c_void, 0, &mut t) } == NVML_OK).then_some(t)
    }

    /// The card's power draw in milliwatts.
    pub fn power_mw(&self, dev: NvDevice) -> Option<u32> {
        let mut mw: c_uint = 0;
        // SAFETY: NVML writes one unsigned int, which we pass.
        (unsafe { (self.power)(dev as *mut c_void, &mut mw) } == NVML_OK).then_some(mw)
    }

    /// The compute (or, if `graphics`, graphics) processes on `dev`, into `out`. False when NVML
    /// does not say; `out` is then empty.
    pub fn processes(&self, dev: NvDevice, graphics: bool, out: &mut Vec<NvProcInfo>) -> bool {
        let f = if graphics {
            self.graphics
        } else {
            self.compute
        };
        let mut cap: c_uint = 16;
        for _ in 0..4 {
            out.clear();
            out.resize(cap as usize, NvProcInfo::default());
            let mut count = cap;
            // SAFETY: `out` holds `cap` structs, and NVML writes at most `count` (= `cap`) of them.
            let rc = unsafe { f(dev as *mut c_void, &mut count, out.as_mut_ptr()) };
            if rc == NVML_OK {
                out.truncate(count.min(cap) as usize);
                return true;
            }
            if rc != NVML_INSUFFICIENT_SIZE {
                break;
            }
            // On this answer NVML puts the size it needs in `count`.
            cap = count.max(cap.saturating_mul(2));
        }
        out.clear();
        false
    }

    /// The process samples on `dev` newer than `since`, into `out`. False when NVML does not say
    /// (or has no such function). No samples at all is true with an empty `out`.
    pub fn samples(&self, dev: NvDevice, since: u64, out: &mut Vec<NvSample>) -> bool {
        let Some(f) = self.samples else {
            return false;
        };
        let mut cap: c_uint = 64;
        for _ in 0..4 {
            out.clear();
            out.resize(cap as usize, NvSample::default());
            let mut count = cap;
            // SAFETY: `out` holds `cap` structs, and NVML writes at most `count` (= `cap`) of them.
            let rc = unsafe { f(dev as *mut c_void, out.as_mut_ptr(), &mut count, since) };
            if rc == NVML_OK {
                out.truncate(count.min(cap) as usize);
                return true;
            }
            if rc == NVML_NOT_FOUND {
                out.clear();
                return true;
            }
            if rc != NVML_INSUFFICIENT_SIZE {
                break;
            }
            cap = count.max(cap.saturating_mul(2));
        }
        out.clear();
        false
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn the_nvml_structs_have_the_layout_of_the_library() {
        assert_eq!(size_of::<NvUtilization>(), 8);
        assert_eq!(size_of::<NvMemory>(), 24);
        assert_eq!(offset_of!(NvMemory, used), 16);
        assert_eq!(size_of::<NvProcInfo>(), 24);
        assert_eq!(offset_of!(NvProcInfo, used_gpu_memory), 8);
        assert_eq!(offset_of!(NvProcInfo, compute_instance_id), 20);
        assert_eq!(size_of::<NvSample>(), 32);
        assert_eq!(offset_of!(NvSample, time_stamp), 8);
        assert_eq!(offset_of!(NvSample, sm_util), 16);
        assert_eq!(offset_of!(NvSample, dec_util), 28);
    }

    #[test]
    fn a_missing_library_gives_no_nvml_session() {
        assert!(open_lib(c"libaether-no-such-library.so.0").is_none());
    }
}
