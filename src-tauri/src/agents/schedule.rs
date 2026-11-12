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

/// Whether the cron schedule fires exactly at `unix_ms`.
pub fn cron_fires_at(expression: &str, timezone: &str, unix_ms: i64) -> Result<bool, String> {
    let schedule = parse_cron(expression)?;
    let at = parse_timezone(timezone)?
        .timestamp_millis_opt(unix_ms)
        .single()
        .ok_or_else(|| format!("Invalid timestamp: {}", unix_ms))?;
    Ok(schedule.includes(at))
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
    // Users write 5-field Vixie cron (`min hour dom mon dow`), optionally
    // followed by a year. The `cron` crate requires a leading seconds
    // field; pad with `"0 "` so schedules fire on the minute boundary.
    let mut fields = cron_fields(trimmed);
    if !(5..=6).contains(&fields.len()) {
        return Err("Invalid cron expression: it must have 5 fields \
                    (minute hour day-of-month month day-of-week)."
            .to_string());
    }
    fields[4] = day_of_week_names(&fields[4])?;
    let padded = format!("0 {}", fields.join(" "));
    Schedule::from_str(&padded).map_err(|e| format!("Invalid cron expression: {}", e))
}

/// Splits on whitespace, rejoining tokens around `,`, `-` and `/`: the
/// `cron` crate accepted spaces there (`1, 3`), so stored expressions may
/// contain them.
fn cron_fields(expression: &str) -> Vec<String> {
    const JOINERS: [char; 3] = [',', '-', '/'];
    let mut fields: Vec<String> = Vec::new();
    for token in expression.split_whitespace() {
        match fields.last_mut() {
            Some(last) if last.ends_with(JOINERS) || token.starts_with(JOINERS) => {
                last.push_str(token);
            }
            _ => fields.push(token.to_owned()),
        }
    }
    fields
}

const DAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Rewrites a Vixie day-of-week field (0 or 7 = Sunday, 1 = Monday) as
/// the list of day names it selects. The `cron` crate numbers days
/// 1 = Sunday … 7 = Saturday, so numbers can't be passed through, and
/// a range like `5-7` has no equivalent there; names mean the same day
/// in both.
fn day_of_week_names(field: &str) -> Result<String, String> {
    let mut selected = [false; 7];
    for item in field.split(',') {
        let days = day_of_week_item(item).ok_or_else(|| {
            let shown = if item.is_empty() { field } else { item };
            format!(
                "Invalid cron expression: `{}` is not a valid day of the week (0-7 or Sun-Sat).",
                shown
            )
        })?;
        for day in days {
            selected[day % 7] = true;
        }
    }
    let names: Vec<&str> = DAY_NAMES
        .iter()
        .zip(selected)
        .filter_map(|(name, on)| on.then_some(*name))
        .collect();
    Ok(names.join(","))
}

/// Days (0..=7) selected by one list item: `*`, a day, or a range, with
/// an optional `/step`. As in cronie, a range ending in Sunday (`Fri-Sun`,
/// `5-0`) ends at 7. cronie rejects a single day with a step; the `cron`
/// crate accepted it, so it stays valid and runs to 7 (`1/2` = Mon, Wed,
/// Fri, Sun).
fn day_of_week_item(item: &str) -> Option<impl Iterator<Item = usize>> {
    let (base, step) = match item.split_once('/') {
        Some((base, step)) => (base, Some(number(step).filter(|&step| step > 0)?)),
        None => (item, None),
    };
    let (first, last) = if base == "*" || base == "?" {
        (0, 7)
    } else if let Some((first, last)) = base.split_once('-') {
        let (first, last) = (day_of_week(first)?, day_of_week(last)?);
        (first, if last == 0 && first > 0 { 7 } else { last })
    } else {
        let day = day_of_week(base)?;
        (day, if step.is_some() { 7 } else { day })
    };
    (first <= last).then(|| (first..=last).step_by(step.unwrap_or(1)))
}

fn day_of_week(token: &str) -> Option<usize> {
    if let Some(day) = number(token) {
        return (day <= 7).then_some(day);
    }
    let day = match token.to_ascii_lowercase().as_str() {
        "sun" | "sunday" => 0,
        "mon" | "monday" => 1,
        "tue" | "tues" | "tuesday" => 2,
        "wed" | "wednesday" => 3,
        "thu" | "thurs" | "thursday" => 4,
        "fri" | "friday" => 5,
        "sat" | "saturday" => 6,
        _ => return None,
    };
    Some(day)
}

