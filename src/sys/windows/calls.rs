//! The only place with `unsafe` on Windows: the system calls the readers cannot do otherwise.
//! Every function hands back plain values; no pointer from the system outlives its call.

#![allow(unsafe_code)]

use std::mem::{size_of, zeroed};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows_sys::Wdk::Graphics::Direct3D::{
    D3DKMTCloseAdapter, D3DKMTEnumAdapters2, D3DKMTQueryAdapterInfo, D3DKMTQueryStatistics,
    D3DKMT_ADAPTERINFO, D3DKMT_ADAPTERREGISTRYINFO, D3DKMT_ADAPTERTYPE, D3DKMT_CLOSEADAPTER,
    D3DKMT_ENUMADAPTERS2, D3DKMT_QUERYADAPTERINFO, D3DKMT_QUERYSTATISTICS,
    D3DKMT_QUERYSTATISTICS_ADAPTER, D3DKMT_QUERYSTATISTICS_NODE, D3DKMT_QUERYSTATISTICS_SEGMENT,
    D3DKMT_QUERY_DEVICE_IDS, D3DKMT_SEGMENTSIZEINFO, KMTQAITYPE_ADAPTERREGISTRYINFO,
    KMTQAITYPE_ADAPTERTYPE, KMTQAITYPE_GETSEGMENTSIZE, KMTQAITYPE_PHYSICALADAPTERDEVICEIDS,
};
use windows_sys::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemProcessInformation, SystemProcessorPerformanceInformation,
};
use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessCommandLineInformation,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, BOOL, ERROR_ACCESS_DENIED, ERROR_BUFFER_OVERFLOW, FILETIME, HANDLE,
    HWND, INVALID_HANDLE_VALUE, LPARAM, LUID, NO_ERROR, STATUS_INFO_LENGTH_MISMATCH, SYSTEMTIME,
    UNICODE_STRING, WAIT_OBJECT_0,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetAdaptersAddresses, GetExtendedTcpTable, GetExtendedUdpTable, GetIfEntry2,
    GetIfTable2, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2, MIB_IF_TABLE2,
    MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID,
    TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, LookupAccountSidW, TokenUser, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, FILE_SHARE_READ,
    FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle,
    SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP, CONSOLE_SCREEN_BUFFER_INFO,
    CTRL_BREAK_EVENT, CTRL_C_EVENT, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX,
};
use windows_sys::Win32::System::Ioctl::{
    DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE, VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::Power::{
    CallNtPowerInformation, GetSystemPowerStatus, ProcessorInformation, SYSTEM_POWER_STATUS,
};
use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows_sys::Win32::System::SystemInformation::{
    GetLocalTime, GetSystemInfo, GetSystemTimeAsFileTime, GetTickCount64, SYSTEM_INFO,
};
use windows_sys::Win32::System::Threading::{
    GetActiveProcessorCount, GetProcessTimes, GetSystemTimes, OpenProcess, OpenProcessToken,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, ALL_PROCESSOR_GROUPS,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};
use windows_sys::Win32::System::WindowsProgramming::{
    SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION, SYSTEM_PROCESS_INFORMATION, SYSTEM_THREAD_INFORMATION,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};

// ---------------------------------------------------------------------------------------------
// Handles, buffers and strings

/// A handle that is closed when dropped.
pub struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open and is closed once, here.
        unsafe { CloseHandle(self.0) };
    }
}

fn owned(h: HANDLE) -> Option<Handle> {
    (!h.is_null() && h != INVALID_HANDLE_VALUE).then_some(Handle(h))
}

/// `text` as a NUL-ended UTF-16 string, as the system calls take it.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A UTF-16 buffer up to its first NUL, as text.
pub fn from_wide(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// The memory of `words` as bytes.
fn bytes_of(words: &[u64]) -> &[u8] {
    // SAFETY: the same memory, read as bytes; every byte of a u64 is initialised.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), words.len() * 8) }
}

/// The UTF-16 text a system pointer names, if it lies wholly inside `words`.
fn units_in(words: &[u64], ptr: usize, len_bytes: usize) -> Option<&[u16]> {
    let base = words.as_ptr() as usize;
    let end = base + words.len() * 8;
    if ptr < base || !ptr.is_multiple_of(2) || ptr.checked_add(len_bytes)? > end {
        return None;
    }
    // SAFETY: the range was checked to lie inside `words`, and it is 2-aligned.
    Some(unsafe { std::slice::from_raw_parts(ptr as *const u16, len_bytes / 2) })
}

/// A plain-data structure at `at` in `bytes`, or `None` when it does not fit.
fn read_at<T: Copy>(bytes: &[u8], at: usize) -> Option<T> {
    let end = at.checked_add(size_of::<T>())?;
    if end > bytes.len() {
        return None;
    }
    // SAFETY: the bytes are in bounds; `T` is one of the system's plain-data structures, for
    // which any bit pattern is valid.
    Some(unsafe { std::ptr::read_unaligned(bytes.as_ptr().add(at).cast::<T>()) })
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_ne_bytes)
}

