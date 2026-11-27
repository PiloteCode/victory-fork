//! The few system calls the friendly commands make (`bench`, `doctor`,
//! `check`): the local time, closing or ending a run, memory, free space,
//! the processes running.

use std::path::Path;

/// The local date and time: year, month, day, hour, minute, second.
#[cfg(windows)]
pub(crate) fn local_time() -> [u16; 6] {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: fills the structure it is given.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    [t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond]
}

/// The date and time (UTC where the local time is not asked for).
#[cfg(not(windows))]
pub(crate) fn local_time() -> [u16; 6] {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil from days (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    [year as u16, month as u16, day as u16, (rest / 3600) as u16, (rest / 60 % 60) as u16, (rest % 60) as u16]
}

/// Asks the visible top-level windows of process `pid` to close (as their
/// close button does: the title suspends, then the run ends): how many.
#[cfg(windows)]
pub(crate) fn close_windows(pid: u32) -> usize {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    };
    use windows_sys::core::BOOL;
    struct Search {
        pid: u32,
        asked: usize,
    }
    unsafe extern "system" fn each(hwnd: HWND, search: LPARAM) -> BOOL {
        // SAFETY: the `Search` `close_windows` passes, alive for the call.
        let search = unsafe { &mut *(search as *mut Search) };
        let mut owner = 0;
        // SAFETY: plain queries on a window the system enumerates; posting
        // a close asks, it does not wait.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut owner);
            if owner == search.pid && IsWindowVisible(hwnd) != 0 && PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0 {
                search.asked += 1;
            }
        }
        1
    }
    let mut search = Search { pid, asked: 0 };
    // SAFETY: `each` reads `search` only during the call.
    unsafe { EnumWindows(Some(each), &raw mut search as LPARAM) };
    search.asked
}

#[cfg(not(windows))]
pub(crate) fn close_windows(_: u32) -> usize {
    0
}

/// Ends process `pid` at once: whether it could be.
#[cfg(windows)]
pub(crate) fn kill(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: a handle opened, used and closed here.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return false;
        }
        let ended = TerminateProcess(process, 1) != 0;
        CloseHandle(process);
        ended
    }
}

#[cfg(not(windows))]
pub(crate) fn kill(_: u32) -> bool {
    false
}

/// Physical memory and commit, in bytes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Memory {
    pub total: u64,
    pub free: u64,
    pub commit_limit: u64,
    pub commit_free: u64,
}

#[cfg(windows)]
pub(crate) fn memory() -> Option<Memory> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: a sized structure for the call to fill.
    (unsafe { GlobalMemoryStatusEx(&mut m) } != 0).then_some(Memory {
        total: m.ullTotalPhys,
        free: m.ullAvailPhys,
        commit_limit: m.ullTotalPageFile,
        commit_free: m.ullAvailPageFile,
    })
}

#[cfg(not(windows))]
pub(crate) fn memory() -> Option<Memory> {
    None
}

/// Bytes free (for this user) and in all on the drive of `path`, or of its
/// nearest folder that exists.
#[cfg(windows)]
pub(crate) fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let there = path.ancestors().find(|p| p.exists())?;
    let wide: Vec<u16> = there.as_os_str().encode_wide().chain([0]).collect();
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: a NUL-terminated path and two counts to fill.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) } != 0;
    ok.then_some((free, total))
}

#[cfg(not(windows))]
pub(crate) fn disk_space(_: &Path) -> Option<(u64, u64)> {
    None
}

/// The other processes whose executable is one of `names` (case ignored):
/// id and name.
#[cfg(windows)]
pub(crate) fn processes_named(names: &[&str]) -> Vec<(u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    let me = std::process::id();
    let mut out = Vec::new();
    // SAFETY: a snapshot opened, walked and closed here; each entry is sized.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..std::mem::zeroed()
        };
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if entry.th32ProcessID != me && names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                out.push((entry.th32ProcessID, name));
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    out
}

#[cfg(not(windows))]
pub(crate) fn processes_named(_: &[&str]) -> Vec<(u32, String)> {
    Vec::new()
}

/// Whether the processor has AVX2, which the title's code needs.
pub(crate) fn avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Bytes as GB with one decimal.
pub(crate) fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_local_time_is_a_date() {
        let [year, month, day, hour, minute, second] = super::local_time();
        assert!(year >= 2024 && (1..=12).contains(&month) && (1..=31).contains(&day));
        assert!(hour < 24 && minute < 60 && second < 61);
    }

    #[test]
    fn this_process_is_not_listed_as_another() {
        let me = std::env::current_exe().unwrap();
        let name = me.file_name().unwrap().to_string_lossy().into_owned();
        assert!(super::processes_named(&[&name]).iter().all(|(pid, _)| *pid != std::process::id()));
    }

    #[test]
    fn sizes_read_in_gigabytes() {
        assert_eq!(super::gb(31_500_000_000), "31.5 GB");
        assert!(super::disk_space(&std::env::temp_dir()).is_none_or(|(free, total)| free <= total));
    }
}
