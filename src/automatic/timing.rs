//! When a schedule is due. Pure functions, so they are easy to test.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::config::{Frequency, Schedule, Weekday};
use crate::state::{AutomaticOutcome, ScheduleState};

/// "At start" schedules wait a little, so signing in stays quick.
pub const AT_START_DELAY: Duration = Duration::minutes(3);
/// Without "make up missed backups", a run may still start this late.
pub const ON_TIME_WINDOW: Duration = Duration::minutes(30);
/// A backup skipped because the drive was missing is tried again after this.
pub const RETRY_AFTER: Duration = Duration::minutes(15);

fn time_of(schedule: &Schedule) -> NaiveTime {
    NaiveTime::parse_from_str(schedule.time.trim(), "%H:%M")
        .unwrap_or_else(|_| NaiveTime::from_hms_opt(20, 0, 0).unwrap_or_default())
}

fn at(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    // `earliest` also resolves times inside a daylight-saving change.
    Local.from_local_datetime(&date.and_time(time)).earliest()
}

fn matches_day(schedule: &Schedule, date: NaiveDate) -> bool {
    schedule.frequency != Frequency::Weekly
        || date.weekday().num_days_from_monday() == weekday_index(schedule.weekday)
}

fn weekday_index(day: Weekday) -> u32 {
    day as u32
}

/// The most recent planned time at or before `now`.
pub fn previous_occurrence(schedule: &Schedule, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let time = time_of(schedule);
    match schedule.frequency {
        Frequency::AtStart => None,
        Frequency::Hourly => {
            let step = i64::from(schedule.every_hours.clamp(1, 23)) * 60;
            let anchor = at(now.date_naive(), time)?;
            let minutes = (now - anchor).num_minutes();
            Some(anchor + Duration::minutes(minutes.div_euclid(step) * step))
        }
        Frequency::Daily | Frequency::Weekly => (0..8)
            .filter_map(|back| {
                let date = now.date_naive() - Duration::days(back);
                matches_day(schedule, date)
                    .then(|| at(date, time))
                    .flatten()
            })
            .find(|candidate| *candidate <= now),
    }
}

/// The next planned time after `now` (for display).
pub fn next_occurrence(schedule: &Schedule, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let time = time_of(schedule);
    match schedule.frequency {
        Frequency::AtStart => None,
        Frequency::Hourly => {
            let step = Duration::hours(i64::from(schedule.every_hours.clamp(1, 23)));
            previous_occurrence(schedule, now).map(|prev| prev + step)
        }
        Frequency::Daily | Frequency::Weekly => (0..8)
            .filter_map(|ahead| {
                let date = now.date_naive() + Duration::days(ahead);
                matches_day(schedule, date)
                    .then(|| at(date, time))
                    .flatten()
            })
            .find(|candidate| *candidate > now),
    }
}