fn filetime(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

const ZERO_TIME: FILETIME = FILETIME {
    dwLowDateTime: 0,
    dwHighDateTime: 0,
};

/// The system information class of the page files (the bindings do not name it).
const SYSTEM_PAGE_FILE_INFORMATION: i32 = 18;

/// Fills `buf` with the system table `class`, growing `buf` while the table does not fit.
/// The number of bytes filled, or `None` when the call fails.
fn system_table(class: i32, buf: &mut Vec<u64>) -> Option<usize> {
    for _ in 0..4 {
        let size = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        let mut need = 0u32;
        // SAFETY: the buffer is `size` bytes long and writable; `need` is a valid out pointer.
        let status =
            unsafe { NtQuerySystemInformation(class, buf.as_mut_ptr().cast(), size, &mut need) };
        if status >= 0 {
            return Some((need as usize).min(buf.len() * 8));
        }
        if status != STATUS_INFO_LENGTH_MISMATCH {
            return None;
        }
        // Room for what starts between this call and the next.
        let bytes = (need as usize).max(buf.len() * 8) + 64 * 1024;
        buf.resize(bytes / 8 + 1, 0);
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The machine: processors, memory, uptime

/// The logical processors of all groups together.
pub fn thread_count() -> u32 {
    // SAFETY: no pointers.
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
}

/// The logical processors of this program's processor group (at most 64).
pub fn group_threads() -> usize {
    // SAFETY: an all-zero SYSTEM_INFO is valid; the call fills it.
    let mut info: SYSTEM_INFO = unsafe { zeroed() };
    // SAFETY: `info` is a valid out pointer.
    unsafe { GetSystemInfo(&mut info) };
    info.dwNumberOfProcessors as usize
}

/// The whole machine's processor times: (idle, kernel with idle, user), in 100 ns.
pub fn system_times() -> Option<[u64; 3]> {
    let (mut idle, mut kernel, mut user) = (ZERO_TIME, ZERO_TIME, ZERO_TIME);
    // SAFETY: three valid out pointers.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0;
    ok.then(|| [filetime(idle), filetime(kernel), filetime(user)])
}

/// Each logical processor of this program's group: (idle, kernel with idle, user), in 100 ns.
/// `buf` is kept by the caller between readings.
pub fn processor_times(buf: &mut Vec<u64>, out: &mut Vec<[u64; 3]>) -> bool {
    out.clear();
    let Some(filled) = system_table(SystemProcessorPerformanceInformation, buf) else {
        return false;
    };
    let stride = size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>() / 8;
    for i in 0..filled / (stride * 8) {
        if let Some(w) = buf.get(i * stride..i * stride + 3) {
            out.push([w[0], w[1], w[2]]);
        }
    }
    true
}

/// The current clock of each logical processor in MHz, as the power manager reports it.
pub fn processor_mhz(buf: &mut Vec<u64>, threads: usize, out: &mut Vec<u32>) -> bool {
    out.clear();
    // PROCESSOR_POWER_INFORMATION: six u32 values, the current clock third.
    let bytes = threads * 24;
    buf.resize(bytes.div_ceil(8), 0);
    let size = u32::try_from(bytes).unwrap_or(u32::MAX);
    // SAFETY: the output buffer is at least `size` bytes long.
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            null(),
            0,
            buf.as_mut_ptr().cast(),
            size,
        )
    };
    if status < 0 {
        return false;
    }
    let raw = bytes_of(buf);
    out.extend((0..threads).map(|i| u32_at(raw, i * 24 + 8)));
    true
}

/// Physical memory in bytes: (total, available, system cache).
pub fn memory() -> Option<(u64, u64, u64)> {
    // SAFETY: an all-zero PERFORMANCE_INFORMATION is valid; the call fills it.
    let mut p: PERFORMANCE_INFORMATION = unsafe { zeroed() };
    p.cb = size_of::<PERFORMANCE_INFORMATION>() as u32;
    // SAFETY: `p` is a valid out pointer of the size given.
    if unsafe { GetPerformanceInfo(&mut p, p.cb) } == 0 {
        return None;
    }
    let page = p.PageSize as u64;
    Some((
        p.PhysicalTotal as u64 * page,
        p.PhysicalAvailable as u64 * page,
        p.SystemCache as u64 * page,
    ))
}

/// The memory page size in bytes.
pub fn page_size() -> u64 {
    // SAFETY: an all-zero SYSTEM_INFO is valid; the call fills it.
    let mut info: SYSTEM_INFO = unsafe { zeroed() };
    // SAFETY: `info` is a valid out pointer.
    unsafe { GetSystemInfo(&mut info) };
    u64::from(info.dwPageSize)
}

/// The page files together: (total, in use), in pages.
pub fn page_files(buf: &mut Vec<u64>) -> Option<(u64, u64)> {
    let filled = system_table(SYSTEM_PAGE_FILE_INFORMATION, buf)?;
    let raw = &bytes_of(buf)[..filled];
    // Each entry: next offset, total size, in use, peak (u32 each), then the file's name.
    let (mut total, mut used, mut at) = (0u64, 0u64, 0usize);
    while at + 16 <= raw.len() {
        total += u64::from(u32_at(raw, at + 4));
        used += u64::from(u32_at(raw, at + 8));
        let next = u32_at(raw, at) as usize;
        if next == 0 {
            break;
        }
        at += next;
    }
    Some((total, used))
}

/// Milliseconds since the machine started, sleep included.
pub fn uptime_ms() -> u64 {
    // SAFETY: no pointers.
    unsafe { GetTickCount64() }
}

/// The time now, in 100 ns since 1601.
pub fn now_100ns() -> u64 {
    let mut t = ZERO_TIME;
    // SAFETY: a valid out pointer.
    unsafe { GetSystemTimeAsFileTime(&mut t) };
    filetime(t)
}

// ---------------------------------------------------------------------------------------------
// Processes

/// One process from the kernel's table.
pub struct RawProc<'a> {
    pub pid: u32,
    pub ppid: u32,
    pub threads: u32,
    /// The image name in UTF-16, without a folder.
    pub name: &'a [u16],
    /// When it started, in 100 ns since 1601.
    pub created: u64,
    /// User and kernel time together, in 100 ns.
    pub cpu: u64,
    pub working_set: u64,
    pub private_working_set: u64,
    /// Bytes read and written through every kind of I/O.
    pub read_bytes: u64,
    pub write_bytes: u64,
    /// `R` a thread runs or is ready, `T` every thread is suspended, `Z` no thread is left,
    /// else `S`.
    pub state: u8,
}