fn number(token: &str) -> Option<usize> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
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

    /// Regression for the `intervalMinutes` deserialization bug: the FE
    /// sends camelCase keys and serde must honor them for fields *inside*
    /// the variant. Without `rename_all_fields = "camelCase"` on the
    /// enum, the field name stays snake_case (`interval_minutes`) and a
    /// camelCase payload silently defaults to 0, tripping the
    /// "≥1 minute" validator with a confusing message.
    #[test]
    fn camel_case_payload_round_trips_through_serde() {
        let json = r#"{"type":"interval","intervalMinutes":1440}"#;
        let kind: ScheduleKind = serde_json::from_str(json).unwrap();
        match kind {
            ScheduleKind::Interval { interval_minutes } => {
                assert_eq!(interval_minutes, 1440);
            }
            other => panic!("expected Interval, got {:?}", other),
        }
        // Round-trip back to JSON: must serialize as camelCase too so
        // the on-disk shape matches the wire shape.
        let reserialized = serde_json::to_string(&ScheduleKind::Interval {
            interval_minutes: 1440,
        })
        .unwrap();
        assert!(reserialized.contains("intervalMinutes"));
        assert!(!reserialized.contains("interval_minutes"));
    }

    /// Sunday 2026-11-08 00:00 UTC.
    fn week_start() -> i64 {
        chrono::Utc
            .with_ymd_and_hms(2026, 11, 8, 0, 0, 0)
            .unwrap()
            .timestamp_millis()
    }

    /// The first `n` UTC fire times of `expression` after `week_start()`.
    fn runs(expression: &str, n: usize) -> Result<Vec<chrono::DateTime<chrono::Utc>>, String> {
        let kind = ScheduleKind::Cron {
            expression: expression.to_string(),
            timezone: "UTC".to_string(),
        };
        Ok(upcoming_cron_runs(&kind, week_start(), n)?
            .into_iter()
            .map(|ms| chrono::Utc.timestamp_millis_opt(ms).unwrap())
            .collect())
    }

    /// Weekdays on which `0 9 * * <dow>` fires during the week starting
    /// at `week_start()`, in firing order.
    fn fire_days(dow: &str) -> Result<Vec<chrono::Weekday>, String> {
        use chrono::Datelike;
        let week_end = week_start() + 7 * 24 * 60 * 60 * 1000;
        Ok(runs(&format!("0 9 * * {dow}"), 8)?
            .into_iter()
            .filter(|run| run.timestamp_millis() < week_end)
            .map(|run| run.weekday())
            .collect())
    }

    fn utc(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
    ) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn cron_fires_at_matches_only_fire_times() {
        let monday_9 = utc(2026, 11, 9, 9, 0).timestamp_millis();
        assert_eq!(cron_fires_at("0 9 * * 1", "UTC", monday_9), Ok(true));
        assert_eq!(
            cron_fires_at("0 9 * * 1", "UTC", monday_9 - 24 * 60 * 60 * 1000),
            Ok(false)
        );
        assert_eq!(
            cron_fires_at("0 9 * * 1", "UTC", monday_9 + 60_000),
            Ok(false)
        );
        assert!(cron_fires_at("0 9 * * 8", "UTC", monday_9).is_err());
    }

    #[test]
    fn day_of_week_numbers_follow_standard_cron() {
        use chrono::Weekday::*;
        let cases: &[(&str, &[chrono::Weekday])] = &[
            ("1", &[Mon]),
            ("1-5", &[Mon, Tue, Wed, Thu, Fri]),
            ("0", &[Sun]),
            ("7", &[Sun]),
            ("6", &[Sat]),
            ("5-7", &[Sun, Fri, Sat]),
            ("0-6", &[Sun, Mon, Tue, Wed, Thu, Fri, Sat]),
            ("1,3,5", &[Mon, Wed, Fri]),
            ("0,7", &[Sun]),
            ("*/2", &[Sun, Tue, Thu, Sat]),
            ("1-5/2", &[Mon, Wed, Fri]),
            ("5-7/2", &[Sun, Fri]),
            ("1/3", &[Sun, Mon, Thu]),
            ("6-7", &[Sun, Sat]),
            ("7-7", &[Sun]),
            ("0-7", &[Sun, Mon, Tue, Wed, Thu, Fri, Sat]),
            ("?/2", &[Sun, Tue, Thu, Sat]),
            ("*", &[Sun, Mon, Tue, Wed, Thu, Fri, Sat]),
            ("?", &[Sun, Mon, Tue, Wed, Thu, Fri, Sat]),
        ];
        for (dow, expected) in cases {
            assert_eq!(&fire_days(dow).unwrap(), expected, "dow `{dow}`");
        }
    }

    #[test]
    fn day_of_week_names_are_unchanged_and_mix_with_numbers() {
        use chrono::Weekday::*;
        let cases: &[(&str, &[chrono::Weekday])] = &[
            ("MON", &[Mon]),
            ("SUN", &[Sun]),
            ("sat", &[Sat]),
            ("Thurs", &[Thu]),
            ("MON-FRI", &[Mon, Tue, Wed, Thu, Fri]),
            ("Mon-Fri/2", &[Mon, Wed, Fri]),
            ("SUN,3,Fri", &[Sun, Wed, Fri]),
            ("Mon-3", &[Mon, Tue, Wed]),
            ("5-Sat", &[Fri, Sat]),
            ("Fri-Sun", &[Sun, Fri, Sat]),
            ("5-0", &[Sun, Fri, Sat]),
            ("Sun-Sun", &[Sun]),
            ("Sat-Sun", &[Sun, Sat]),
            ("Mon/2", &[Sun, Mon, Wed, Fri]),
        ];
        for (dow, expected) in cases {
            assert_eq!(&fire_days(dow).unwrap(), expected, "dow `{dow}`");
        }
    }

    #[test]
    fn invalid_day_of_week_is_rejected() {
        for dow in ["8", "0-8", "5-2", "Fri-Mon", "*/0", "1,", "Funday", "1-5/x"] {
            let err = fire_days(dow).unwrap_err();
            assert!(
                err.starts_with("Invalid cron expression:") && err.contains("day of the week"),
                "dow `{dow}`: {err}"
            );
        }
        // `-1` joins the month field (`*-1`), leaving four fields.
        assert!(fire_days("-1").unwrap_err().contains("5 fields"));
    }

    #[test]
    fn empty_day_of_week_item_quotes_the_whole_field() {
        for dow in ["1,", "1,,2", "Mon,"] {
            let err = fire_days(dow).unwrap_err();
            assert!(err.contains(&format!("`{dow}` is not")), "{err}");
        }
        // A leading comma joins the month field (`*,1`), leaving four fields.
        assert!(fire_days(",1").unwrap_err().contains("5 fields"));
    }

    #[test]
    fn spaces_around_list_range_and_step_separators_are_joined() {
        use chrono::Weekday::*;
        assert_eq!(fire_days("1, 3").unwrap(), [Mon, Wed]);
        assert_eq!(fire_days("1 ,3").unwrap(), [Mon, Wed]);
        assert_eq!(fire_days("1 - 3").unwrap(), [Mon, Tue, Wed]);
        assert_eq!(fire_days("*/ 3").unwrap(), [Sun, Wed, Sat]);
        assert_eq!(
            runs("0, 30 9 * * 1", 2).unwrap(),
            [utc(2026, 11, 9, 9, 0), utc(2026, 11, 9, 9, 30)]
        );
    }

    #[test]
    fn field_count_must_be_five_or_six_with_a_year() {
        for expression in ["0 9 * *", "0 9 * * 1 2027 3"] {
            let err = runs(expression, 1).unwrap_err();
            assert!(
                err.starts_with("Invalid cron expression:") && err.contains("5 fields"),
                "`{expression}`: {err}"
            );
        }
        assert_eq!(runs("0 9 * * 1 2026", 1).unwrap(), [utc(2026, 11, 9, 9, 0)]);
    }

    #[test]
    fn other_fields_pass_through_unchanged() {
        assert_eq!(
            runs("0 9 1-5 JAN *", 6).unwrap(),
            [
                utc(2027, 1, 1, 9, 0),
                utc(2027, 1, 2, 9, 0),
                utc(2027, 1, 3, 9, 0),
                utc(2027, 1, 4, 9, 0),
                utc(2027, 1, 5, 9, 0),
                utc(2028, 1, 1, 9, 0),
            ]
        );
    }

    #[test]
    fn camel_case_cron_payload_round_trips_through_serde() {
        let json = r#"{"type":"cron","expression":"0 9 * * 1-5","timezone":"UTC"}"#;
        let kind: ScheduleKind = serde_json::from_str(json).unwrap();
        match kind {
            ScheduleKind::Cron {
                expression,
                timezone,
            } => {
                assert_eq!(expression, "0 9 * * 1-5");
                assert_eq!(timezone, "UTC");
            }
            other => panic!("expected Cron, got {:?}", other),
        }
    }
}
