use chrono::{Datelike, Duration, NaiveDateTime, NaiveTime, Weekday};
use serde::{Deserialize, Serialize};

/// Processes that must never be blocked: killing them takes the session (or
/// the whole machine) down, or would make despegate fight itself.
const PROTECTED_APPS: &[&str] = &[
    "despegate.exe",
    "despegated.exe",
    "csrss.exe",
    "dwm.exe",
    "explorer.exe",
    "lsass.exe",
    "services.exe",
    "smss.exe",
    "svchost.exe",
    "wininit.exe",
    "winlogon.exe",
];

pub const WEEK: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Language code for everything despegate says. Unset follows Windows.
    pub language: Option<String>,
    /// "light", "dark" or "system" (follow Windows) for the window and the lock screen.
    pub appearance: String,
    /// The user's own words on why despegate is installed. Shown on the lock
    /// screen and before uninstalling.
    pub reasons: String,
    /// Photos and videos shown with the reasons; file names in the media directory.
    pub media: Vec<String>,
    /// A rule cannot be removed (nor settings weakened) this close to a block.
    pub lead_minutes: u32,
    /// How long before a block the warning banner appears.
    pub warn_minutes: u32,
    /// Length of the text to type for an emergency pause. 0 disables the pause.
    pub emergency_chars: u32,
    pub emergency_minutes: u32,
    /// Forced breaks after a stretch of continuous use.
    pub breaks: Option<BreakPolicy>,
    /// Daily time budgets for individual programs.
    pub allowances: Vec<Allowance>,
    pub rules: Vec<Rule>,
    pub oneoffs: Vec<OneOff>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: None,
            appearance: "system".into(),
            reasons: String::new(),
            media: Vec::new(),
            lead_minutes: 30,
            warn_minutes: 5,
            emergency_chars: 80,
            emergency_minutes: 5,
            breaks: None,
            allowances: Vec::new(),
            rules: Vec::new(),
            oneoffs: Vec::new(),
        }
    }
}

/// After `work_minutes` of use without a proper rest, the screen locks for
/// `break_minutes`. Stepping away for that long on your own counts as the break.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct BreakPolicy {
    pub work_minutes: u32,
    pub break_minutes: u32,
}

/// `app` may be in the foreground for `minutes` per day; after that it is
/// closed until the next day.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Allowance {
    pub app: String,
    pub minutes: u32,
}

/// A recurring block: every listed day from `start` to `end`. When `end` is not
/// after `start` the window runs past midnight and belongs to the day it starts.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Rule {
    pub name: String,
    pub days: Vec<Weekday>,
    pub start: NaiveTime,
    pub end: NaiveTime,
    /// Take over the whole screen.
    #[serde(default)]
    pub lock: bool,
    /// Executable names to close while the block is active.
    #[serde(default)]
    pub apps: Vec<String>,
}

/// A block started on the spot with `despegate now`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OneOff {
    pub name: String,
    pub until: NaiveDateTime,
    #[serde(default)]
    pub lock: bool,
    #[serde(default)]
    pub apps: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveBlock {
    pub name: String,
    pub until: NaiveDateTime,
    pub lock: bool,
    pub apps: Vec<String>,
}

impl Rule {
    fn window(&self, day: chrono::NaiveDate) -> (NaiveDateTime, NaiveDateTime) {
        let start = day.and_time(self.start);
        let end = if self.end > self.start {
            day.and_time(self.end)
        } else {
            (day + Duration::days(1)).and_time(self.end)
        };
        (start, end)
    }

    /// End of the window containing `now`, if the rule is active.
    pub fn active_until(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        [now.date(), now.date() - Duration::days(1)]
            .into_iter()
            .filter(|day| self.days.contains(&day.weekday()))
            .map(|day| self.window(day))
            .find(|(start, end)| *start <= now && now < *end)
            .map(|(_, end)| end)
    }

    /// Start of the next window that begins after `now`.
    pub fn next_start(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        (0..=7)
            .map(|i| now.date() + Duration::days(i))
            .filter(|day| self.days.contains(&day.weekday()))
            .map(|day| day.and_time(self.start))
            .find(|start| *start > now)
    }
}

impl Config {
    pub fn active_blocks(&self, now: NaiveDateTime) -> Vec<ActiveBlock> {
        let rules = self.rules.iter().filter_map(|r| {
            r.active_until(now).map(|until| ActiveBlock {
                name: r.name.clone(),
                until,
                lock: r.lock,
                apps: r.apps.clone(),
            })
        });
        let oneoffs = self
            .oneoffs
            .iter()
            .filter(|o| o.until > now)
            .map(|o| ActiveBlock {
                name: o.name.clone(),
                until: o.until,
                lock: o.lock,
                apps: o.apps.clone(),
            });
        rules.chain(oneoffs).collect()
    }

