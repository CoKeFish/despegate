//! The part of despegate that lives in the user's session: it draws what the
//! daemon tells it to and reports what the user is doing.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::i18n::Lang;
use crate::overlay::{self, LockView, Mode, UiState};
use crate::paths::Paths;
use crate::service::{AgentReport, Request, Response, View};
use crate::{ipc, log, session};

const SYNC: Duration = Duration::from_millis(500);
/// Missed syncs after which the daemon is taken to be gone for good.
const MAX_FAILURES: u32 = 6;

pub fn run(paths: Paths) {
    let log_path = paths.agent_log();
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    log::init(&log_path);
    std::panic::set_hook(Box::new(|info| {
        log!("panic: {info}");
        std::process::exit(1);
    }));

    let ui = Arc::new(Mutex::new(UiState::default()));
    {
        let ui = ui.clone();
        let pipe = paths.pipe();
        thread::spawn(move || sync(&pipe, &ui));
    }
    overlay::run(ui);
}

/// Keeps reporting to the daemon and showing what it answers. Exits the
/// process when the daemon is gone or has started another agent.
fn sync(pipe: &str, ui: &Mutex<UiState>) {
    let mut failures = 0;
    loop {
        let report = AgentReport {
            idle_secs: session::idle_seconds(),
            foreground: session::foreground_app(),
            typed: ui.lock().unwrap().typed.clone(),
        };
        match ipc::request(pipe, Request::AgentSync(report)) {
            Ok(Some(Response {
                ok: true,
                view: Some(view),
                ..
            })) => {
                failures = 0;
                show(&mut ui.lock().unwrap(), view);
            }
            Ok(Some(response)) => {
                log!("the daemon turned this agent away: {}", response.message);
                std::process::exit(0);
            }
            Ok(None) | Err(_) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    log!("the daemon is not answering, exiting");
                    std::process::exit(0);
                }
            }
        }
        thread::sleep(SYNC);
    }
}

fn challenge(mode: &Mode) -> Option<&str> {
    match mode {
        Mode::Lock(LockView { challenge, .. }) => challenge.as_deref(),
        _ => None,
    }
}

fn show(ui: &mut UiState, view: View) {
    // What was typed belongs to one challenge; a new one starts from nothing.
    if challenge(&ui.mode).is_none() || challenge(&ui.mode) != challenge(&view.mode) {
        ui.typed.clear();
    }
    ui.lang = Lang::from_code(&view.lang).unwrap_or_default();
    ui.mode = view.mode;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(challenge: Option<&str>) -> Mode {
        Mode::Lock(LockView {
            until: Default::default(),
            reasons: String::new(),
            challenge: challenge.map(str::to_string),
            emergency_minutes: 5,
        })
    }

    fn view(mode: Mode) -> View {
        View {
            lang: "es".into(),
            mode,
        }
    }

    #[test]
    fn typed_text_survives_only_while_the_challenge_stays_the_same() {
        let mut ui = UiState {
            mode: lock(Some("abc")),
            typed: "ab".into(),
            ..UiState::default()
        };
        show(&mut ui, view(lock(Some("abc"))));
        assert_eq!(ui.typed, "ab");
        assert_eq!(ui.lang.code(), "es");

        show(&mut ui, view(lock(Some("xyz"))));
        assert_eq!(ui.typed, "");

        ui.typed = "x".into();
        show(&mut ui, view(Mode::Idle));
        assert_eq!((ui.typed.as_str(), &ui.mode), ("", &Mode::Idle));
    }
}