/// Reads the kernel's process table into `buf` (kept by the caller) and gives each entry to
/// `each`. One system call for every process, whoever owns it. False when the call fails.
pub fn processes(buf: &mut Vec<u64>, mut each: impl FnMut(&RawProc<'_>)) -> bool {
    let Some(filled) = system_table(SystemProcessInformation, buf) else {
        return false;
    };
    let words: &[u64] = buf;
    let bytes = &bytes_of(words)[..filled];
    let head = size_of::<SYSTEM_PROCESS_INFORMATION>();
    let thread = size_of::<SYSTEM_THREAD_INFORMATION>();
    let mut at = 0usize;
    while let Some(p) = read_at::<SYSTEM_PROCESS_INFORMATION>(bytes, at) {
        // The first reserved bytes hold: private working set, hard faults, the highest thread
        // count, cycle time, then creation, user and kernel time (8 bytes each but the two
        // counts).
        let r = p.Reserved1;
        let word = |o: usize| {
            r.get(o..o + 8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map_or(0, u64::from_ne_bytes)
        };
        let name = units_in(
            words,
            p.ImageName.Buffer as usize,
            usize::from(p.ImageName.Length),
        )
        .unwrap_or(&[]);
        let mut state = if p.NumberOfThreads == 0 { b'Z' } else { b'S' };
        let mut suspended = 0u32;
        for i in 0..p.NumberOfThreads as usize {
            let Some(t) = read_at::<SYSTEM_THREAD_INFORMATION>(bytes, at + head + i * thread)
            else {
                break;
            };
            match t.ThreadState {
                // Ready, running, standby, deferred ready.
                1 | 2 | 3 | 7 => {
                    state = b'R';
                    break;
                }
                // Waiting, for the reason "suspended".
                5 if t.WaitReason == 5 => suspended += 1,
                _ => {}
            }
        }
        if state == b'S' && suspended == p.NumberOfThreads {
            state = b'T';
        }
        each(&RawProc {
            pid: p.UniqueProcessId as usize as u32,
            // The reserved pointer after the pid is the parent's pid.
            ppid: p.Reserved2 as usize as u32,
            threads: p.NumberOfThreads,
            name,
            created: word(24),
            cpu: word(32).saturating_add(word(40)),
            working_set: p.WorkingSetSize as u64,
            private_working_set: word(0),
            // The last reserved counters: read, write and other operations, then the bytes of
            // each.
            read_bytes: p.Reserved7[3] as u64,
            write_bytes: p.Reserved7[4] as u64,
            state,
        });
        if p.NextEntryOffset == 0 {
            break;
        }
        at += p.NextEntryOffset as usize;
    }
    true
}

fn open_process(pid: u32, access: u32) -> Option<Handle> {
    // SAFETY: no pointers; a failed open gives a null handle.
    owned(unsafe { OpenProcess(access, 0, pid) })
}

/// The SID of the account a process runs as, as bytes; `None` when its token cannot be read
/// (another account's process, without administrator rights).
pub fn process_sid(pid: u32) -> Option<Vec<u8>> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut token: HANDLE = null_mut();
    // SAFETY: a valid process handle and out pointer.
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = owned(token)?;
    // TOKEN_USER, then the SID it points to.
    let mut buf = [0u64; 32];
    let size = (buf.len() * 8) as u32;
    let mut len = 0u32;
    // SAFETY: the buffer is `size` bytes long; `len` is a valid out pointer.
    let ok =
        unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), size, &mut len) };
    if ok == 0 {
        return None;
    }
    let user: TOKEN_USER = read_at(bytes_of(&buf), 0)?;
    let sid = user.User.Sid;
    if sid.is_null() {
        return None;
    }
    // SAFETY: the system just wrote a valid SID at this address.
    let n = unsafe { GetLengthSid(sid) } as usize;
    let start = (sid as usize).checked_sub(buf.as_ptr() as usize)?;
    bytes_of(&buf)
        .get(start..start.checked_add(n)?)
        .map(<[u8]>::to_vec)
}

