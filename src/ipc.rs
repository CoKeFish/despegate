//! One JSON request line and one JSON response line per named-pipe connection.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::ptr::null_mut;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, RevertToSelf, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::i18n::Lang;
use crate::service::{Call, Request, Response};
use crate::wide;

/// SYSTEM and administrators get full control; interactive users may connect
/// and exchange data but not create pipe instances of their own. The explicit
/// DACL matters because the installed daemon runs as SYSTEM while the CLI and
/// the agent run as the user.
const PIPE_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12019b;;;IU)";
const SDDL_REVISION_1: u32 = 1;
const ERROR_PIPE_BUSY: i32 = 231;

/// Serves requests forever on the calling thread. The handler is told the
/// process id of whoever is asking.
/// Who is on the other end of a connection.
pub struct Client {
    pub pid: u32,
    pipe: HANDLE,
}

impl Client {
    /// Runs `f` with the client's own identity, so file access is checked
    /// against what the client may do rather than what the daemon may.
    pub fn as_client<T>(&self, f: impl FnOnce() -> T) -> io::Result<T> {
        if unsafe { ImpersonateNamedPipeClient(self.pipe) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = f();
        unsafe { RevertToSelf() };
        Ok(result)
    }
}

pub fn serve(
    pipe_name: &str,
    mut handler: impl FnMut(Call, &Client) -> Response,
) -> io::Result<()> {
    let name = wide(pipe_name);
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(PIPE_SDDL).as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };

    loop {
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let connected = unsafe { ConnectNamedPipe(handle, null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        // The File owns the handle from here on and closes it when dropped.
        let pipe = unsafe { File::from_raw_handle(handle) };
        if !connected {
            continue;
        }
        let mut pid = 0;
        unsafe { GetNamedPipeClientProcessId(handle, &mut pid) };
        let client = Client { pid, pipe: handle };
        if let Err(e) = answer(&pipe, &client, &mut handler) {
            crate::log!("ipc: {e}");
        }
        unsafe {
            FlushFileBuffers(handle);
            DisconnectNamedPipe(handle);
        }
    }
}

fn answer(
    pipe: &File,
    client: &Client,
    handler: &mut impl FnMut(Call, &Client) -> Response,
) -> io::Result<()> {
    let mut line = String::new();
    BufReader::new(pipe).read_line(&mut line)?;
    let response = match serde_json::from_str::<Call>(&line) {
        Ok(call) => handler(call, client),
        Err(e) => Response {
            ok: false,
            message: format!("malformed request: {e}"),
            view: None,
        },
    };
    let mut pipe = pipe;
    writeln!(pipe, "{}", serde_json::to_string(&response)?)
}

/// Sends a request to the daemon. `Ok(None)` means no daemon is listening.
pub fn request(pipe_name: &str, request: Request) -> io::Result<Option<Response>> {
    let mut attempts = 0;
    let pipe = loop {
        match OpenOptions::new().read(true).write(true).open(pipe_name) {
            Ok(pipe) => break pipe,
            // Between two connections there is an instant with no pipe instance;
            // a busy pipe means the daemon is answering someone else.
            Err(e)
                if attempts < 20
                    && (e.kind() == io::ErrorKind::NotFound
                        || e.raw_os_error() == Some(ERROR_PIPE_BUSY)) =>
            {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
    };
    let call = Call {
        lang: Lang::system().code().to_string(),
        request,
    };
    writeln!(&pipe, "{}", serde_json::to_string(&call)?)?;
    let mut line = String::new();
    BufReader::new(&pipe).read_line(&mut line)?;
    serde_json::from_str(&line)
        .map(Some)
        .map_err(io::Error::other)
}
