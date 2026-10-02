//! Keeps count of how long the computer and individual programs have been in
//! use, for forced breaks and daily allowances.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Input within the last minute counts as using the computer.
pub const ACTIVE_WINDOW_SECS: u64 = 60;
/// A longer gap between two updates means the computer or the daemon was off,
/// which is rest, not use.
const MAX_STEP_SECS: u64 = 10;

/// What survives a restart of the daemon, so killing it does not reset anything.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct State {
    pub last_seen: Option<NaiveDateTime>,
    /// Use accumulated since the last proper rest.
    pub usage_secs: u64,
    /// Length of the current stretch without use.
    pub rest_secs: u64,
    pub break_until: Option<NaiveDateTime>,
    /// The day `app_secs` refers to.
    pub day: Option<NaiveDate>,
    /// Time each program with an allowance has spent in the foreground today.
    pub app_secs: BTreeMap<String, u64>,
}

/// What is known about the user's session at this instant.
#[derive(Clone, Debug, Default)]
pub struct Sensors {
    /// Seconds since the last keyboard or mouse input; `None` when nobody is
    /// reporting, which is counted as use so that silencing the reporter
    /// earns nothing.
    pub idle_secs: Option<u64>,
    /// Programs with an allowance that are in use right now.
    pub in_use: Vec<String>,
    /// The lock screen is up, so the computer cannot be used.
    pub locked: bool,
}

impl State {
    /// Moves the clocks forward to `now`. Returns true when a break starts.
    pub fn advance(&mut self, config: &Config, now: NaiveDateTime, sensors: &Sensors) -> bool {
        let elapsed = self
            .last_seen
            .map(|last| (now - last).num_seconds().max(0) as u64)
            .unwrap_or(0);
        self.last_seen = Some(now);
        let gap = elapsed > MAX_STEP_SECS;

        if self.day != Some(now.date()) {
            self.day = Some(now.date());
            self.app_secs.clear();
        }
        if !gap && !sensors.locked {
            for app in &sensors.in_use {
                *self.app_secs.entry(app.clone()).or_default() += elapsed;
            }
        }

        let Some(policy) = config.breaks else {
            self.usage_secs = 0;
            self.rest_secs = 0;
            self.break_until = None;
            return false;
        };
        let break_secs = policy.break_minutes as u64 * 60;
        if self.break_until.is_some_and(|until| until <= now) {
            self.break_until = None;
        }

        let idle = sensors.idle_secs.unwrap_or(0);
        let resting =
            gap || sensors.locked || self.break_until.is_some() || idle >= ACTIVE_WINDOW_SECS;
        if resting {
            // The idle time reported is itself a stretch without use.
            self.rest_secs = (self.rest_secs + elapsed).max(idle);
        } else {
            self.rest_secs = 0;
            self.usage_secs += elapsed;
        }
        if self.rest_secs >= break_secs {
            self.usage_secs = 0;
        }

        if self.break_until.is_none() && self.usage_secs >= policy.work_minutes as u64 * 60 {
            self.break_until = Some(now + Duration::seconds(break_secs as i64));
            self.usage_secs = 0;
            self.rest_secs = 0;
            return true;
        }
        false
    }

    pub fn on_break(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        self.break_until.filter(|until| *until > now)
    }

    /// Use left before the next forced break.
    pub fn break_due_in(&self, config: &Config) -> Option<Duration> {
        let work = config.breaks?.work_minutes as u64 * 60;
        Some(Duration::seconds(
            work.saturating_sub(self.usage_secs) as i64
        ))
    }

    pub fn used_today(&self, app: &str, now: NaiveDateTime) -> u64 {
        if self.day != Some(now.date()) {
            return 0;
        }
        self.app_secs.get(app).copied().unwrap_or(0)
    }

    /// Time `app` may still be used today, if it has an allowance.
    pub fn allowance_left(
        &self,
        config: &Config,
        app: &str,
        now: NaiveDateTime,
    ) -> Option<Duration> {
        let limit = config.allowance(app)?.minutes as u64 * 60;
        Some(Duration::seconds(
            limit.saturating_sub(self.used_today(app, now)) as i64,
        ))
    }

