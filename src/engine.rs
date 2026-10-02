//! The daemon: decides what is blocked at every moment, closes programs,
//! answers the CLI, and tells the agent what to show.

use std::collections::HashSet;
use std::hash::{BuildHasher, RandomState};
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDateTime};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;

use crate::config::{ActiveBlock, Config};
use crate::i18n::Lang;
use crate::overlay::{LockView, Mode, countdown};
use crate::paths::Paths;
use crate::service::{self, AgentReport, Call, Request, Response, Runtime, View};
use crate::session::{self, Agent};
use crate::store::Store;
use crate::usage::{Sensors, State};
use crate::{enforce, ipc, log, tr, wide};

const TICK: Duration = Duration::from_secs(1);
const FLASH: Duration = Duration::from_secs(6);
/// An agent report older than this says nothing about the present.
const REPORT_FRESH: Duration = Duration::from_secs(3);
const SAVE_EVERY_TICKS: u32 = 15;
/// Closed during a screen lock so it cannot be used to end the agent.
const TASK_MANAGER: &str = "taskmgr.exe";
/// This many agent deaths within a minute during a lock is taken as someone
/// killing it on purpose; the session is then sent to the sign-in screen.
const SUSPICIOUS_DEATHS: usize = 5;
const DEATH_WINDOW: Duration = Duration::from_secs(60);

/// How the daemon was started.
#[derive(Clone, Copy, PartialEq)]
pub enum Host {
    /// As a Windows service, away from the user's session.
    Service,
    /// By hand, inside the user's own session (development).
    Standalone,
}

/// What the tick and the pipe server both work on.
struct Shared {
    config: Store<Config>,
    state: Store<State>,
    paused_until: Option<NaiveDateTime>,
    /// Text to type on the lock screen for an emergency pause.
    challenge: Option<String>,
    /// What the agent must show right now.
    view: View,
    report: Option<(AgentReport, Instant)>,
    /// Only this process may speak as the agent.
    agent_pid: Option<u32>,
    /// The language of the user's session, as last told by the agent.
    hint: Option<String>,
}

struct Daemon {
    paths: Paths,
    host: Host,
    shared: Arc<Mutex<Shared>>,
    agent: Option<Agent>,
    agent_error: String,
    deaths: Vec<Instant>,
    last_disconnect: Option<Instant>,
    /// Short-lived banner, e.g. after closing an app.
    flash: Option<(String, Instant)>,
    ticks: u32,
}

