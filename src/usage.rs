//! Keeps count of how long the computer and individual programs have been in
//! use, for forced breaks and daily allowances, and of the breaks taken, for
//! the history of each day.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::config::{BreakPolicy, Config};

/// Input within the last minute counts as using the computer.
pub const ACTIVE_WINDOW_SECS: u64 = 60;
/// A longer gap between two updates means the computer or the daemon was off,
/// which is rest, not use.
const MAX_STEP_SECS: u64 = 10;
/// Days of history kept.
const HISTORY_DAYS: i64 = 60;

/// What survives a restart of the daemon, so killing it does not reset anything.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct State {
    pub last_seen: Option<NaiveDateTime>,
    /// Use accumulated since the last proper rest.
    pub usage_secs: u64,
    /// Length of the current stretch without use.
    pub rest_secs: u64,
    /// How much the current stretch without use has already counted for:
    /// nothing yet, a short break, or a long one.
    pub rest_counted: Rested,
    pub break_until: Option<NaiveDateTime>,
    /// Whether the break under way is the long one.
    pub break_long: bool,
    /// Short breaks taken since the last long one.
    pub cycle: u32,
    /// The day `app_secs` refers to.
    pub day: Option<NaiveDate>,
    /// Time each program with an allowance has spent in the foreground today.
    pub app_secs: BTreeMap<String, u64>,
    /// Use and breaks, day by day.
    pub history: BTreeMap<NaiveDate, DayLog>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum Rested {
    #[default]
    Nothing,
    Short,
    Long,
}

