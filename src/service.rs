//! The requests the CLI can make and the rules for accepting them.
//!
//! Anything that makes despegate stricter is always accepted. Anything that
//! loosens it is refused while a block is active or about to start, so the
//! only way out in the heat of the moment is `despegate uninstall`.

use chrono::{Datelike, Duration, NaiveDateTime, Weekday};
use serde::{Deserialize, Serialize};

use crate::config::{Allowance, AppError, BreakPolicy, Config, OneOff, Rule, WEEK, normalize_app};
use crate::i18n::Lang;
use crate::media::ImportError;
use crate::overlay::{Mode, countdown};
use crate::tr;
use crate::usage::State;

/// A request together with the language its sender's Windows is displayed
/// in, which is used when no language has been configured.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Call {
    pub lang: String,
    pub request: Request,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    RuleAdd {
        rule: Rule,
    },
    RuleRemove {
        name: String,
    },
    Now {
        minutes: u32,
        lock: bool,
        apps: Vec<String>,
    },
    ReasonsSet {
        text: String,
    },
    /// A photo or video to copy in; the file is read as the caller.
    MediaImport {
        source: String,
    },
    /// Registers a file already in the media directory (the daemon's own step).
    MediaAdd {
        name: String,
    },
    MediaRemove {
        name: String,
    },
    Set {
        key: Setting,
        value: u32,
    },
    BreakSet {
        work_minutes: u32,
        break_minutes: u32,
    },
    BreakOff,
    AllowanceSet {
        app: String,
        minutes: u32,
    },
    AllowanceRemove {
        app: String,
    },
    LanguageSet {
        code: Option<String>,
    },
    AppearanceSet {
        mode: Appearance,
    },
    /// Sent by the agent in the user's session, never by the CLI.
    AgentSync(AgentReport),
}

/// What the agent observes in the user's session.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AgentReport {
    /// Seconds since the last keyboard or mouse input.
    pub idle_secs: u64,
    /// Executable name of the program in the foreground.
    pub foreground: Option<String>,
    /// What has been typed on the lock screen towards the emergency challenge.
    pub typed: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Follow the Windows setting
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn code(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Setting {
    /// Minutes before a block during which rules can no longer be loosened
    LeadMinutes,
    /// Minutes before a block when the warning banner appears
    WarnMinutes,
    /// Characters to type on the lock screen for an emergency pause (0 disables it)
    EmergencyChars,
    /// Length of the emergency pause in minutes
    EmergencyMinutes,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    /// What the agent must show; only present in answers to [`Request::AgentSync`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<View>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct View {
    pub lang: String,
    /// "light", "dark" or "system".
    #[serde(default)]
    pub theme: String,
    pub mode: Mode,
}

/// What the daemon knows beyond the config.
pub struct Runtime<'a> {
    pub daemon: bool,
    pub paused_until: Option<NaiveDateTime>,
    pub state: &'a State,
}

pub struct Outcome {
    pub response: Response,
    pub changed: bool,
}

const MAX_NOW_MINUTES: u32 = 24 * 60;

