//! Running the daemon as a Windows service.
//!
//! The service does not accept a stop request: the sanctioned ways to end it
//! are the stop marker written by the installer and system shutdown. If it is
//! killed, the service manager starts it again.

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::engine::{self, Host};
use crate::paths::Paths;

pub const SERVICE_NAME: &str = "despegate";

define_windows_service!(ffi_service_main, service_main);

/// Hands the calling thread to the service manager. Fails when the process
/// was not started by it.
pub fn run() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_arguments: Vec<OsString>) {
    let stop = Arc::new(AtomicBool::new(false));
    let handler = {
        let stop = stop.clone();
        move |control| match control {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Shutdown => {
                stop.store(true, Ordering::SeqCst);
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    let Ok(status) = service_control_handler::register(SERVICE_NAME, handler) else {
        return;
    };
    let report = |state, accepted, code| {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accepted,
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        });
    };

    report(ServiceState::Running, ServiceControlAccept::SHUTDOWN, 0);
    let result = engine::run(Paths::new(None), Host::Service, &stop);
    // A non-zero exit code makes the service manager start the service again.
    report(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        result.is_err() as u32,
    );
}