/// The account name of a SID, without its domain ("SYSTEM", "dana").
pub fn account_name(sid: &[u8]) -> Option<String> {
    // A SID is 8 bytes and four for each sub-authority; anything else is not passed on.
    let count = usize::from(*sid.get(1)?);
    if sid.len() != 8 + 4 * count {
        return None;
    }
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut name_len, mut domain_len) = (256u32, 256u32);
    let mut kind = 0;
    // SAFETY: the SID was checked for its length; the buffers and their lengths match.
    let ok = unsafe {
        LookupAccountSidW(
            null(),
            sid.as_ptr().cast_mut().cast(),
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    (ok != 0).then(|| from_wide(&name[..(name_len as usize).min(name.len())]))
}

/// The full path of a process's program.
pub fn image_path(pid: u32) -> Option<String> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer holds `len` units; `len` is a valid in-out pointer.
    let ok = unsafe {
        QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
    };
    (ok != 0).then(|| from_wide(&buf[..(len as usize).min(buf.len())]))
}

/// The command line a process was started with.
pub fn command_line(pid: u32) -> Option<String> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buf = vec![0u64; 512];
    for _ in 0..3 {
        let size = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        let mut need = 0u32;
        // SAFETY: the buffer is `size` bytes long; `need` is a valid out pointer.
        let status = unsafe {
            NtQueryInformationProcess(
                process.0,
                ProcessCommandLineInformation,
                buf.as_mut_ptr().cast(),
                size,
                &mut need,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            buf.resize(need as usize / 8 + 2, 0);
            continue;
        }
        if status < 0 {
            return None;
        }
        // A UNICODE_STRING whose text follows it in the same buffer.
        let s: UNICODE_STRING = read_at(bytes_of(&buf), 0)?;
        let text = units_in(&buf, s.Buffer as usize, usize::from(s.Length))?;
        return Some(String::from_utf16_lossy(text));
    }
    None
}

/// The processor time a process has used, user and kernel, in seconds.
pub fn cpu_seconds(pid: u32) -> Option<f64> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let (mut created, mut exited, mut kernel, mut user) =
        (ZERO_TIME, ZERO_TIME, ZERO_TIME, ZERO_TIME);
    // SAFETY: a valid handle and four valid out pointers.
    let ok =
        unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| (filetime(kernel) + filetime(user)) as f64 / 1e7)
}

/// Why a process could not be opened to end it.
pub enum OpenError {
    Gone,
    Denied,
}

/// A process opened to end it and to wait for it.
pub fn open_to_end(pid: u32) -> Result<Handle, OpenError> {
    let access = PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
    match open_process(pid, access) {
        Some(h) => Ok(h),
        // SAFETY: no pointers.
        None if unsafe { GetLastError() } == ERROR_ACCESS_DENIED => Err(OpenError::Denied),
        None => Err(OpenError::Gone),
    }
}

/// Waits up to `ms` for the process to end; true when it has.
pub fn wait_end(process: &Handle, ms: u32) -> bool {
    // SAFETY: a valid handle opened with SYNCHRONIZE.
    unsafe { WaitForSingleObject(process.0, ms) == WAIT_OBJECT_0 }
}

/// Ends the process at once.
pub fn terminate(process: &Handle) -> bool {
    // SAFETY: a valid handle opened with PROCESS_TERMINATE.
    unsafe { TerminateProcess(process.0, 1) != 0 }
}

struct Ask {
    pid: u32,
    visible_only: bool,
    asked: u32,
}

unsafe extern "system" fn ask_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `Ask` that `close_windows` passed, alive for the whole walk.
    let ask = unsafe { &mut *(lparam as *mut Ask) };
    let mut owner = 0u32;
    // SAFETY: a window handle from the walk and a valid out pointer.
    unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    // SAFETY: a window handle from the walk.
    let shown = unsafe { IsWindowVisible(hwnd) } != 0;
    // SAFETY: posting a message takes no pointers.
    if owner == ask.pid
        && (shown || !ask.visible_only)
        && unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } != 0
    {
        ask.asked += 1;
    }
    1
}

/// Asks a process to close as its own close button would: the visible top-level windows it
/// owns (else its hidden ones) are sent WM_CLOSE. The number of windows asked; a program
/// without a window gets none.
pub fn close_windows(pid: u32) -> u32 {
    for visible_only in [true, false] {
        let mut ask = Ask {
            pid,
            visible_only,
            asked: 0,
        };
        // SAFETY: the callback reads `ask` only during this call.
        unsafe { EnumWindows(Some(ask_window), &mut ask as *mut Ask as LPARAM) };
        if ask.asked > 0 {
            return ask.asked;
        }
    }
    0
}

// ---------------------------------------------------------------------------------------------
// Disks

/// The drive letters in use, one bit each from A.
pub fn drive_mask() -> u32 {
    // SAFETY: no pointers.
    unsafe { GetLogicalDrives() }
}

/// What kind of drive a root ("C:\" as NUL-ended UTF-16) is.
pub fn drive_type(root: &[u16]) -> u32 {
    if root.last() != Some(&0) {
        return 0;
    }
    // SAFETY: a NUL-ended string.
    unsafe { GetDriveTypeW(root.as_ptr()) }
}

/// A drive's space in bytes: (total, free).
pub fn space(root: &[u16]) -> Option<(u64, u64)> {
    if root.last() != Some(&0) {
        return None;
    }
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: a NUL-ended string and three valid out pointers.
    let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((total, free))
}

/// A drive without its medium answers with an error, not a dialog.
pub fn quiet_drive_errors() {
    // SAFETY: no pointers.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX) };
}

/// A device (`\\.\C:`, `\\.\PhysicalDrive0` as NUL-ended UTF-16) opened for queries only:
/// neither read nor write access, which needs no administrator rights.
pub fn open_device(path: &[u16]) -> Option<Handle> {
    if path.last() != Some(&0) {
        return None;
    }
    // SAFETY: a NUL-ended string; no security attributes or template.
    owned(unsafe {
        CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    })
}

