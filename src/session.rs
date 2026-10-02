//! Everything that depends on which Windows session a process lives in.
//!
//! The installed daemon is a service in session 0, where nothing can be shown
//! and no input is seen. It therefore starts an agent inside the user's
//! session to draw the lock screen and report what the user is doing.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{IntoRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::process::Command;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, TOKEN_LINKED_TOKEN,
    TokenElevationType, TokenElevationTypeLimited, TokenLinkedToken, TokenPrimary,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::RemoteDesktop::{
    WTSDisconnectSession, WTSGetActiveConsoleSessionId, WTSQueryUserToken,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcessId, OpenProcess,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::paths::Paths;
use crate::{enforce, wide};

pub const DAEMON_EXE: &str = "despegated.exe";
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
const NO_SESSION: u32 = 0xFFFF_FFFF;

/// `despegated.exe` lives next to whichever of our binaries is running.
pub fn daemon_exe() -> io::Result<PathBuf> {
    Ok(std::env::current_exe()?.with_file_name(DAEMON_EXE))
}

pub fn current_session() -> u32 {
    enforce::session_of(unsafe { GetCurrentProcessId() }).unwrap_or(0)
}

/// The session attached to the physical screen and keyboard, if any.
pub fn console_session() -> Option<u32> {
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    (session != NO_SESSION).then_some(session)
}

/// Sends `session` back to the Windows sign-in screen without logging it off.
pub fn disconnect(session: u32) {
    unsafe { WTSDisconnectSession(null_mut(), session, 0) };
}

/// A running agent process.
pub struct Agent {
    pub pid: u32,
    pub session: u32,
    handle: HANDLE,
}

impl Agent {
    pub fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
    }

    pub fn kill(&self) {
        unsafe { TerminateProcess(self.handle, 0) };
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Starts the agent as an ordinary child process in this same session.
pub fn spawn_agent_here(paths: &Paths) -> io::Result<Agent> {
    let child = Command::new(daemon_exe()?)
        .arg("agent")
        .args(paths.args())
        .spawn()?;
    let pid = child.id();
    Ok(Agent {
        pid,
        session: current_session(),
        handle: OwnedHandle::from(child).into_raw_handle(),
    })
}

/// Starts the agent in the console session, as the user logged on there.
/// Only a service running as SYSTEM may do this. Fails while nobody is logged on.
pub fn spawn_agent_in_console_session() -> io::Result<Agent> {
    let session = console_session().ok_or_else(|| io::Error::other("no console session"))?;
    let exe = wide(&daemon_exe()?.to_string_lossy());
    let mut command_line = wide(&format!("\"{}\" agent", daemon_exe()?.display()));
    let mut desktop = wide(r"winsta0\default");

    unsafe {
        let mut user: HANDLE = null_mut();
        if WTSQueryUserToken(session, &mut user) == 0 {
            return Err(io::Error::last_os_error());
        }
        // An administrator's session token is the filtered one. Its elevated
        // twin makes the agent out of reach of ordinary processes, and lets
        // its keyboard hook see keys pressed in elevated windows.
        let source = elevated_twin(user).unwrap_or(user);
        let mut primary: HANDLE = null_mut();
        let duplicated = DuplicateTokenEx(
            source,
            MAXIMUM_ALLOWED,
            null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        );
        let duplicate_error = io::Error::last_os_error();
        if source != user {
            CloseHandle(source);
        }
        CloseHandle(user);
        if duplicated == 0 {
            return Err(duplicate_error);
        }

        let mut environment: *mut c_void = null_mut();
        if CreateEnvironmentBlock(&mut environment, primary, 0) == 0 {
            environment = null_mut();
        }
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        startup.lpDesktop = desktop.as_mut_ptr();
        let mut process: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessAsUserW(
            primary,
            exe.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_UNICODE_ENVIRONMENT,
            environment,
            null(),
            &startup,
            &mut process,
        );
        let create_error = io::Error::last_os_error();
        if !environment.is_null() {
            DestroyEnvironmentBlock(environment);
        }
        CloseHandle(primary);
        if created == 0 {
            return Err(create_error);
        }
        CloseHandle(process.hThread);
        Ok(Agent {
            pid: process.dwProcessId,
            session,
            handle: process.hProcess,
        })
    }
}

/// The full administrator token linked to a UAC-filtered one, if there is one.
unsafe fn elevated_twin(token: HANDLE) -> Option<HANDLE> {
    unsafe {
        let mut kind = 0i32;
        let mut len = 0u32;
        let known = GetTokenInformation(
            token,
            TokenElevationType,
            &mut kind as *mut i32 as *mut c_void,
            size_of::<i32>() as u32,
            &mut len,
        );
        if known == 0 || kind != TokenElevationTypeLimited {
            return None;
        }
        let mut linked: TOKEN_LINKED_TOKEN = std::mem::zeroed();
        let found = GetTokenInformation(
            token,
            TokenLinkedToken,
            &mut linked as *mut TOKEN_LINKED_TOKEN as *mut c_void,
            size_of::<TOKEN_LINKED_TOKEN>() as u32,
            &mut len,
        );
        (found != 0 && !linked.LinkedToken.is_null()).then_some(linked.LinkedToken)
    }
}

/// Seconds since the last keyboard or mouse input in the calling session.
pub fn idle_seconds() -> u64 {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    unsafe {
        if GetLastInputInfo(&mut info) == 0 {
            return 0;
        }
        (GetTickCount().wrapping_sub(info.dwTime) / 1000) as u64
    }
}

/// Lowercase executable name of the program whose window is in the foreground.
pub fn foreground_app() -> Option<String> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut path = [0u16; 1024];
        let mut len = path.len() as u32;
        let found = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if found == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&path[..len as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_lowercase)
    }
}
