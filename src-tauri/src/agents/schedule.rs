//! Schedule computation — single source of truth for "when does the
//! manager run next?" Used by the runner after each completion, by
//! `workspace_set_schedule` when arming a fresh anchor, and by
//! `workspace_preview_schedule` for the UI's live next-N-runs preview.
//!
//! The function takes a `ScheduleKind` and a wall-clock `now` (Unix ms),
//! and returns the next fire time as Unix ms. For `Interval` mode this
//! is just `now + interval_ms`; for `Cron` it parses a 5-field
//! expression, computes the next fire in the user's IANA timezone, and
//! converts back to Unix ms.
//!
//! Errors:
//! - `Interval` with `interval_minutes == 0`: rejected — would degenerate
//!   into an infinite tight loop.
//! - `Cron` with an empty or malformed expression / unknown timezone:
//!   rejected with a message safe to surface to the UI.
//! - `Cron` whose iterator yields no upcoming time (e.g. a pattern that
//!   matches a year-end edge case far in the past): rejected.

use std::str::FromStr;

use chrono::TimeZone;
use cron::Schedule;

use crate::config::workspace_config::ScheduleKind;

/// Compute the next fire time for the given schedule, strictly after
/// `now_unix_ms`. Returns the result in Unix ms (UTC), ready to persist
/// into `WorkspaceSchedule.next_run_at_unix_ms`.
pub fn compute_next_run_at(kind: &ScheduleKind, now_unix_ms: i64) -> Result<i64, String> {
    match kind {
        ScheduleKind::Interval { interval_minutes } => {
            if *interval_minutes == 0 {
                return Err("Interval schedule requires at least 1 minute.".to_string());
            }
            let interval_ms = (*interval_minutes as i64).saturating_mul(60_000);
            Ok(now_unix_ms.saturating_add(interval_ms))
        }
        ScheduleKind::Cron {
            expression,
            timezone,
        } => compute_next_cron_run_at(expression, timezone, now_unix_ms),
    }
}

/// Compute the next `n` cron fire times for the live preview in the UI.
/// Returns an empty list if `kind` is not Cron — interval mode has no
/// list to display beyond the single computed next-run.
pub fn upcoming_cron_runs(
    kind: &ScheduleKind,
    now_unix_ms: i64,
    n: usize,
) -> Result<Vec<i64>, String> {
    let ScheduleKind::Cron {
        expression,
        timezone,
    } = kind
    else {
        return Ok(Vec::new());
    };
    let schedule = parse_cron(expression)?;
    let tz = parse_timezone(timezone)?;
    let now = tz
        .timestamp_millis_opt(now_unix_ms)
        .single()
        .ok_or_else(|| format!("Invalid `now` timestamp: {}", now_unix_ms))?;
    Ok(schedule
        .after(&now)
        .take(n)
        .map(|dt| dt.timestamp_millis())
        .collect())
}

fn compute_next_cron_run_at(
    expression: &str,
    timezone: &str,
    now_unix_ms: i64,
) -> Result<i64, String> {
    let schedule = parse_cron(expression)?;
    let tz = parse_timezone(timezone)?;
    let now = tz
        .timestamp_millis_opt(now_unix_ms)
        .single()
        .ok_or_else(|| format!("Invalid `now` timestamp: {}", now_unix_ms))?;
    schedule
        .after(&now)
        .next()
        .map(|dt| dt.timestamp_millis())
        .ok_or_else(|| {
            "Cron expression has no upcoming fire times — check that the pattern is valid."
                .to_string()
        })
}

fn parse_cron(expression: &str) -> Result<Schedule, String> {
    let trimmed = expression.trim();
    if trimmed.is_empty() {
        return Err("Cron expression is empty.".to_string());
    }
    // Users write 5-field Vixie cron (`min hour dom mon dow`). The
    // `cron` crate requires a leading seconds field; pad with `"0 "` so
    // schedules fire on the minute boundary.
    let padded = format!("0 {}", trimmed);
    Schedule::from_str(&padded).map_err(|e| format!("Invalid cron expression: {}", e))
}

fn parse_timezone(name: &str) -> Result<chrono_tz::Tz, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Cron schedule requires a timezone.".to_string());
    }
    chrono_tz::Tz::from_str(trimmed).map_err(|_| format!("Unknown IANA timezone: `{}`", trimmed))
}

/// Best-effort detection of the host's IANA timezone, exposed to the FE
/// as the default value for new cron schedules. Falls back to `"UTC"`
/// if the platform can't resolve the local zone.
pub fn host_timezone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_returns_now_plus_minutes() {
        let now = 1_000_000_000_000_i64;
        let kind = ScheduleKind::Interval {
            interval_minutes: 5,
        };
        assert_eq!(compute_next_run_at(&kind, now).unwrap(), now + 5 * 60_000);
    }

    #[test]
    fn interval_zero_is_rejected() {
        let kind = ScheduleKind::Interval {
            interval_minutes: 0,
        };
        assert!(compute_next_run_at(&kind, 0).is_err());
    }

    #[test]
    fn cron_returns_future_unix_ms() {
        let kind = ScheduleKind::Cron {
            // Every minute, UTC.
            expression: "* * * * *".to_string(),
            timezone: "UTC".to_string(),
        };
        let now = chrono::Utc::now().timestamp_millis();
        let next = compute_next_run_at(&kind, now).unwrap();
        assert!(
            next > now,
            "next ({}) must be strictly after now ({})",
            next,
            now
        );
        // Should fire within the next 60 seconds.
        assert!((next - now) <= 60_000);
    }

    #[test]
    fn cron_invalid_expression_is_rejected() {
        let kind = ScheduleKind::Cron {
            expression: "not a cron".to_string(),
            timezone: "UTC".to_string(),
        };
        let err = compute_next_run_at(&kind, 0).unwrap_err();
        assert!(err.to_lowercase().contains("invalid cron"));
    }

    #[test]
    fn cron_unknown_timezone_is_rejected() {
        let kind = ScheduleKind::Cron {
            expression: "* * * * *".to_string(),
            timezone: "Imaginary/Place".to_string(),
        };
        let err = compute_next_run_at(&kind, 0).unwrap_err();
        assert!(err.to_lowercase().contains("timezone"));
    }

    #[test]
    fn cron_empty_timezone_is_rejected() {
        let kind = ScheduleKind::Cron {
            expression: "* * * * *".to_string(),
            timezone: String::new(),
        };
        assert!(compute_next_run_at(&kind, 0).is_err());
    }

    #[test]
    fn upcoming_cron_runs_returns_n_strictly_increasing_times() {
        let kind = ScheduleKind::Cron {
            expression: "0 * * * *".to_string(), // top of every hour
            timezone: "UTC".to_string(),
        };
        let now = chrono::Utc::now().timestamp_millis();
        let upcoming = upcoming_cron_runs(&kind, now, 3).unwrap();
        assert_eq!(upcoming.len(), 3);
        assert!(upcoming[0] > now);
        assert!(upcoming[1] > upcoming[0]);
        assert!(upcoming[2] > upcoming[1]);
    }

    #[test]
    fn upcoming_cron_runs_is_empty_for_interval_kind() {
        let kind = ScheduleKind::Interval {
            interval_minutes: 5,
        };
        let now = chrono::Utc::now().timestamp_millis();
        assert_eq!(
            upcoming_cron_runs(&kind, now, 3).unwrap(),
            Vec::<i64>::new()
        );
    }
}