/// The number of the physical disk a volume starts on.
pub fn disk_number(volume: &Handle) -> Option<u32> {
    // SAFETY: an all-zero VOLUME_DISK_EXTENTS is valid; the call fills it.
    let mut ext: VOLUME_DISK_EXTENTS = unsafe { zeroed() };
    let mut got = 0u32;
    // SAFETY: a valid handle, an output buffer of the size given, a valid out pointer.
    let ok = unsafe {
        DeviceIoControl(
            volume.0,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            null(),
            0,
            (&mut ext as *mut VOLUME_DISK_EXTENTS).cast(),
            size_of::<VOLUME_DISK_EXTENTS>() as u32,
            &mut got,
            null_mut(),
        )
    };
    (ok != 0 && ext.NumberOfDiskExtents > 0).then_some(ext.Extents[0].DiskNumber)
}

/// A disk's counters: bytes read and written, then idle time and the time of the query, in
/// 100 ns.
pub fn disk_counters(device: &Handle) -> Option<[u64; 4]> {
    // SAFETY: an all-zero DISK_PERFORMANCE is valid; the call fills it.
    let mut p: DISK_PERFORMANCE = unsafe { zeroed() };
    let mut got = 0u32;
    // SAFETY: a valid handle, an output buffer of the size given, a valid out pointer.
    let ok = unsafe {
        DeviceIoControl(
            device.0,
            IOCTL_DISK_PERFORMANCE,
            null(),
            0,
            (&mut p as *mut DISK_PERFORMANCE).cast(),
            size_of::<DISK_PERFORMANCE>() as u32,
            &mut got,
            null_mut(),
        )
    };
    (ok != 0).then_some([
        p.BytesRead as u64,
        p.BytesWritten as u64,
        p.IdleTime as u64,
        p.QueryTime as u64,
    ])
}

// ---------------------------------------------------------------------------------------------
// Network

/// The hardware network interfaces as (index, name); filter layers and loopback left out.
pub fn interfaces(out: &mut Vec<(u32, String)>) -> bool {
    out.clear();
    let mut table: *mut MIB_IF_TABLE2 = null_mut();
    // SAFETY: a valid out pointer; the table is freed below.
    if unsafe { GetIfTable2(&mut table) } != NO_ERROR || table.is_null() {
        return false;
    }
    // SAFETY: the table the system allocated: its count, then that many rows.
    let rows = unsafe {
        let n = (*table).NumEntries as usize;
        std::slice::from_raw_parts(std::ptr::addr_of!((*table).Table).cast::<MIB_IF_ROW2>(), n)
    };
    for row in rows {
        // Bit 0: a hardware interface; bit 1: a filter layer over one.
        let flags = row.InterfaceAndOperStatusFlags._bitfield;
        if flags & 1 == 0 || flags & 2 != 0 || row.Type == IF_TYPE_SOFTWARE_LOOPBACK {
            continue;
        }
        out.push((row.InterfaceIndex, from_wide(&row.Alias)));
    }
    // SAFETY: the table from GetIfTable2, freed once.
    unsafe { FreeMibTable(table.cast()) };
    true
}

/// An interface's bytes received and sent, and its link speed in bits per second.
pub fn octets(index: u32) -> Option<(u64, u64, Option<u64>)> {
    // SAFETY: an all-zero MIB_IF_ROW2 is valid; the call fills it.
    let mut row: MIB_IF_ROW2 = unsafe { zeroed() };
    row.InterfaceIndex = index;
    // SAFETY: a valid in-out pointer.
    let ok = unsafe { GetIfEntry2(&mut row) } == NO_ERROR;
    let speed = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
    let link = (speed != 0 && speed != u64::MAX).then_some(speed);
    ok.then_some((row.InOctets, row.OutOctets, link))
}

fn socket_ip(sa: *const SOCKADDR, len: i32) -> Option<IpAddr> {
    let len = usize::try_from(len).ok()?;
    if sa.is_null() || len < 2 {
        return None;
    }
    // SAFETY: the system gives the address with its length.
    let b = unsafe { std::slice::from_raw_parts(sa.cast::<u8>(), len) };
    let family = u16::from_ne_bytes([b[0], b[1]]);
    if family == AF_INET {
        let a: [u8; 4] = b.get(4..8)?.try_into().ok()?;
        Some(IpAddr::V4(Ipv4Addr::from(a)))
    } else if family == AF_INET6 {
        let a: [u8; 16] = b.get(8..24)?.try_into().ok()?;
        Some(IpAddr::V6(Ipv6Addr::from(a)))
    } else {
        None
    }
}