/// What a day added up to while breaks were on.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(default)]
pub struct DayLog {
    /// Time in use.
    pub work_secs: u64,
    /// Breaks taken, forced or on one's own.
    pub breaks: u32,
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
            self.rest_counted = Rested::Nothing;
            self.break_until = None;
            self.break_long = false;
            self.cycle = 0;
            return false;
        };
        let break_secs = policy.break_minutes as u64 * 60;
        let long_secs = policy.long_break_minutes as u64 * 60;
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
            self.rest_counted = Rested::Nothing;
            self.usage_secs += elapsed;
            self.log(now).work_secs += elapsed;
        }

        // Stepping away for as long as a break counts as one.
        if self.rest_secs >= break_secs && self.rest_counted < Rested::Short {
            self.rest_counted = Rested::Short;
            self.usage_secs = 0;
            self.log(now).breaks += 1;
            if policy.has_long() {
                self.cycle = (self.cycle + 1).min(policy.cycles - 1);
            }
        }
        if policy.has_long() && self.rest_secs >= long_secs && self.rest_counted < Rested::Long {
            self.rest_counted = Rested::Long;
            self.cycle = 0;
        }

        if self.break_until.is_none() && self.usage_secs >= policy.work_minutes as u64 * 60 {
            let long = self.next_is_long(&policy);
            let secs = if long { long_secs } else { break_secs };
            self.break_until = Some(now + Duration::seconds(secs as i64));
            self.break_long = long;
            self.cycle = if long || !policy.has_long() {
                0
            } else {
                self.cycle + 1
            };
            self.usage_secs = 0;
            self.rest_secs = 0;
            self.rest_counted = if long { Rested::Long } else { Rested::Short };
            self.log(now).breaks += 1;
            return true;
        }
        false
    }

    /// Today's entry in the history, made if missing; old days are dropped.
    fn log(&mut self, now: NaiveDateTime) -> &mut DayLog {
        let oldest = now.date() - Duration::days(HISTORY_DAYS);
        self.history.retain(|day, _| *day > oldest);
        self.history.entry(now.date()).or_default()
    }

    /// Whether the next forced break is the long one.
    pub fn next_is_long(&self, policy: &BreakPolicy) -> bool {
        policy.has_long() && self.cycle + 1 >= policy.cycles
    }

    /// Whether a long rest, forced or on one's own, has just happened or is under way.
    pub fn rested_long(&self) -> bool {
        self.rest_counted == Rested::Long
    }

    /// Whether a focus session has reached its end: the long break, or any
    /// break when there are no long ones.
    pub fn focus_over(&self, policy: Option<&BreakPolicy>) -> bool {
        match policy {
            None => true,
            Some(policy) if policy.has_long() => self.rested_long(),
            Some(_) => self.rest_counted >= Rested::Short,
        }
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

    /// Days in a row, up to `today`, with at least one break taken. A day
    /// with none yet does not break the streak until it is over.
    pub fn streak(&self, today: NaiveDate) -> u32 {
        let took = |day: NaiveDate| self.history.get(&day).is_some_and(|log| log.breaks > 0);
        let mut day = if took(today) {
            today
        } else {
            today - Duration::days(1)
        };
        let mut streak = 0;
        while took(day) {
            streak += 1;
            day -= Duration::days(1);
        }
        streak
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Allowance;

    fn at(day: u32, h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, day)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    fn config() -> Config {
        Config {
            breaks: Some(BreakPolicy::short(50, 10)),
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

    fn pomodoro() -> Config {
        Config {
            breaks: Some(BreakPolicy {
                work_minutes: 25,
                break_minutes: 5,
                long_break_minutes: 20,
                cycles: 4,
            }),
            ..Config::default()
        }
    }

    /// Works until the next forced break starts; returns when it does.
    fn work_until_break(state: &mut State, cfg: &Config, from: NaiveDateTime) -> NaiveDateTime {
        let mut now = from;
        while !state.advance(cfg, now, &active()) {
            now += Duration::seconds(1);
        }
        now
    }

    #[test]
    fn every_fourth_break_is_the_long_one() {
        let (cfg, mut state) = (pomodoro(), State::default());
        let mut now = at(5, 9, 0, 0);
        let mut lengths = Vec::new();
        for _ in 0..5 {
            now = work_until_break(&mut state, &cfg, now);
            let until = state.on_break(now).unwrap();
            lengths.push((until - now).num_minutes());
            now = until + Duration::seconds(1);
        }
        assert_eq!(lengths, vec![5, 5, 5, 20, 5]);
        assert_eq!(state.history[&at(5, 0, 0, 0).date()].breaks, 5);
    }

    #[test]
    fn a_long_rest_on_ones_own_starts_the_cycles_over() {
        let (cfg, mut state) = (pomodoro(), State::default());
        let now = work_until_break(&mut state, &cfg, at(5, 9, 0, 0));
        let after = state.on_break(now).unwrap();
        let now = work_until_break(&mut state, &cfg, after);
        assert_eq!(state.cycle, 2);
        // Away for half an hour after that break: longer than the long break.
        let back = state.on_break(now).unwrap();
        state.advance(&cfg, back + Duration::minutes(30), &idle(30 * 60));
        assert!(state.rested_long());
        assert_eq!(state.cycle, 0);
        // The two forced breaks counted; the long rest that followed them is not a third.
        assert_eq!(state.history[&at(5, 0, 0, 0).date()].breaks, 2);
    }

    #[test]
    fn a_short_rest_on_ones_own_counts_once() {
        let (cfg, mut state) = (pomodoro(), State::default());
        run(&mut state, &cfg, at(5, 9, 0, 0), 600, &active());
        for i in 0..400 {
            state.advance(
                &cfg,
                at(5, 9, 10, 1) + Duration::seconds(i),
                &idle(60 + i as u64),
            );
        }
        assert_eq!((state.cycle, state.usage_secs), (1, 0));
        assert_eq!(state.history[&at(5, 0, 0, 0).date()].breaks, 1);
        assert_eq!(state.history[&at(5, 0, 0, 0).date()].work_secs, 600);
    }

    #[test]
    fn the_streak_counts_days_with_a_break() {
        let mut state = State::default();
        for day in [2, 3, 4, 6] {
            state.history.insert(
                at(day, 0, 0, 0).date(),
                DayLog {
                    work_secs: 60,
                    breaks: 1,
                },
            );
        }
        // Today (the 6th) has one, the 5th has none: the streak is just today.
        assert_eq!(state.streak(at(6, 0, 0, 0).date()), 1);
        // On the 5th, before any break, the streak still runs back to the 2nd.
        assert_eq!(state.streak(at(5, 0, 0, 0).date()), 3);
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