/// Carries out `request`. `hint` is the language of whoever is asking; it
/// is used unless a language has been configured.
pub fn handle(
    request: Request,
    cfg: &mut Config,
    now: NaiveDateTime,
    rt: &Runtime,
    hint: Lang,
) -> Outcome {
    let lang = Lang::resolve(cfg.language.as_deref(), Some(hint.code()));
    let result = match request {
        Request::Status => {
            return Outcome {
                response: ok(status(cfg, now, rt, lang)),
                changed: false,
            };
        }
        Request::RuleAdd { rule } => rule_add(cfg, rule, now, lang),
        Request::RuleRemove { name } => rule_remove(cfg, &name, now, lang),
        Request::Now {
            minutes,
            lock,
            apps,
        } => start_now(cfg, minutes, lock, apps, now, lang),
        Request::ReasonsSet { text } => {
            cfg.reasons = text.trim().to_string();
            Ok(tr!(lang, "reasons.saved"))
        }
        Request::Set { key, value } => set(cfg, key, value, now, rt, lang),
        Request::BreakSet {
            work_minutes,
            break_minutes,
        } => break_set(
            cfg,
            Some(BreakPolicy {
                work_minutes,
                break_minutes,
            }),
            now,
            rt,
            lang,
        ),
        Request::BreakOff => break_set(cfg, None, now, rt, lang),
        Request::AllowanceSet { app, minutes } => {
            allowance_set(cfg, &app, Some(minutes), now, rt, lang)
        }
        Request::AllowanceRemove { app } => allowance_set(cfg, &app, None, now, rt, lang),
        Request::LanguageSet { code } => language_set(cfg, code, lang, hint),
        Request::AppearanceSet { mode } => {
            cfg.appearance = mode.code().to_string();
            let name = lang.format(&format!("appearance.{}", mode.code()), &[]);
            Ok(tr!(lang, "appearance.set", mode = name))
        }
        Request::MediaImport { .. } => Err(tr!(lang, "error.not_agent")),
        Request::MediaAdd { name } => {
            if !cfg.media.contains(&name) {
                cfg.media.push(name.clone());
            }
            Ok(tr!(lang, "media.added", name = name))
        }
        Request::MediaRemove { name } => {
            if !cfg.media.contains(&name) {
                Err(tr!(lang, "error.no_media", name = name))
            } else {
                cfg.media.retain(|m| *m != name);
                Ok(tr!(lang, "media.removed", name = name))
            }
        }
        Request::AgentSync(_) => Err(tr!(lang, "error.not_agent")),
    };
    match result {
        Ok(message) => Outcome {
            response: ok(message),
            changed: true,
        },
        Err(message) => Outcome {
            response: Response {
                ok: false,
                message,
                view: None,
            },
            changed: false,
        },
    }
}

/// Why a photo or video could not be taken in.
pub fn import_error(error: ImportError, lang: Lang) -> String {
    match error {
        ImportError::Unsupported => tr!(lang, "error.media_unsupported"),
        ImportError::TooLarge => tr!(
            lang,
            "error.media_too_large",
            max = crate::media::MAX_BYTES / (1024 * 1024)
        ),
        ImportError::Unreadable(e) => tr!(lang, "error.media_unreadable", error = e),
        ImportError::Unwritable(e) => tr!(lang, "error.media_unwritable", error = e),
    }
}

/// Drops one-off blocks that have ended. Returns whether anything was removed.
pub fn prune(cfg: &mut Config, now: NaiveDateTime) -> bool {
    let before = cfg.oneoffs.len();
    cfg.oneoffs.retain(|o| o.until > now);
    cfg.oneoffs.len() != before
}

/// Why loosening anything is currently forbidden, if it is: a block or break
/// is active, or one is about to start.
pub fn locked_in(cfg: &Config, state: &State, now: NaiveDateTime, lang: Lang) -> Option<String> {
    if let Some(block) = cfg.active_blocks(now).first() {
        return Some(tr!(
            lang,
            "why.block_active",
            name = block.name,
            until = when(lang, block.until)
        ));
    }
    if let Some(until) = state.on_break(now) {
        return Some(tr!(lang, "why.break_active", until = time(until)));
    }
    if let Some((rule, start)) = cfg.next_start(now)
        && start - now <= Duration::minutes(cfg.lead_minutes as i64)
    {
        return Some(tr!(
            lang,
            "why.block_imminent",
            name = rule.name,
            start = time(start),
            lead = cfg.lead_minutes
        ));
    }
    // With short work periods the full lead time would never leave a window
    // in which the break policy can be changed.
    let policy = cfg.breaks?;
    let lead = cfg.lead_minutes.min(policy.work_minutes / 2);
    let due = state.break_due_in(cfg)?;
    (due <= Duration::minutes(lead as i64))
        .then(|| tr!(lang, "why.break_imminent", left = countdown(due)))
}

pub fn describe_effect(lock: bool, apps: &[String], lang: Lang) -> String {
    match (lock, apps.is_empty()) {
        (true, true) => tr!(lang, "effect.lock"),
        (true, false) => tr!(lang, "effect.lock_and_close", apps = apps.join(", ")),
        (false, _) => tr!(lang, "effect.close", apps = apps.join(", ")),
    }
}

pub fn describe_rule(rule: &Rule, lang: Lang) -> String {
    tr!(
        lang,
        "rule.describe",
        name = rule.name,
        days = describe_days(&rule.days, lang),
        start = rule.start.format("%H:%M"),
        end = rule.end.format("%H:%M"),
        effect = describe_effect(rule.lock, &rule.apps, lang)
    )
}

