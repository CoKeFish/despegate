use std::collections::HashSet;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_TERMINATE, TerminateProcess,
};

/// Process id and lowercase executable name of everything running in `session`.
/// Other sessions belong to other users and are none of our business.
fn processes(session: u32) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return found;
    }
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
        if session_of(entry.th32ProcessID) == Some(session) {
            found.push((entry.th32ProcessID, name));
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    found
}

/// Terminates every process in `session` whose executable name is in `names`
/// (lowercase). Returns the names that were actually closed.
pub fn close_apps(names: &HashSet<String>, session: u32) -> Vec<String> {
    let mut closed = Vec::new();
    if names.is_empty() {
        return closed;
    }
    let me = unsafe { GetCurrentProcessId() };
    for (pid, name) in processes(session) {
        if names.contains(&name) && pid != me && terminate(pid) && !closed.contains(&name) {
            closed.push(name);
        }
    }
    closed
}

/// Which of `names` are running in `session`.
pub fn running(names: &[String], session: u32) -> Vec<String> {
    if names.is_empty() {
        return Vec::new();
    }
    let present: HashSet<String> = processes(session)
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    names
        .iter()
        .filter(|name| present.contains(*name))
        .cloned()
        .collect()
}

pub fn session_of(pid: u32) -> Option<u32> {
    let mut session = 0;
    (unsafe { ProcessIdToSessionId(pid, &mut session) } != 0).then_some(session)
}

fn terminate(pid: u32) -> bool {
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return false;
        }
        let ok = TerminateProcess(process, 1) != 0;
        CloseHandle(process);
        ok
    }
}