/// Every interface's unicast addresses as (interface index, address). `buf` is kept by the
/// caller.
pub fn addresses(buf: &mut Vec<u64>, out: &mut Vec<(u32, IpAddr)>) {
    out.clear();
    if buf.len() < 2048 {
        buf.resize(2048, 0);
    }
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut ok = false;
    for _ in 0..3 {
        let mut size = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        // SAFETY: the buffer is `size` bytes long; `size` is a valid in-out pointer.
        let r = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                flags,
                null(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if r == ERROR_BUFFER_OVERFLOW {
            buf.resize(size as usize / 8 + 1, 0);
            continue;
        }
        ok = r == NO_ERROR;
        break;
    }
    if !ok {
        return;
    }
    let mut cur = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !cur.is_null() {
        // SAFETY: a list the system wrote into `buf`, ended by a null link.
        let a = unsafe { &*cur };
        // SAFETY: both fields of the union are plain integers.
        let v4 = unsafe { a.Anonymous1.Anonymous.IfIndex };
        let index = if v4 != 0 { v4 } else { a.Ipv6IfIndex };
        let mut u = a.FirstUnicastAddress.cast_const();
        while !u.is_null() {
            // SAFETY: a list the system wrote into `buf`, ended by a null link.
            let ua = unsafe { &*u };
            if let Some(ip) = socket_ip(ua.Address.lpSockaddr, ua.Address.iSockaddrLength) {
                out.push((index, ip));
            }
            u = ua.Next;
        }
        cur = a.Next;
    }
}

// ---------------------------------------------------------------------------------------------
// Battery

/// The power state: (on mains, battery flags, percent, seconds left), as the system reports
/// them (255 and u32::MAX for unknown).
pub fn power_status() -> Option<(u8, u8, u8, u32)> {
    // SAFETY: an all-zero SYSTEM_POWER_STATUS is valid; the call fills it.
    let mut s: SYSTEM_POWER_STATUS = unsafe { zeroed() };
    // SAFETY: a valid out pointer.
    let ok = unsafe { GetSystemPowerStatus(&mut s) } != 0;
    ok.then_some((
        s.ACLineStatus,
        s.BatteryFlag,
        s.BatteryLifePercent,
        s.BatteryLifeTime,
    ))
}

// ---------------------------------------------------------------------------------------------
// Sockets

/// One row of the TCP or UDP table, as the system gives it.
pub struct RawSocket {
    pub udp: bool,
    pub local: IpAddr,
    pub port: u16,
    pub remote: IpAddr,
    pub remote_port: u16,
    /// The TCP state (1 closed ... 12 delete), 0 for UDP.
    pub state: u32,
    pub pid: u32,
}

/// The port of a table row: network byte order in the low 16 bits.
fn row_port(v: u32) -> u16 {
    u16::from_be(v as u16)
}

/// The system's own table of one kind, in a buffer of 8-byte units (so rows are aligned).
fn extended_table(udp: bool, v6: bool) -> Option<Vec<u64>> {
    let family = u32::from(if v6 { AF_INET6 } else { AF_INET });
    let mut size = 0u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..8 {
        let ptr = if size == 0 {
            null_mut()
        } else {
            buf.as_mut_ptr().cast()
        };
        // SAFETY: the buffer holds at least `size` bytes, or is null with size 0 to ask for it.
        let rc = unsafe {
            if udp {
                GetExtendedUdpTable(ptr, &mut size, 0, family, UDP_TABLE_OWNER_PID, 0)
            } else {
                GetExtendedTcpTable(ptr, &mut size, 0, family, TCP_TABLE_OWNER_PID_ALL, 0)
            }
        };
        if rc == NO_ERROR {
            return Some(buf);
        }
        // 122: the buffer is too small, and `size` says how much.
        if rc != 122 {
            return None;
        }
        buf = vec![0u64; (size as usize).div_ceil(8) + 8];
        size = (buf.len() * 8) as u32;
    }
    None
}

/// Every TCP and UDP socket with its owning pid, v4 and v6.
pub fn sockets(out: &mut Vec<RawSocket>) -> bool {
    out.clear();
    let mut ok = false;
    for udp in [false, true] {
        for v6 in [false, true] {
            let Some(buf) = extended_table(udp, v6) else {
                continue;
            };
            ok = true;
            let base = buf.as_ptr();
            // SAFETY: the table starts with its row count, then that many rows of one type,
            // all inside the buffer the system filled.
            unsafe {
                let n = *base.cast::<u32>() as usize;
                let rows = base.cast::<u8>().add(4);
                match (udp, v6) {
                    (false, false) => {
                        let r = std::slice::from_raw_parts(rows.cast::<MIB_TCPROW_OWNER_PID>(), n);
                        for r in r {
                            out.push(RawSocket {
                                udp,
                                local: IpAddr::V4(Ipv4Addr::from(u32::from_be(r.dwLocalAddr))),
                                port: row_port(r.dwLocalPort),
                                remote: IpAddr::V4(Ipv4Addr::from(u32::from_be(r.dwRemoteAddr))),
                                remote_port: row_port(r.dwRemotePort),
                                state: r.dwState,
                                pid: r.dwOwningPid,
                            });
                        }
                    }
                    (false, true) => {
                        let r = std::slice::from_raw_parts(rows.cast::<MIB_TCP6ROW_OWNER_PID>(), n);
                        for r in r {
                            out.push(RawSocket {
                                udp,
                                local: IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)),
                                port: row_port(r.dwLocalPort),
                                remote: IpAddr::V6(Ipv6Addr::from(r.ucRemoteAddr)),
                                remote_port: row_port(r.dwRemotePort),
                                state: r.dwState,
                                pid: r.dwOwningPid,
                            });
                        }
                    }
                    (true, false) => {
                        let r = std::slice::from_raw_parts(rows.cast::<MIB_UDPROW_OWNER_PID>(), n);
                        for r in r {
                            out.push(RawSocket {
                                udp,
                                local: IpAddr::V4(Ipv4Addr::from(u32::from_be(r.dwLocalAddr))),
                                port: row_port(r.dwLocalPort),
                                remote: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                                remote_port: 0,
                                state: 0,
                                pid: r.dwOwningPid,
                            });
                        }
                    }
                    (true, true) => {
                        let r = std::slice::from_raw_parts(rows.cast::<MIB_UDP6ROW_OWNER_PID>(), n);
                        for r in r {
                            out.push(RawSocket {
                                udp,
                                local: IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)),
                                port: row_port(r.dwLocalPort),
                                remote: IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                                remote_port: 0,
                                state: 0,
                                pid: r.dwOwningPid,
                            });
                        }
                    }
                }
            }
        }
    }
    ok
}