    /// The inactive rule that starts soonest.
    pub fn next_start(&self, now: NaiveDateTime) -> Option<(&Rule, NaiveDateTime)> {
        self.rules
            .iter()
            .filter(|r| r.active_until(now).is_none())
            .filter_map(|r| r.next_start(now).map(|start| (r, start)))
            .min_by_key(|(_, start)| *start)
    }

    pub fn allowance(&self, app: &str) -> Option<&Allowance> {
        self.allowances.iter().find(|a| a.app == app)
    }
}

#[derive(Debug, PartialEq)]
pub enum AppError {
    Empty,
    /// A system process that must not be blocked.
    Protected(String),
}

/// Canonical form of an executable name: bare file name, lowercase, `.exe`.
pub fn normalize_app(name: &str) -> Result<String, AppError> {
    let name = name
        .trim()
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    if name.is_empty() {
        return Err(AppError::Empty);
    }
    let name = if name.ends_with(".exe") {
        name
    } else {
        format!("{name}.exe")
    };
    if PROTECTED_APPS.contains(&name.as_str()) {
        return Err(AppError::Protected(name));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(day: u32, h: u32, m: u32) -> NaiveDateTime {
        // October 2026: the 5th is a Monday.
        NaiveDate::from_ymd_opt(2026, 10, day)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn rule(days: &[Weekday], start: (u32, u32), end: (u32, u32)) -> Rule {
        Rule {
            name: "r".into(),
            days: days.to_vec(),
            start: NaiveTime::from_hms_opt(start.0, start.1, 0).unwrap(),
            end: NaiveTime::from_hms_opt(end.0, end.1, 0).unwrap(),
            lock: true,
            apps: vec![],
        }
    }

    #[test]
    fn same_day_window() {
        let r = rule(&[Weekday::Mon], (9, 0), (17, 0));
        assert_eq!(r.active_until(at(5, 8, 59)), None);
        assert_eq!(r.active_until(at(5, 9, 0)), Some(at(5, 17, 0)));
        assert_eq!(r.active_until(at(5, 16, 59)), Some(at(5, 17, 0)));
        assert_eq!(r.active_until(at(5, 17, 0)), None);
        assert_eq!(r.active_until(at(6, 12, 0)), None);
    }

    #[test]
    fn overnight_window_belongs_to_its_start_day() {
        let r = rule(&[Weekday::Mon], (23, 0), (7, 0));
        assert_eq!(r.active_until(at(5, 23, 30)), Some(at(6, 7, 0)));
        // Tuesday morning is still Monday's window.
        assert_eq!(r.active_until(at(6, 6, 59)), Some(at(6, 7, 0)));
        assert_eq!(r.active_until(at(6, 7, 0)), None);
        // Monday morning is not covered: Sunday is not in the rule.
        assert_eq!(r.active_until(at(5, 3, 0)), None);
        // Tuesday night is not covered either.
        assert_eq!(r.active_until(at(6, 23, 30)), None);
    }

    #[test]
    fn next_start_skips_to_the_following_week() {
        let r = rule(&[Weekday::Mon], (9, 0), (17, 0));
        assert_eq!(r.next_start(at(5, 8, 0)), Some(at(5, 9, 0)));
        assert_eq!(r.next_start(at(5, 9, 0)), Some(at(12, 9, 0)));
        assert_eq!(r.next_start(at(7, 0, 0)), Some(at(12, 9, 0)));
    }

    #[test]
    fn oneoffs_are_active_until_they_expire() {
        let mut cfg = Config::default();
        cfg.oneoffs.push(OneOff {
            name: "now".into(),
            until: at(5, 10, 0),
            lock: false,
            apps: vec!["a.exe".into()],
        });
        assert_eq!(cfg.active_blocks(at(5, 9, 59)).len(), 1);
        assert!(cfg.active_blocks(at(5, 10, 0)).is_empty());
    }

    #[test]
    fn app_names_are_normalized() {
        assert_eq!(normalize_app(r"C:\Games\Steam.EXE").unwrap(), "steam.exe");
        assert_eq!(normalize_app("chrome").unwrap(), "chrome.exe");
        assert_eq!(
            normalize_app("csrss"),
            Err(AppError::Protected("csrss.exe".into()))
        );
        assert!(normalize_app("despegated.exe").is_err());
        assert_eq!(normalize_app("  "), Err(AppError::Empty));
    }

    #[test]
    fn config_round_trips_through_toml() {
        let mut cfg = Config {
            language: Some("es".into()),
            reasons: "Sleep.\nSecond line.".into(),
            breaks: Some(BreakPolicy {
                work_minutes: 50,
                break_minutes: 10,
            }),
            ..Config::default()
        };
        cfg.allowances.push(Allowance {
            app: "game.exe".into(),
            minutes: 60,
        });
        cfg.rules
            .push(rule(&[Weekday::Mon, Weekday::Fri], (23, 0), (7, 0)));
        cfg.oneoffs.push(OneOff {
            name: "now".into(),
            until: at(5, 10, 0),
            lock: true,
            apps: vec![],
        });
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), cfg);
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }
}
