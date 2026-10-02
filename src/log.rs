use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

const MAX_BYTES: u64 = 1024 * 1024;

/// Opens the log file for the lifetime of the process. The daemon has no
/// console, so this is its only output.
pub fn init(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = LOG.set(Mutex::new(file));
    }
}

pub fn write(msg: &str) {
    if let Some(log) = LOG.get() {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(log.lock().unwrap(), "{now} [{}] {msg}", std::process::id());
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}
