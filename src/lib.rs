//! despegate: an intrusive blocker that forces you to step away from the computer.
//!
//! Two binaries share this library: `despegate` (the CLI) and `despegated`
//! (the daemon, which runs as a service, and the agent it starts in the
//! user's session).

pub mod agent;
pub mod config;
pub mod enforce;
pub mod engine;
pub mod i18n;
pub mod install;
pub mod ipc;
pub mod log;
pub mod media;
pub mod overlay;
pub mod paths;
pub mod service;
pub mod session;
pub mod store;
pub mod ui;
pub mod usage;
pub mod web;
pub mod winsvc;

/// Null-terminated UTF-16 copy of `s`, for Win32 `PCWSTR` parameters.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
