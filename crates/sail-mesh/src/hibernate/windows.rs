use std::ffi::OsString;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, RECT};

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSuspendProcess(h: HANDLE) -> i32;
    fn NtResumeProcess(h: HANDLE) -> i32;
}
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
    PROCESS_SUSPEND_RESUME, PROCESS_VM_READ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetDesktopWindow, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
};

use super::{ProcInfo, ProcessControl};

pub struct WindowsProcessControl;

impl WindowsProcessControl {
    pub fn new() -> Self {
        Self
    }
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

fn process_start_and_ws(pid: u32) -> (u64, u64) {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if h.is_null() {
        return (0, 0);
    }
    let _guard = OwnedHandle(h);
    let mut created: FILETIME = unsafe { zeroed() };
    let mut exit: FILETIME = unsafe { zeroed() };
    let mut kernel: FILETIME = unsafe { zeroed() };
    let mut user: FILETIME = unsafe { zeroed() };
    let start_time =
        if unsafe { GetProcessTimes(h, &mut created, &mut exit, &mut kernel, &mut user) } != 0 {
            filetime_to_u64(created)
        } else {
            0
        };

    let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { zeroed() };
    pmc.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ws = if unsafe { GetProcessMemoryInfo(h, &mut pmc, pmc.cb) } != 0 {
        pmc.WorkingSetSize as u64
    } else {
        0
    };
    (start_time, ws)
}

fn foreground_info() -> (Option<u32>, bool) {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_null() {
        return (None, false);
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(fg, &mut pid) };
    let mut fg_rect: RECT = unsafe { zeroed() };
    let mut desk_rect: RECT = unsafe { zeroed() };
    let desk = unsafe { GetDesktopWindow() };
    let full = if unsafe { GetWindowRect(fg, &mut fg_rect) } != 0
        && !desk.is_null()
        && unsafe { GetWindowRect(desk, &mut desk_rect) } != 0
    {
        fg_rect.left <= desk_rect.left
            && fg_rect.top <= desk_rect.top
            && fg_rect.right >= desk_rect.right
            && fg_rect.bottom >= desk_rect.bottom
    } else {
        false
    };
    (if pid > 0 { Some(pid) } else { None }, full)
}

impl ProcessControl for WindowsProcessControl {
    fn list(&mut self) -> Vec<ProcInfo> {
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap == (-1isize as HANDLE) || snap.is_null() {
            return Vec::new();
        }
        let _guard = OwnedHandle(snap);
        let (fg_pid, fg_full) = foreground_info();

        let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        if unsafe { Process32FirstW(snap, &mut entry) } == 0 {
            return Vec::new();
        }

        let mut out = Vec::new();
        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = OsString::from_wide(&entry.szExeFile[..len])
                .to_string_lossy()
                .into_owned();
            let pid = entry.th32ProcessID;
            let (start_time, working_set) = process_start_and_ws(pid);
            let is_fg = fg_pid == Some(pid);
            out.push(ProcInfo {
                pid,
                parent: if entry.th32ParentProcessID > 0 {
                    Some(entry.th32ParentProcessID)
                } else {
                    None
                },
                name,
                start_time,
                working_set,
                full_screen: is_fg && fg_full,
                foreground: is_fg,
            });
            if unsafe { Process32NextW(snap, &mut entry) } == 0 {
                break;
            }
        }
        out
    }

    fn suspend(&mut self, pid: u32) -> io::Result<()> {
        let h = unsafe { OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _guard = OwnedHandle(h);
        // SAFETY: `h` is a valid process handle opened with `PROCESS_SUSPEND_RESUME` and closed by `_guard`.
        let status = unsafe { NtSuspendProcess(h) };
        if status >= 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }

    fn resume(&mut self, pid: u32) -> io::Result<()> {
        let h = unsafe { OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _guard = OwnedHandle(h);
        // SAFETY: `h` is a valid process handle opened with `PROCESS_SUSPEND_RESUME` and closed by `_guard`.
        let status = unsafe { NtResumeProcess(h) };
        if status >= 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }

    fn trim(&mut self, pid: u32) -> io::Result<u64> {
        let h = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_QUOTA | PROCESS_VM_READ,
                0,
                pid,
            )
        };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _guard = OwnedHandle(h);
        let mut before: PROCESS_MEMORY_COUNTERS = unsafe { zeroed() };
        before.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let before_ws = if unsafe { GetProcessMemoryInfo(h, &mut before, before.cb) } != 0 {
            before.WorkingSetSize as u64
        } else {
            0
        };
        if unsafe { EmptyWorkingSet(h) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut after: PROCESS_MEMORY_COUNTERS = unsafe { zeroed() };
        after.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let after_ws = if unsafe { GetProcessMemoryInfo(h, &mut after, after.cb) } != 0 {
            after.WorkingSetSize as u64
        } else {
            before_ws
        };
        Ok(before_ws.saturating_sub(after_ws))
    }

    fn free_commit(&mut self) -> Option<u64> {
        let mut mem: MEMORYSTATUSEX = unsafe { zeroed() };
        mem.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut mem) } != 0 {
            Some(mem.ullAvailPageFile)
        } else {
            None
        }
    }
}