/// Runs the daemon until the stop marker appears or `stop` is raised.
pub fn run(paths: Paths, host: Host, stop: &AtomicBool) -> Result<(), String> {
    // The mutex handle is deliberately leaked: it marks this process as the
    // daemon until it exits.
    let mutex = unsafe { CreateMutexW(null(), 0, wide(&paths.mutex()).as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(());
    }
    if paths.stop_marker().exists() {
        return Ok(());
    }
    let _ = std::fs::create_dir_all(&paths.home);
    log::init(&paths.log());
    std::panic::set_hook(Box::new(|info| {
        log!("panic: {info}");
        std::process::exit(1);
    }));

    let config: Store<Config> = open_store(&paths.config())
        .map_err(|e| fail(format!("cannot open {}: {e}", paths.config().display())))?;
    let state: Store<State> = open_store(&paths.state())
        .map_err(|e| fail(format!("cannot open {}: {e}", paths.state().display())))?;
    log!("daemon started with {} rule(s)", config.data.rules.len());

    let shared = Arc::new(Mutex::new(Shared {
        config,
        state,
        paused_until: None,
        challenge: None,
        view: View::default(),
        report: None,
        agent_pid: None,
        hint: None,
    }));
    {
        let shared = shared.clone();
        let pipe = paths.pipe();
        thread::spawn(move || {
            let result = ipc::serve(&pipe, |call, client| answer(&shared, call, client));
            // Without the pipe neither the CLI nor the agent can reach us;
            // exiting lets the service manager start a healthy daemon.
            log!("ipc server stopped: {result:?}");
            std::process::exit(1);
        });
    }

    let mut daemon = Daemon {
        paths,
        host,
        shared,
        agent: None,
        agent_error: String::new(),
        deaths: Vec::new(),
        last_disconnect: None,
        flash: None,
        ticks: 0,
    };
    while !stop.load(Ordering::SeqCst) && daemon.tick() {
        thread::sleep(TICK);
    }
    daemon.shutdown();
    Ok(())
}

fn fail(message: String) -> String {
    log!("{message}");
    message
}

/// A daemon that was just killed may not have released the file yet.
fn open_store<T>(path: &std::path::Path) -> std::io::Result<Store<T>>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    let mut attempts = 0;
    loop {
        match Store::open(path) {
            Ok(store) => return Ok(store),
            Err(_) if attempts < 20 => {
                attempts += 1;
                thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(e),
        }
    }
}

fn answer(shared: &Mutex<Shared>, call: Call, client: u32) -> Response {
    let now = Local::now().naive_local();
    let mut shared = shared.lock().unwrap();
    let shared = &mut *shared;

    if let Request::AgentSync(report) = call.request {
        if shared.agent_pid != Some(client) {
            return Response {
                ok: false,
                message: "not the current agent".into(),
                view: None,
            };
        }
        if shared
            .challenge
            .as_deref()
            .is_some_and(|challenge| challenge == report.typed)
        {
            let minutes = shared.config.data.emergency_minutes;
            shared.paused_until = Some(now + chrono::Duration::minutes(minutes as i64));
            shared.challenge = None;
            // Take the lock screen down at once rather than at the next tick.
            shared.view.mode = Mode::Idle;
            log!("emergency pause for {minutes} min");
        }
        shared.hint = Some(call.lang);
        shared.report = Some((report, Instant::now()));
        return Response {
            ok: true,
            message: String::new(),
            view: Some(shared.view.clone()),
        };
    }

    let hint = Lang::from_code(&call.lang).unwrap_or_default();
    let runtime = Runtime {
        daemon: true,
        paused_until: shared.paused_until,
        state: &shared.state.data,
    };
    let outcome = service::handle(
        call.request.clone(),
        &mut shared.config.data,
        now,
        &runtime,
        hint,
    );
    if outcome.changed {
        log!("accepted {:?}", call.request);
        if let Err(e) = shared.config.save() {
            let lang = Lang::resolve(shared.config.data.language.as_deref(), Some(hint.code()));
            let message = tr!(lang, "error.save", error = e);
            return Response {
                ok: false,
                message,
                view: None,
            };
        }
    }
    outcome.response
}

impl Daemon {
    /// One round of enforcement. Returns false when the daemon must exit.
    fn tick(&mut self) -> bool {
        if self.paths.stop_marker().exists() {
            return false;
        }
        let was_locked = matches!(self.shared.lock().unwrap().view.mode, Mode::Lock(_));
        self.keep_agent_alive(was_locked);
        let session = self.session();

        let now = Local::now().naive_local();
        let shared = self.shared.clone();
        let mut shared = shared.lock().unwrap();
        let shared = &mut *shared;

        if service::prune(&mut shared.config.data, now)
            && let Err(e) = shared.config.save()
        {
            log!("could not save the config: {e}");
        }
        if shared.paused_until.is_some_and(|until| until <= now) {
            shared.paused_until = None;
            log!("emergency pause is over");
        }
        let config = shared.config.data.clone();
        let lang = Lang::resolve(config.language.as_deref(), shared.hint.as_deref());

        let report = shared
            .report
            .as_ref()
            .filter(|(_, at)| at.elapsed() < REPORT_FRESH)
            .map(|(report, _)| report.clone());
        let limited: Vec<String> = config.allowances.iter().map(|a| a.app.clone()).collect();
        let sensors = Sensors {
            idle_secs: report.as_ref().map(|r| r.idle_secs),
            // With nobody reporting which window is in front, a limited
            // program counts as in use for as long as it is running.
            in_use: match &report {
                Some(report) => report
                    .foreground
                    .iter()
                    .filter(|app| limited.contains(app))
                    .cloned()
                    .collect(),
                None => enforce::running(&limited, session),
            },
            locked: was_locked,
        };
        let break_started = shared.state.data.advance(&config, now, &sensors);
        if break_started {
            log!("break started");
        }
        self.ticks += 1;
        if (break_started || self.ticks.is_multiple_of(SAVE_EVERY_TICKS))
            && let Err(e) = shared.state.save()
        {
            log!("could not save the state: {e}");
        }
        let state = &shared.state.data;

        let mut blocks = config.active_blocks(now);
        if let Some(until) = state.on_break(now) {
            blocks.push(ActiveBlock {
                name: "break".into(),
                until,
                lock: true,
                apps: vec![],
            });
        }
        let exhausted = state.exhausted(&config, now);
        let lock_until = blocks.iter().filter(|b| b.lock).map(|b| b.until).max();
        let enforcing = !blocks.is_empty() || !exhausted.is_empty();

        let mode = match shared.paused_until {
            Some(until) if enforcing => Mode::Banner {
                text: tr!(lang, "banner.paused", left = countdown(until - now)),
            },
            Some(_) => self.banner(&config, state, &sensors, now, lang),
            None => {
                let mut apps: HashSet<String> =
                    blocks.iter().flat_map(|b| b.apps.iter().cloned()).collect();
                apps.extend(exhausted.iter().cloned());
                if lock_until.is_some() {
                    apps.insert(TASK_MANAGER.into());
                }
                for app in enforce::close_apps(&apps, session) {
                    if app == TASK_MANAGER {
                        continue;
                    }
                    log!("closed {app}");
                    let until = blocks
                        .iter()
                        .filter(|b| b.apps.contains(&app))
                        .map(|b| b.until)
                        .max();
                    let text = match until {
                        Some(until) => tr!(
                            lang,
                            "banner.blocked",
                            app = app,
                            until = until.format("%H:%M")
                        ),
                        None => tr!(lang, "banner.allowance_spent", app = app),
                    };
                    self.flash = Some((text, Instant::now() + FLASH));
                }
                match lock_until {
                    Some(until) => {
                        if shared.challenge.is_none() && config.emergency_chars > 0 {
                            shared.challenge = Some(challenge(config.emergency_chars as usize));
                        }
                        Mode::Lock(LockView {
                            until,
                            reasons: config.reasons.clone(),
                            challenge: shared.challenge.clone(),
                            emergency_minutes: config.emergency_minutes,
                        })
                    }
                    None => self.banner(&config, state, &sensors, now, lang),
                }
            }
        };
        if lock_until.is_none() {
            shared.challenge = None;
        }
        shared.view = View {
            lang: lang.code().to_string(),
            mode,
        };
        true
    }

    /// What to show when the screen is not locked: a recent notice, a warning
    /// about whatever is about to happen soonest, or nothing.
    fn banner(
        &mut self,
        config: &Config,
        state: &State,
        sensors: &Sensors,
        now: NaiveDateTime,
        lang: Lang,
    ) -> Mode {
        if let Some((text, expires)) = &self.flash {
            if Instant::now() < *expires {
                return Mode::Banner { text: text.clone() };
            }
            self.flash = None;
        }
        let warn = chrono::Duration::minutes(config.warn_minutes as i64);
        let mut warnings: Vec<(chrono::Duration, String)> = Vec::new();
        if let Some((rule, start)) = config.next_start(now) {
            let left = start - now;
            warnings.push((
                left,
                tr!(
                    lang,
                    "banner.rule_soon",
                    name = rule.name,
                    left = countdown(left)
                ),
            ));
        }
        if state.on_break(now).is_none()
            && let Some(left) = state.break_due_in(config)
        {
            warnings.push((left, tr!(lang, "banner.break_soon", left = countdown(left))));
        }
        for app in &sensors.in_use {
            if let Some(left) = state.allowance_left(config, app, now) {
                warnings.push((
                    left,
                    tr!(
                        lang,
                        "banner.allowance_soon",
                        app = app,
                        left = countdown(left)
                    ),
                ));
            }
        }
        warnings
            .into_iter()
            .filter(|(left, _)| *left <= warn)
            .min_by_key(|(left, _)| *left)
            .map_or(Mode::Idle, |(_, text)| Mode::Banner { text })
    }

    /// The session whose programs are being policed.
    fn session(&self) -> u32 {
        match (&self.agent, self.host) {
            (Some(agent), _) => agent.session,
            (None, Host::Service) => session::console_session().unwrap_or(0),
            (None, Host::Standalone) => session::current_session(),
        }
    }

    fn keep_agent_alive(&mut self, locked: bool) {
        // The user at the screen may have changed (fast user switching).
        if self.host == Host::Service
            && let Some(agent) = &self.agent
            && session::console_session() != Some(agent.session)
        {
            agent.kill();
        }
        if self.agent.as_ref().is_some_and(Agent::alive) {
            return;
        }
        if let Some(agent) = self.agent.take() {
            log!("agent {} is gone", agent.pid);
            self.shared.lock().unwrap().agent_pid = None;
            if locked {
                self.suspect(agent.session);
            }
        }
        let spawned = match self.host {
            Host::Service => session::spawn_agent_in_console_session(),
            Host::Standalone => session::spawn_agent_here(&self.paths),
        };
        match spawned {
            Ok(agent) => {
                log!("agent {} started in session {}", agent.pid, agent.session);
                self.shared.lock().unwrap().agent_pid = Some(agent.pid);
                self.agent = Some(agent);
                self.agent_error.clear();
            }
            // Expected while nobody is logged on; logged once, not every second.
            Err(e) => {
                let error = e.to_string();
                if error != self.agent_error {
                    log!("no agent: {error}");
                    self.agent_error = error;
                }
            }
        }
    }

    /// An agent died while the screen was locked. Once is a crash; again and
    /// again is someone clearing the lock screen away.
    fn suspect(&mut self, session: u32) {
        let now = Instant::now();
        self.deaths
            .retain(|at| now.duration_since(*at) < DEATH_WINDOW);
        self.deaths.push(now);
        let recently = self
            .last_disconnect
            .is_some_and(|at| now.duration_since(at) < DEATH_WINDOW);
        if self.host == Host::Service && self.deaths.len() >= SUSPICIOUS_DEATHS && !recently {
            log!(
                "the agent keeps dying during a lock; sending session {session} to the sign-in screen"
            );
            session::disconnect(session);
            self.last_disconnect = Some(now);
        }
    }

    fn shutdown(&mut self) {
        log!("daemon exiting");
        if let Err(e) = self.shared.lock().unwrap().state.save() {
            log!("could not save the state: {e}");
        }
        if let Some(agent) = self.agent.take() {
            agent.kill();
        }
    }
}

/// Random text in groups of five, without look-alike characters.
fn challenge(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let random = RandomState::new();
    let text: String = (0..len)
        .map(|i| {
            if i % 6 == 5 {
                ' '
            } else {
                ALPHABET[(random.hash_one(i) % ALPHABET.len() as u64) as usize] as char
            }
        })
        .collect();
    text.trim_end().to_string()
}