// ---------------------------------------------------------------------------------------------
// Graphics adapters, through the kernel's graphics statistics (what Task Manager reads)

/// One graphics adapter.
pub struct Adapter {
    pub luid: LUID,
    pub name: String,
    pub vendor_id: u32,
    /// Dedicated video memory in bytes (0 when it has none of its own).
    pub vram_total: u64,
    /// It draws (not a display-only device).
    pub render: bool,
    /// It is the processor drawing in software (the basic render driver).
    pub software: bool,
}

fn adapter_info<T: Copy>(adapter: u32, kind: i32, value: &mut T) -> bool {
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: adapter,
        Type: kind,
        pPrivateDriverData: (value as *mut T).cast(),
        PrivateDriverDataSize: size_of::<T>() as u32,
    };
    // SAFETY: the query points at `value`, of the size given.
    unsafe { D3DKMTQueryAdapterInfo(&mut q) >= 0 }
}

/// Every graphics adapter with its name, vendor and dedicated memory.
pub fn adapters(out: &mut Vec<Adapter>) {
    out.clear();
    let mut e = D3DKMT_ENUMADAPTERS2 {
        NumAdapters: 0,
        pAdapters: null_mut(),
    };
    // SAFETY: with no list, the call only counts the adapters.
    if unsafe { D3DKMTEnumAdapters2(&mut e) } < 0 || e.NumAdapters == 0 {
        return;
    }
    // SAFETY: an all-zero D3DKMT_ADAPTERINFO is valid.
    let mut list = vec![unsafe { zeroed::<D3DKMT_ADAPTERINFO>() }; e.NumAdapters as usize];
    e.pAdapters = list.as_mut_ptr();
    // SAFETY: the list holds `NumAdapters` entries.
    if unsafe { D3DKMTEnumAdapters2(&mut e) } < 0 {
        return;
    }
    for info in list.iter().take(e.NumAdapters as usize) {
        // SAFETY: all-zero answers are valid; each query fills its own.
        let mut ty: D3DKMT_ADAPTERTYPE = unsafe { zeroed() };
        let mut reg: D3DKMT_ADAPTERREGISTRYINFO = unsafe { zeroed() };
        let mut size: D3DKMT_SEGMENTSIZEINFO = unsafe { zeroed() };
        let mut ids: D3DKMT_QUERY_DEVICE_IDS = unsafe { zeroed() };
        let bits = if adapter_info(info.hAdapter, KMTQAITYPE_ADAPTERTYPE, &mut ty) {
            // SAFETY: the union is one u32 either way.
            unsafe { ty.Anonymous.Value }
        } else {
            0
        };
        let named = adapter_info(info.hAdapter, KMTQAITYPE_ADAPTERREGISTRYINFO, &mut reg);
        let sized = adapter_info(info.hAdapter, KMTQAITYPE_GETSEGMENTSIZE, &mut size);
        let known = adapter_info(info.hAdapter, KMTQAITYPE_PHYSICALADAPTERDEVICEIDS, &mut ids);
        let close = D3DKMT_CLOSEADAPTER {
            hAdapter: info.hAdapter,
        };
        // SAFETY: an adapter handle from the walk, closed once.
        unsafe { D3DKMTCloseAdapter(&close) };
        out.push(Adapter {
            luid: info.AdapterLuid,
            name: if named {
                from_wide(&reg.AdapterString)
            } else {
                String::new()
            },
            vendor_id: if known { ids.DeviceIds.VendorID } else { 0 },
            vram_total: if sized {
                size.DedicatedVideoMemorySize
            } else {
                0
            },
            // Bit 0: render supported; bit 2: a software device.
            render: bits & 1 != 0,
            software: bits & 4 != 0,
        });
    }
}

fn statistics(luid: LUID, kind: i32, id: u32) -> Option<D3DKMT_QUERYSTATISTICS> {
    // SAFETY: an all-zero query is valid; the call fills its result.
    let mut q: D3DKMT_QUERYSTATISTICS = unsafe { zeroed() };
    q.Type = kind;
    q.AdapterLuid = luid;
    if kind == D3DKMT_QUERYSTATISTICS_NODE {
        q.Anonymous.QueryNode.NodeId = id;
    } else if kind == D3DKMT_QUERYSTATISTICS_SEGMENT {
        q.Anonymous.QuerySegment.SegmentId = id;
    }
    // SAFETY: the query is valid and writable; the call writes its result into it.
    let status = unsafe { D3DKMTQueryStatistics(std::ptr::addr_of_mut!(q).cast_const()) };
    (status >= 0).then_some(q)
}

