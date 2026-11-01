//! Finds the root process of an app, which process-loopback capture targets.
//!
//! Browsers and Electron apps play audio from a child process, so capture asks
//! for the whole tree under the app's main (root) process.

use std::path::PathBuf;

use windows::{
    core::PWSTR,
    Win32::{
        Foundation::CloseHandle,
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
            },
            Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    },
};

/// A running process.
#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub parent_pid: u32,
    pub exe: String,
}

/// The app's main process for `pid`: its highest ancestor running the same program.
///
/// ```no_run
/// use kiku::processes::{root_of, snapshot};
/// let processes = snapshot();
/// let main_process = root_of(1234, &processes);
/// ```
pub fn root_of(pid: u32, processes: &[Process]) -> Option<&Process> {
    let find = |pid: u32| processes.iter().find(|p| p.pid == pid);
    let mut current = find(pid)?;
    // Bounded, in case reused PIDs ever form a parent cycle.
    for _ in 0..processes.len() {
        match find(current.parent_pid) {
            Some(parent) if parent.exe.eq_ignore_ascii_case(&current.exe) => current = parent,
            _ => break,
        }
    }
    Some(current)
}

/// Every running process.
pub fn snapshot() -> Vec<Process> {
    let mut processes = Vec::new();
    let Ok(handle) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return processes;
    };
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut more = unsafe { Process32FirstW(handle, &mut entry) }.is_ok();
    while more {
        processes.push(Process {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            exe: utf16_until_nul(&entry.szExeFile),
        });
        more = unsafe { Process32NextW(handle, &mut entry) }.is_ok();
    }
    unsafe { CloseHandle(handle).ok() };
    processes
}

/// Full path of a process's executable, if this process may query it.
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut len = buffer.len() as u32;
    let queried = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len) };
    unsafe { CloseHandle(process).ok() };
    queried.ok()?;
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize])))
}

fn utf16_until_nul(chars: &[u16]) -> String {
    let len = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..len])
}
