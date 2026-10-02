//! The background half of despegate. Built for the Windows subsystem so it
//! never opens a console window; everything it has to say goes to the log.
#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use despegate::engine::{self, Host};
use despegate::paths::Paths;
use despegate::{agent, winsvc};

fn main() {
    // Usage: despegated service              (started by the service manager)
    //        despegated daemon [--home DIR]  (the same daemon, run by hand)
    //        despegated agent [--home DIR]   (started by the daemon)
    let mut args = std::env::args().skip(1);
    let mut mode = None;
    let mut home = None;
    while let Some(arg) = args.next() {
        if arg == "--home" {
            home = args.next().map(PathBuf::from);
        } else {
            mode = Some(arg);
        }
    }
    match mode.as_deref() {
        Some("service") => {
            let _ = winsvc::run();
        }
        Some("daemon") => {
            let _ = engine::run(Paths::new(home), Host::Standalone, &AtomicBool::new(false));
        }
        Some("agent") => agent::run(Paths::new(home)),
        _ => {}
    }
}