    /// Programs whose allowance for today is spent.
    pub fn exhausted(&self, config: &Config, now: NaiveDateTime) -> Vec<String> {
        config
            .allowances
            .iter()
            .filter(|a| self.used_today(&a.app, now) >= a.minutes as u64 * 60)
            .map(|a| a.app.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Allowance, BreakPolicy};

    fn at(day: u32, h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, day)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    fn config() -> Config {
        Config {
            breaks: Some(BreakPolicy {
                work_minutes: 50,
                break_minutes: 10,
            }),
            allowances: vec![Allowance {
                app: "game.exe".into(),
                minutes: 1,
            }],
            ..Config::default()
        }
    }

    fn active() -> Sensors {
        Sensors {
            idle_secs: Some(2),
            ..Sensors::default()
        }
    }

    fn idle(secs: u64) -> Sensors {
        Sensors {
            idle_secs: Some(secs),
            ..Sensors::default()
        }
    }

    /// Ticks once a second for `secs` seconds starting at `from`.
    fn run(
        state: &mut State,
        cfg: &Config,
        from: NaiveDateTime,
        secs: i64,
        sensors: &Sensors,
    ) -> bool {
        (0..=secs).any(|i| state.advance(cfg, from + Duration::seconds(i), sensors))
    }

    #[test]
    fn continuous_use_ends_in_a_break() {
        let (cfg, mut state) = (config(), State::default());
        assert!(!run(
            &mut state,
            &cfg,
            at(5, 9, 0, 0),
            50 * 60 - 1,
            &active()
        ));
        assert_eq!(state.break_due_in(&cfg), Some(Duration::seconds(1)));
        assert!(state.advance(&cfg, at(5, 9, 50, 0), &active()));
        assert_eq!(state.on_break(at(5, 9, 50, 0)), Some(at(5, 10, 0, 0)));
        assert_eq!(state.usage_secs, 0);
        // The break ends on its own and use starts counting again.
        assert!(!run(&mut state, &cfg, at(5, 9, 50, 1), 10 * 60, &active()));
        assert_eq!(state.on_break(at(5, 10, 0, 1)), None);
        assert!(state.usage_secs <= 2, "{}", state.usage_secs);
    }

    #[test]
    fn stepping_away_long_enough_counts_as_the_break() {
        let (cfg, mut state) = (config(), State::default());
        run(&mut state, &cfg, at(5, 9, 0, 0), 30 * 60, &active());
        assert_eq!(state.usage_secs, 30 * 60);
        // Nine minutes idle is not a full break: the count is kept.
        state.advance(&cfg, at(5, 9, 39, 0), &idle(9 * 60));
        assert_eq!(state.usage_secs, 30 * 60);
        // Ten minutes is.
        state.advance(&cfg, at(5, 9, 40, 0), &idle(10 * 60));
        assert_eq!(state.usage_secs, 0);
    }

    #[test]
    fn short_pauses_neither_count_as_use_nor_reset_it() {
        let (cfg, mut state) = (config(), State::default());
        run(&mut state, &cfg, at(5, 9, 0, 0), 600, &active());
        // Idle for two minutes (reported as such once the minute threshold passes).
        for i in 0..120 {
            state.advance(
                &cfg,
                at(5, 9, 10, 1) + Duration::seconds(i),
                &idle(60 + i as u64),
            );
        }
        assert_eq!(state.usage_secs, 600);
        run(&mut state, &cfg, at(5, 9, 12, 1), 60, &active());
        assert_eq!(state.usage_secs, 661);
    }

    #[test]
    fn time_with_the_computer_off_is_rest() {
        let (cfg, mut state) = (config(), State::default());
        run(&mut state, &cfg, at(5, 9, 0, 0), 30 * 60, &active());
        // Next seen the following morning.
        assert!(!state.advance(&cfg, at(6, 8, 0, 0), &active()));
        assert_eq!(state.usage_secs, 0);
    }

    #[test]
    fn an_unknown_idle_time_counts_as_use() {
        let (cfg, mut state) = (config(), State::default());
        run(&mut state, &cfg, at(5, 9, 0, 0), 100, &Sensors::default());
        assert_eq!(state.usage_secs, 100);
    }

    #[test]
    fn a_locked_screen_is_rest() {
        let (cfg, mut state) = (config(), State::default());
        run(&mut state, &cfg, at(5, 9, 0, 0), 600, &active());
        let locked = Sensors {
            idle_secs: Some(0),
            locked: true,
            ..Sensors::default()
        };
        run(&mut state, &cfg, at(5, 9, 10, 1), 10 * 60, &locked);
        assert_eq!(state.usage_secs, 0);
    }

    #[test]
    fn without_a_break_policy_nothing_is_counted() {
        let cfg = Config::default();
        let mut state = State {
            usage_secs: 500,
            break_until: Some(at(5, 10, 0, 0)),
            ..State::default()
        };
        assert!(!run(&mut state, &cfg, at(5, 9, 0, 0), 10, &active()));
        assert_eq!((state.usage_secs, state.break_until), (0, None));
    }

    #[test]
    fn allowances_count_foreground_time_and_reset_each_day() {
        let (cfg, mut state) = (config(), State::default());
        let playing = Sensors {
            idle_secs: Some(0),
            in_use: vec!["game.exe".into()],
            locked: false,
        };
        run(&mut state, &cfg, at(5, 9, 0, 0), 59, &playing);
        assert_eq!(
            state.allowance_left(&cfg, "game.exe", at(5, 9, 1, 0)),
            Some(Duration::seconds(1))
        );
        assert!(state.exhausted(&cfg, at(5, 9, 1, 0)).is_empty());
        // In the background it does not count.
        run(&mut state, &cfg, at(5, 9, 1, 0), 30, &active());
        assert_eq!(state.used_today("game.exe", at(5, 9, 2, 0)), 59);
        state.advance(&cfg, at(5, 9, 1, 31), &playing);
        assert_eq!(state.exhausted(&cfg, at(5, 9, 2, 0)), vec!["game.exe"]);
        // A new day brings a new budget, even before the first update of that day.
        assert!(state.exhausted(&cfg, at(6, 0, 0, 1)).is_empty());
        state.advance(&cfg, at(6, 8, 0, 0), &active());
        assert_eq!(state.used_today("game.exe", at(6, 8, 0, 0)), 0);
        assert_eq!(
            state.allowance_left(&cfg, "other.exe", at(6, 8, 0, 0)),
            None
        );
    }
}