pub fn day_name(day: Weekday, lang: Lang) -> String {
    lang.format(&format!("day.{}", day.to_string().to_lowercase()), &[])
}

fn describe_days(days: &[Weekday], lang: Lang) -> String {
    let present: Vec<usize> = (0..7).filter(|i| days.contains(&WEEK[*i])).collect();
    match present.as_slice() {
        [] => tr!(lang, "days.never"),
        [_, _, _, _, _, _, _] => tr!(lang, "days.every"),
        // A run of three or more consecutive days reads better as a range.
        [first, .., last] if present.len() >= 3 && last - first + 1 == present.len() => {
            format!(
                "{}-{}",
                day_name(WEEK[*first], lang),
                day_name(WEEK[*last], lang)
            )
        }
        _ => present
            .iter()
            .map(|i| day_name(WEEK[*i], lang))
            .collect::<Vec<_>>()
            .join(","),
    }
}

fn time(at: NaiveDateTime) -> String {
    at.format("%H:%M").to_string()
}

/// Day and time, e.g. "Mon 07:00".
pub fn when(lang: Lang, at: NaiveDateTime) -> String {
    format!("{} {}", day_name(at.weekday(), lang), time(at))
}

fn ok(message: String) -> Response {
    Response {
        ok: true,
        message,
        view: None,
    }
}

fn app_name(app: &str, lang: Lang) -> Result<String, String> {
    normalize_app(app).map_err(|e| match e {
        AppError::Empty => tr!(lang, "error.app_empty"),
        AppError::Protected(app) => tr!(lang, "error.app_protected", app = app),
    })
}

fn normalize_apps(apps: Vec<String>, lang: Lang) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for app in apps {
        let app = app_name(&app, lang)?;
        if !out.contains(&app) {
            out.push(app);
        }
    }
    Ok(out)
}

fn rule_add(
    cfg: &mut Config,
    mut rule: Rule,
    now: NaiveDateTime,
    lang: Lang,
) -> Result<String, String> {
    rule.name = rule.name.trim().to_string();
    if rule.name.is_empty() {
        return Err(tr!(lang, "error.name_required"));
    }
    if cfg.rules.iter().any(|r| r.name == rule.name) {
        return Err(tr!(lang, "error.name_taken", name = rule.name));
    }
    if rule.days.is_empty() {
        return Err(tr!(lang, "error.days_required"));
    }
    if rule.start == rule.end {
        return Err(tr!(lang, "error.same_time"));
    }
    rule.apps = normalize_apps(rule.apps, lang)?;
    if !rule.lock && rule.apps.is_empty() {
        return Err(tr!(lang, "error.no_effect"));
    }
    let mut message = tr!(lang, "rule.added", rule = describe_rule(&rule, lang));
    if let Some(until) = rule.active_until(now) {
        message += "\n";
        message += &tr!(lang, "rule.active_now", until = when(lang, until));
    }
    cfg.rules.push(rule);
    Ok(message)
}

fn rule_remove(
    cfg: &mut Config,
    name: &str,
    now: NaiveDateTime,
    lang: Lang,
) -> Result<String, String> {
    let index = cfg
        .rules
        .iter()
        .position(|r| r.name == name)
        .ok_or_else(|| tr!(lang, "error.no_rule", name = name))?;
    let rule = &cfg.rules[index];
    if let Some(until) = rule.active_until(now) {
        return Err(tr!(
            lang,
            "error.rule_active",
            name = name,
            until = when(lang, until)
        ));
    }
    if let Some(start) = rule.next_start(now)
        && start - now <= Duration::minutes(cfg.lead_minutes as i64)
    {
        return Err(tr!(
            lang,
            "error.rule_imminent",
            name = name,
            start = time(start),
            lead = cfg.lead_minutes
        ));
    }
    cfg.rules.remove(index);
    Ok(tr!(lang, "rule.removed", name = name))
}