/// An adapter's engines (nodes) and memory segments.
pub fn gpu_layout(luid: LUID) -> Option<(u32, u32)> {
    let q = statistics(luid, D3DKMT_QUERYSTATISTICS_ADAPTER, 0)?;
    // SAFETY: the adapter query fills the adapter part of the result.
    let a = unsafe { q.QueryResult.AdapterInformation };
    Some((a.NodeCount, a.NbSegments))
}

/// The time an engine has run, in 100 ns.
pub fn gpu_node_time(luid: LUID, node: u32) -> Option<u64> {
    let q = statistics(luid, D3DKMT_QUERYSTATISTICS_NODE, node)?;
    // SAFETY: the node query fills the node part of the result.
    let t = unsafe { q.QueryResult.NodeInformation.GlobalInformation.RunningTime };
    u64::try_from(t).ok()
}

/// A memory segment's resident bytes, and whether it is system memory seen through an aperture
/// (not the card's own).
pub fn gpu_segment(luid: LUID, segment: u32) -> Option<(u64, bool)> {
    let q = statistics(luid, D3DKMT_QUERYSTATISTICS_SEGMENT, segment)?;
    // SAFETY: the segment query fills the segment part of the result.
    let s = unsafe { q.QueryResult.SegmentInformation };
    Some((s.BytesResident, s.Aperture != 0))
}

// ---------------------------------------------------------------------------------------------
// The console: escape codes, UTF-8, its size, and Ctrl-C

static STOP: AtomicBool = AtomicBool::new(false);
static HANDLER: AtomicBool = AtomicBool::new(false);
static SAVED: AtomicBool = AtomicBool::new(false);
static SAVED_MODE: AtomicU32 = AtomicU32::new(0);
static SAVED_CP: AtomicU32 = AtomicU32::new(0);

const UTF8: u32 = 65001;

fn stdout_handle() -> HANDLE {
    // SAFETY: no pointers.
    unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }
}

/// Puts the console's mode and code page back, once.
fn restore_console() {
    if SAVED.swap(false, Ordering::SeqCst) {
        // SAFETY: no pointers; a handle that is not a console makes the calls fail, harmlessly.
        unsafe {
            SetConsoleMode(stdout_handle(), SAVED_MODE.load(Ordering::SeqCst));
            SetConsoleOutputCP(SAVED_CP.load(Ordering::SeqCst));
        }
    }
}

unsafe extern "system" fn on_ctrl(kind: u32) -> BOOL {
    STOP.store(true, Ordering::SeqCst);
    if kind != CTRL_C_EVENT && kind != CTRL_BREAK_EVENT {
        // The console closes, or the user logs off: the program may end before its loop does.
        restore_console();
    }
    1
}

/// Ctrl-C, Ctrl-Break and a closing console ask the program to stop instead of ending it, so
/// the console is put back first.
pub fn catch_stops() {
    if !HANDLER.swap(true, Ordering::SeqCst) {
        // SAFETY: a handler that only touches atomics and the console mode.
        unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    }
}

/// The local time of day as (hour, minute, second).
pub fn local_hms() -> Option<(u32, u32, u32)> {
    // SAFETY: GetLocalTime fills the zeroed struct it is given.
    let t: SYSTEMTIME = unsafe {
        let mut t: SYSTEMTIME = zeroed();
        GetLocalTime(&mut t);
        t
    };
    Some((
        u32::from(t.wHour),
        u32::from(t.wMinute),
        u32::from(t.wSecond),
    ))
}

/// Whether Ctrl-C (or the console closing) has asked the program to stop.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// The console set for the run: escape codes on, UTF-8 out. Put back as it was when dropped,
/// and by the Ctrl-C handler when the console closes.
pub struct Console(());

impl Drop for Console {
    fn drop(&mut self) {
        restore_console();
    }
}

/// Sets standard output's console for escape codes and UTF-8; `None` when it is not a console
/// or cannot take escape codes.
pub fn console_open() -> Option<Console> {
    let out = stdout_handle();
    let mut mode = 0u32;
    // SAFETY: a valid out pointer; a handle that is not a console makes the call fail.
    if out.is_null() || unsafe { GetConsoleMode(out, &mut mode) } == 0 {
        return None;
    }
    // SAFETY: no pointers.
    let cp = unsafe { GetConsoleOutputCP() };
    SAVED_MODE.store(mode, Ordering::SeqCst);
    SAVED_CP.store(cp, Ordering::SeqCst);
    SAVED.store(true, Ordering::SeqCst);
    catch_stops();
    let want = mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    // SAFETY: no pointers.
    if unsafe { SetConsoleMode(out, want) } == 0 {
        restore_console();
        return None;
    }
    // SAFETY: no pointers.
    unsafe { SetConsoleOutputCP(UTF8) };
    Some(Console(()))
}

/// The console window's (rows, columns) on standard output.
pub fn term_size() -> Option<(u16, u16)> {
    // SAFETY: an all-zero CONSOLE_SCREEN_BUFFER_INFO is valid; the call fills it.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { zeroed() };
    // SAFETY: a valid out pointer.
    if unsafe { GetConsoleScreenBufferInfo(stdout_handle(), &mut info) } == 0 {
        return None;
    }
    let w = info.srWindow;
    let rows = u16::try_from(i32::from(w.Bottom) - i32::from(w.Top) + 1).ok()?;
    let cols = u16::try_from(i32::from(w.Right) - i32::from(w.Left) + 1).ok()?;
    (rows > 0 && cols > 0).then_some((rows, cols))
}