/// Whether `schedule` should run now.
///
/// * Occurrences before the schedule was switched on (`armed_at`) are ignored.
/// * A missed occurrence runs as soon as possible with `catch_up`, otherwise
///   only within [`ON_TIME_WINDOW`].
/// * A run skipped because the drive was not connected is retried every
///   [`RETRY_AFTER`] until the next occurrence.
pub fn is_due(
    schedule: &Schedule,
    state: &ScheduleState,
    now: DateTime<Local>,
    process_start: DateTime<Local>,
) -> bool {
    if !schedule.enabled {
        return false;
    }
    let local = |t: DateTime<Utc>| t.with_timezone(&Local);
    let armed = state.armed_at.map(local);
    let attempt = state.last_attempt.map(local);

    if schedule.frequency == Frequency::AtStart {
        let start = armed.map_or(process_start, |a| a.max(process_start));
        return now >= start + AT_START_DELAY && attempt.is_none_or(|a| a < process_start);
    }

    let Some(previous) = previous_occurrence(schedule, now) else {
        return false;
    };
    if armed.is_none_or(|a| a > previous) {
        return false;
    }
    if let Some(attempt) = attempt
        && attempt >= previous
    {
        let retryable = matches!(
            state.last_run.as_ref().map(|r| r.outcome),
            Some(
                AutomaticOutcome::DestinationUnavailable
                    | AutomaticOutcome::AlreadyRunning
                    | AutomaticOutcome::Interrupted
            )
        );
        return schedule.catch_up && retryable && now - attempt >= RETRY_AFTER;
    }
    schedule.catch_up || now - previous <= ON_TIME_WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AutomaticRun;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(y, m, d)
                    .unwrap()
                    .and_hms_opt(h, min, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
    }

    fn daily(time: &str) -> Schedule {
        Schedule {
            id: "a".into(),
            frequency: Frequency::Daily,
            time: time.into(),
            ..Schedule::default()
        }
    }

    fn armed(at: DateTime<Local>) -> ScheduleState {
        ScheduleState {
            armed_at: Some(at.with_timezone(&Utc)),
            ..ScheduleState::default()
        }
    }

    #[test]
    fn occurrences_daily_weekly_hourly() {
        let now = local(2026, 9, 16, 14, 10); // a Wednesday
        let schedule = daily("20:00");
        assert_eq!(
            previous_occurrence(&schedule, now),
            Some(local(2026, 9, 15, 20, 0))
        );
        assert_eq!(
            next_occurrence(&schedule, now),
            Some(local(2026, 9, 16, 20, 0))
        );

        let weekly = Schedule {
            frequency: Frequency::Weekly,
            weekday: Weekday::Sunday,
            ..daily("09:30")
        };
        assert_eq!(
            previous_occurrence(&weekly, now),
            Some(local(2026, 9, 13, 9, 30))
        );
        assert_eq!(
            next_occurrence(&weekly, now),
            Some(local(2026, 9, 20, 9, 30))
        );

        let hourly = Schedule {
            frequency: Frequency::Hourly,
            every_hours: 4,
            ..daily("08:00")
        };
        assert_eq!(
            previous_occurrence(&hourly, now),
            Some(local(2026, 9, 16, 12, 0))
        );
        assert_eq!(
            next_occurrence(&hourly, now),
            Some(local(2026, 9, 16, 16, 0))
        );
        // Before the anchor time the grid continues from yesterday.
        let early = local(2026, 9, 16, 3, 0);
        assert_eq!(
            previous_occurrence(&hourly, early),
            Some(local(2026, 9, 16, 0, 0))
        );
    }

    #[test]
    fn a_new_schedule_does_not_run_for_the_past() {
        let schedule = daily("20:00");
        let now = local(2026, 9, 16, 14, 0);
        let start = now - Duration::hours(1);
        assert!(!is_due(&schedule, &armed(now), now, start));
        // Nothing is armed yet: nothing runs.
        assert!(!is_due(&schedule, &ScheduleState::default(), now, start));
        // At 20:00 it runs.
        let evening = local(2026, 9, 16, 20, 0);
        assert!(is_due(&schedule, &armed(now), evening, start));
    }

    #[test]
    fn missed_backups_are_caught_up_once() {
        let schedule = daily("20:00");
        let armed_state = armed(local(2026, 9, 1, 12, 0));
        // The computer was off at 20:00; it is switched on the next morning.
        let morning = local(2026, 9, 16, 8, 0);
        assert!(is_due(&schedule, &armed_state, morning, morning));

        let without_catch_up = Schedule {
            catch_up: false,
            ..schedule.clone()
        };
        assert!(!is_due(&without_catch_up, &armed_state, morning, morning));
        let shortly_after = local(2026, 9, 15, 20, 20);
        assert!(is_due(
            &without_catch_up,
            &armed_state,
            shortly_after,
            morning
        ));

        // After an attempt it waits for the next occurrence …
        let mut ran = armed_state.clone();
        ran.last_attempt = Some(morning.with_timezone(&Utc));
        ran.last_run = Some(AutomaticRun {
            at: morning.with_timezone(&Utc),
            outcome: AutomaticOutcome::Complete,
            message: String::new(),
            files: 0,
            bytes: 0,
            schedule: String::new(),
            problems: crate::state::Problems::default(),
        });
        assert!(!is_due(
            &schedule,
            &ran,
            morning + Duration::hours(2),
            morning
        ));
        // … unless the drive was missing: then it tries again later.
        if let Some(run) = &mut ran.last_run {
            run.outcome = AutomaticOutcome::DestinationUnavailable;
        }
        assert!(!is_due(
            &schedule,
            &ran,
            morning + Duration::minutes(5),
            morning
        ));
        assert!(is_due(
            &schedule,
            &ran,
            morning + Duration::minutes(20),
            morning
        ));
        // An interrupted backup is made up the same way.
        if let Some(run) = &mut ran.last_run {
            run.outcome = AutomaticOutcome::Interrupted;
        }
        assert!(is_due(
            &schedule,
            &ran,
            morning + Duration::minutes(20),
            morning
        ));
    }

    #[test]
    fn at_start_runs_once_per_start_after_a_delay() {
        let schedule = Schedule {
            frequency: Frequency::AtStart,
            ..daily("20:00")
        };
        let start = local(2026, 9, 16, 8, 0);
        let state = armed(start - Duration::days(3));
        assert!(!is_due(
            &schedule,
            &state,
            start + Duration::minutes(1),
            start
        ));
        assert!(is_due(
            &schedule,
            &state,
            start + Duration::minutes(4),
            start
        ));
        let mut ran = state.clone();
        ran.last_attempt = Some((start + Duration::minutes(4)).with_timezone(&Utc));
        assert!(!is_due(&schedule, &ran, start + Duration::hours(5), start));
    }
}