fn start_now(
    cfg: &mut Config,
    minutes: u32,
    lock: bool,
    apps: Vec<String>,
    now: NaiveDateTime,
    lang: Lang,
) -> Result<String, String> {
    if minutes == 0 || minutes > MAX_NOW_MINUTES {
        return Err(tr!(
            lang,
            "error.duration_range",
            hours = MAX_NOW_MINUTES / 60
        ));
    }
    let apps = normalize_apps(apps, lang)?;
    if !lock && apps.is_empty() {
        return Err(tr!(lang, "error.nothing_to_block"));
    }
    let until = now + Duration::minutes(minutes as i64);
    let base = format!("now-{}", until.format("%H:%M"));
    let mut name = base.clone();
    let mut n = 2;
    while cfg.oneoffs.iter().any(|o| o.name == name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    let message = tr!(
        lang,
        "now.started",
        effect = describe_effect(lock, &apps, lang),
        until = when(lang, until)
    );
    cfg.oneoffs.push(OneOff {
        name,
        until,
        lock,
        apps,
    });
    Ok(message)
}

impl Setting {
    fn slot(self, cfg: &mut Config) -> &mut u32 {
        match self {
            Setting::LeadMinutes => &mut cfg.lead_minutes,
            Setting::WarnMinutes => &mut cfg.warn_minutes,
            Setting::EmergencyChars => &mut cfg.emergency_chars,
            Setting::EmergencyMinutes => &mut cfg.emergency_minutes,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Setting::LeadMinutes => "lead-minutes",
            Setting::WarnMinutes => "warn-minutes",
            Setting::EmergencyChars => "emergency-chars",
            Setting::EmergencyMinutes => "emergency-minutes",
        }
    }
}

/// Refuses a change that loosens despegate while it is locked in.
fn may_loosen(cfg: &Config, now: NaiveDateTime, rt: &Runtime, lang: Lang) -> Result<(), String> {
    match locked_in(cfg, rt.state, now, lang) {
        Some(why) => Err(tr!(lang, "error.loosens", why = why)),
        None => Ok(()),
    }
}

fn set(
    cfg: &mut Config,
    key: Setting,
    value: u32,
    now: NaiveDateTime,
    rt: &Runtime,
    lang: Lang,
) -> Result<String, String> {
    let old = *key.slot(cfg);
    let (max, weaker) = match key {
        Setting::LeadMinutes => (24 * 60, value < old),
        Setting::WarnMinutes => (60, false),
        // 0 disables the pause entirely, so it is the strictest value.
        Setting::EmergencyChars => (2000, value != 0 && (old == 0 || value < old)),
        Setting::EmergencyMinutes => (60, value > old),
    };
    if value > max {
        return Err(tr!(lang, "error.max", max = max));
    }
    if key == Setting::EmergencyMinutes && value == 0 {
        return Err(tr!(lang, "error.emergency_min"));
    }
    // Checked before applying, so lowering the lead time cannot unlock itself.
    if weaker {
        may_loosen(cfg, now, rt, lang)?;
    }
    *key.slot(cfg) = value;
    let mut message = tr!(lang, "set.done", key = key.name(), value = value, old = old);
    if key == Setting::EmergencyChars && value == 0 {
        message += "\n";
        message += &tr!(lang, "set.no_emergency_warning");
    }
    Ok(message)
}

fn break_set(
    cfg: &mut Config,
    policy: Option<BreakPolicy>,
    now: NaiveDateTime,
    rt: &Runtime,
    lang: Lang,
) -> Result<String, String> {
    if let Some(p) = policy {
        if !(5..=480).contains(&p.work_minutes) {
            return Err(tr!(lang, "error.break_work_range"));
        }
        if !(1..=120).contains(&p.break_minutes) {
            return Err(tr!(lang, "error.break_rest_range"));
        }
    }
    let weaker = match (cfg.breaks, policy) {
        (Some(old), Some(new)) => {
            new.work_minutes > old.work_minutes || new.break_minutes < old.break_minutes
        }
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => return Err(tr!(lang, "error.break_already_off")),
    };
    if weaker {
        may_loosen(cfg, now, rt, lang)?;
    }
    cfg.breaks = policy;
    Ok(match policy {
        Some(p) => tr!(
            lang,
            "break.set",
            rest = p.break_minutes,
            work = p.work_minutes
        ),
        None => tr!(lang, "break.off"),
    })
}

fn allowance_set(
    cfg: &mut Config,
    app: &str,
    minutes: Option<u32>,
    now: NaiveDateTime,
    rt: &Runtime,
    lang: Lang,
) -> Result<String, String> {
    let app = app_name(app, lang)?;
    let old = cfg.allowance(&app).map(|a| a.minutes);
    if let Some(minutes) = minutes
        && !(1..=24 * 60).contains(&minutes)
    {
        return Err(tr!(lang, "error.allowance_range"));
    }
    let weaker = match (old, minutes) {
        (Some(old), Some(new)) => new > old,
        (Some(_), None) => true,
        (None, None) => return Err(tr!(lang, "error.no_allowance", app = app)),
        (None, Some(_)) => false,
    };
    if weaker {
        may_loosen(cfg, now, rt, lang)?;
        // Otherwise the limit could be raised the moment it is reached.
        let used = rt.state.used_today(&app, now);
        if used > 0 {
            return Err(tr!(
                lang,
                "error.allowance_used",
                app = app,
                used = countdown(Duration::seconds(used as i64))
            ));
        }
    }
    cfg.allowances.retain(|a| a.app != app);
    Ok(match minutes {
        Some(minutes) => {
            cfg.allowances.push(Allowance {
                app: app.clone(),
                minutes,
            });
            tr!(lang, "allowance.set", app = app, minutes = minutes)
        }
        None => tr!(lang, "allowance.removed", app = app),
    })
}

/// `lang` is the language in use now; `hint` the caller's own, which is what
/// "follow Windows" goes back to.
fn language_set(
    cfg: &mut Config,
    code: Option<String>,
    lang: Lang,
    hint: Lang,
) -> Result<String, String> {
    let Some(code) = code else {
        cfg.language = None;
        return Ok(tr!(hint, "language.auto", name = hint.name()));
    };
    let Some(chosen) = Lang::from_code(&code) else {
        let list: Vec<&str> = Lang::all().map(Lang::code).collect();
        return Err(tr!(
            lang,
            "error.language",
            code = code,
            list = list.join(", ")
        ));
    };
    cfg.language = Some(chosen.code().to_string());
    Ok(tr!(chosen, "language.set", name = chosen.name()))
}

fn status(cfg: &Config, now: NaiveDateTime, rt: &Runtime, lang: Lang) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(if rt.daemon {
        tr!(lang, "status.daemon_running")
    } else {
        tr!(lang, "status.daemon_stopped")
    });
    if let Some(until) = rt.paused_until.filter(|until| *until > now) {
        lines.push(tr!(lang, "status.paused", until = until.format("%H:%M:%S")));
    }

    let active = cfg.active_blocks(now);
    let on_break = rt.state.on_break(now);
    if active.is_empty() && on_break.is_none() {
        lines.push(tr!(lang, "status.active_none"));
    } else {
        lines.push(tr!(lang, "status.active"));
        for block in &active {
            lines.push(tr!(
                lang,
                "status.active_item",
                name = block.name,
                effect = describe_effect(block.lock, &block.apps, lang),
                until = when(lang, block.until)
            ));
        }
        if let Some(until) = on_break {
            lines.push(tr!(lang, "status.break_active", until = time(until)));
        }
    }
    if let Some((rule, start)) = cfg.next_start(now) {
        lines.push(tr!(
            lang,
            "status.next",
            name = rule.name,
            when = when(lang, start)
        ));
    }

    if cfg.rules.is_empty() {
        lines.push(tr!(lang, "status.rules_none"));
    } else {
        lines.push(tr!(lang, "status.rules"));
        lines.extend(
            cfg.rules
                .iter()
                .map(|rule| format!("  {}", describe_rule(rule, lang))),
        );
    }

    match cfg.breaks {
        None => lines.push(tr!(lang, "status.breaks_off")),
        Some(policy) => {
            let left = rt.state.break_due_in(cfg).unwrap_or_default();
            lines.push(tr!(
                lang,
                "status.breaks",
                rest = policy.break_minutes,
                work = policy.work_minutes,
                left = countdown(left)
            ));
        }
    }

    if cfg.allowances.is_empty() {
        lines.push(tr!(lang, "status.allowances_none"));
    } else {
        lines.push(tr!(lang, "status.allowances"));
        for allowance in &cfg.allowances {
            let left = rt
                .state
                .allowance_left(cfg, &allowance.app, now)
                .unwrap_or_default();
            lines.push(tr!(
                lang,
                "status.allowance_item",
                app = allowance.app,
                minutes = allowance.minutes,
                left = countdown(left)
            ));
        }
    }

    let emergency = if cfg.emergency_chars == 0 {
        tr!(lang, "status.emergency_off")
    } else {
        tr!(
            lang,
            "status.emergency_on",
            minutes = cfg.emergency_minutes,
            chars = cfg.emergency_chars
        )
    };
    lines.push(tr!(
        lang,
        "status.settings",
        lead = cfg.lead_minutes,
        warn = cfg.warn_minutes,
        emergency = emergency
    ));
    lines.push(match &cfg.language {
        Some(_) => tr!(lang, "status.language", name = lang.name()),
        None => tr!(lang, "status.language_auto", name = lang.name()),
    });

    if !cfg.media.is_empty() {
        lines.push(tr!(lang, "status.media", list = cfg.media.join(", ")));
    }
    if cfg.reasons.is_empty() {
        lines.push(tr!(lang, "status.reasons_none"));
    } else {
        lines.push(tr!(lang, "status.reasons"));
        lines.extend(cfg.reasons.lines().map(|line| format!("  {line}")));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    const EN: Lang = Lang::EN;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        // 2026-10-05 is a Monday.
        NaiveDate::from_ymd_opt(2026, 10, 5)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn work_rule() -> Rule {
        Rule {
            name: "work".into(),
            days: vec![Weekday::Mon],
            start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
            lock: false,
            apps: vec!["Steam".into()],
        }
    }

    fn run_with(cfg: &mut Config, state: &State, req: Request, now: NaiveDateTime) -> Outcome {
        let rt = Runtime {
            daemon: true,
            paused_until: None,
            state,
        };
        handle(req, cfg, now, &rt, EN)
    }

    fn run(cfg: &mut Config, req: Request, now: NaiveDateTime) -> Outcome {
        run_with(cfg, &State::default(), req, now)
    }

    #[test]
    fn adding_is_always_allowed_and_normalizes_apps() {
        let mut cfg = Config::default();
        let out = run(&mut cfg, Request::RuleAdd { rule: work_rule() }, at(12, 0));
        assert!(out.response.ok && out.changed);
        assert_eq!(cfg.rules[0].apps, vec!["steam.exe"]);
        assert!(out.response.message.contains("active right now"));
    }

    #[test]
    fn invalid_rules_are_rejected() {
        let mut cfg = Config::default();
        run(&mut cfg, Request::RuleAdd { rule: work_rule() }, at(6, 0));
        // Duplicate name.
        assert!(
            !run(&mut cfg, Request::RuleAdd { rule: work_rule() }, at(6, 0))
                .response
                .ok
        );
        let mut useless = work_rule();
        useless.name = "useless".into();
        useless.apps.clear();
        assert!(
            !run(&mut cfg, Request::RuleAdd { rule: useless }, at(6, 0))
                .response
                .ok
        );
        let mut system = work_rule();
        system.name = "system".into();
        system.apps = vec!["winlogon.exe".into()];
        let refused = run(&mut cfg, Request::RuleAdd { rule: system }, at(6, 0)).response;
        assert!(
            !refused.ok && refused.message.contains("winlogon.exe"),
            "{}",
            refused.message
        );
        assert_eq!(cfg.rules.len(), 1);
    }

    #[test]
    fn removal_is_refused_while_active_or_about_to_start() {
        let mut cfg = Config::default();
        run(&mut cfg, Request::RuleAdd { rule: work_rule() }, at(6, 0));
        let remove = || Request::RuleRemove {
            name: "work".into(),
        };

        let active = run(&mut cfg, remove(), at(12, 0));
        assert!(!active.response.ok && !active.changed);
        assert!(!run(&mut cfg, remove(), at(8, 45)).response.ok);
        assert_eq!(cfg.rules.len(), 1);

        assert!(run(&mut cfg, remove(), at(8, 0)).response.ok);
        assert!(cfg.rules.is_empty());
    }

    #[test]
    fn now_creates_a_block_that_expires() {
        let mut cfg = Config::default();
        let now = |minutes, lock| Request::Now {
            minutes,
            lock,
            apps: vec![],
        };
        let out = run(&mut cfg, now(30, true), at(12, 0));
        assert!(out.response.ok);
        assert_eq!(cfg.oneoffs[0].until, at(12, 30));
        assert!(!prune(&mut cfg, at(12, 29)));
        assert!(prune(&mut cfg, at(12, 30)));
        assert!(cfg.oneoffs.is_empty());
        assert!(!run(&mut cfg, now(0, true), at(12, 0)).response.ok);
        assert!(!run(&mut cfg, now(5, false), at(12, 0)).response.ok);
    }

    #[test]
    fn settings_can_tighten_but_not_loosen_during_a_block() {
        let mut cfg = Config::default();
        run(&mut cfg, Request::RuleAdd { rule: work_rule() }, at(6, 0));
        let set = |key, value| Request::Set { key, value };

        // Loosening during the block is refused and leaves the value untouched.
        assert!(
            !run(&mut cfg, set(Setting::EmergencyChars, 10), at(12, 0))
                .response
                .ok
        );
        assert!(
            !run(&mut cfg, set(Setting::EmergencyMinutes, 30), at(12, 0))
                .response
                .ok
        );
        assert!(
            !run(&mut cfg, set(Setting::LeadMinutes, 0), at(12, 0))
                .response
                .ok
        );
        assert_eq!(
            cfg,
            Config {
                rules: cfg.rules.clone(),
                ..Config::default()
            }
        );

        // Lowering the lead time inside the lead window must not unlock itself.
        assert!(
            !run(&mut cfg, set(Setting::LeadMinutes, 0), at(8, 45))
                .response
                .ok
        );

        // Tightening is fine at any time.
        assert!(
            run(&mut cfg, set(Setting::EmergencyChars, 200), at(12, 0))
                .response
                .ok
        );
        assert!(
            run(&mut cfg, set(Setting::EmergencyChars, 0), at(12, 0))
                .response
                .ok
        );
        // Re-enabling the pause is loosening.
        assert!(
            !run(&mut cfg, set(Setting::EmergencyChars, 200), at(12, 0))
                .response
                .ok
        );
        // Outside any block everything is allowed.
        assert!(
            run(&mut cfg, set(Setting::EmergencyChars, 40), at(20, 0))
                .response
                .ok
        );
        assert_eq!(cfg.emergency_chars, 40);
    }

    #[test]
    fn breaks_can_be_tightened_any_time_but_loosened_only_early_in_a_work_period() {
        let mut cfg = Config::default();
        let set = |work_minutes, break_minutes| Request::BreakSet {
            work_minutes,
            break_minutes,
        };
        let fresh = State::default();
        let nearly_due = State {
            usage_secs: 40 * 60,
            ..State::default()
        };
        let resting = State {
            break_until: Some(at(12, 10)),
            ..State::default()
        };

        assert!(!run(&mut cfg, set(2, 10), at(12, 0)).response.ok);
        assert!(!run(&mut cfg, set(50, 0), at(12, 0)).response.ok);
        assert!(!run(&mut cfg, Request::BreakOff, at(12, 0)).changed);
        assert!(
            run_with(&mut cfg, &nearly_due, set(50, 10), at(12, 0))
                .response
                .ok
        );

        // Stricter: accepted even when a break is about to start or under way.
        assert!(
            run_with(&mut cfg, &nearly_due, set(45, 10), at(12, 0))
                .response
                .ok
        );
        assert!(
            run_with(&mut cfg, &resting, set(45, 15), at(12, 0))
                .response
                .ok
        );
        // Looser: refused in both cases.
        assert!(
            !run_with(&mut cfg, &nearly_due, set(60, 15), at(12, 0))
                .response
                .ok
        );
        assert!(
            !run_with(&mut cfg, &resting, Request::BreakOff, at(12, 0))
                .response
                .ok
        );
        assert_eq!(
            cfg.breaks,
            Some(BreakPolicy {
                work_minutes: 45,
                break_minutes: 15
            })
        );
        // Early in the work period it goes through.
        assert!(
            run_with(&mut cfg, &fresh, set(60, 15), at(12, 0))
                .response
                .ok
        );
        assert!(
            run_with(&mut cfg, &fresh, Request::BreakOff, at(12, 0))
                .response
                .ok
        );
        assert_eq!(cfg.breaks, None);
    }

    #[test]
    fn allowances_cannot_be_raised_once_the_app_was_used_today() {
        let mut cfg = Config::default();
        let set = |minutes| Request::AllowanceSet {
            app: "Game".into(),
            minutes,
        };
        let remove = || Request::AllowanceRemove {
            app: "game.exe".into(),
        };
        let now = at(12, 0);
        let played = State {
            day: Some(now.date()),
            app_secs: [("game.exe".to_string(), 90)].into(),
            ..State::default()
        };

        assert!(!run(&mut cfg, remove(), now).response.ok);
        assert!(!run(&mut cfg, set(0), now).response.ok);
        assert!(run(&mut cfg, set(60), now).response.ok);
        assert_eq!(
            cfg.allowances,
            vec![Allowance {
                app: "game.exe".into(),
                minutes: 60
            }]
        );

        assert!(run_with(&mut cfg, &played, set(30), now).response.ok);
        let refused = run_with(&mut cfg, &played, set(90), now).response;
        assert!(
            !refused.ok && refused.message.contains("1:30"),
            "{}",
            refused.message
        );
        assert!(!run_with(&mut cfg, &played, remove(), now).response.ok);
        assert_eq!(cfg.allowances[0].minutes, 30);

        // Not used yet today: both go through.
        assert!(run(&mut cfg, set(90), now).response.ok);
        assert!(run(&mut cfg, remove(), now).response.ok);
        assert!(cfg.allowances.is_empty());
    }

    #[test]
    fn language_can_be_chosen_or_left_to_windows() {
        let mut cfg = Config::default();
        let set = |code: Option<&str>| Request::LanguageSet {
            code: code.map(str::to_string),
        };
        let out = run(&mut cfg, set(Some("es-CO")), at(12, 0)).response;
        assert!(out.ok && out.message.contains("Español"), "{}", out.message);
        assert_eq!(cfg.language.as_deref(), Some("es"));
        assert!(!run(&mut cfg, set(Some("tlh")), at(12, 0)).response.ok);
        assert!(run(&mut cfg, set(None), at(12, 0)).response.ok);
        assert_eq!(cfg.language, None);
    }

    #[test]
    fn status_lists_everything_in_the_requested_language() {
        let mut cfg = Config::default();
        run(&mut cfg, Request::RuleAdd { rule: work_rule() }, at(6, 0));
        run(
            &mut cfg,
            Request::ReasonsSet {
                text: " Sleep more. ".into(),
            },
            at(6, 0),
        );
        run(
            &mut cfg,
            Request::BreakSet {
                work_minutes: 50,
                break_minutes: 10,
            },
            at(6, 0),
        );
        run(
            &mut cfg,
            Request::AllowanceSet {
                app: "game".into(),
                minutes: 60,
            },
            at(6, 0),
        );
        let state = State::default();
        let rt = Runtime {
            daemon: true,
            paused_until: None,
            state: &state,
        };

        let text = handle(Request::Status, &mut cfg, at(12, 0), &rt, EN)
            .response
            .message;
        assert!(
            text.contains("work: closes steam.exe until Mon 17:00"),
            "{text}"
        );
        assert!(
            text.contains("work: Mon 09:00-17:00, closes steam.exe"),
            "{text}"
        );
        assert!(text.contains("game.exe: 60 min per day"), "{text}");
        assert!(text.contains("Sleep more."), "{text}");

        let es = Lang::from_code("es").unwrap();
        let text = handle(Request::Status, &mut cfg, at(12, 0), &rt, es)
            .response
            .message;
        assert!(
            text.contains("work: cierra steam.exe hasta lun 17:00"),
            "{text}"
        );
    }

    #[test]
    fn days_are_described_compactly() {
        use Weekday::*;
        assert_eq!(
            describe_days(&[Mon, Tue, Wed, Thu, Fri, Sat, Sun], EN),
            "every day"
        );
        assert_eq!(describe_days(&[Fri, Mon, Wed, Tue, Thu], EN), "Mon-Fri");
        assert_eq!(describe_days(&[Sat, Sun], EN), "Sat,Sun");
        assert_eq!(describe_days(&[Mon, Wed, Fri], EN), "Mon,Wed,Fri");
        for lang in Lang::all() {
            for day in WEEK {
                assert!(
                    !day_name(day, lang).starts_with("day."),
                    "{} lacks {day}",
                    lang.code()
                );
            }
        }
    }
}
