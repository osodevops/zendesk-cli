//! Human-friendly time parsing (`--created-after "2 hours ago"`) and table-friendly formatting.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

use crate::{Result, ZdkError};

/// Parse a point in time from any of:
/// RFC 3339 (`2026-09-11T06:34:57Z`), `YYYY-MM-DD` (midnight UTC), `YYYY-MM-DD HH:MM[:SS]` (UTC),
/// a relative duration (`24h`, `30m`, `7d`, `1h30m` = that long ago), `"2 hours ago"`,
/// and the keywords `now`, `today`, `yesterday`.
pub fn parse_human_time(input: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let raw = input.trim();
    let lower = raw.to_ascii_lowercase();

    match lower.as_str() {
        "now" => return Ok(now),
        "today" => return Ok(start_of_day(now)),
        "yesterday" => return Ok(start_of_day(now) - chrono::Duration::days(1)),
        _ => {}
    }

    if let Some(ts) = parse_timestamp(raw) {
        return Ok(ts);
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap_or_default()));
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Ok(Utc.from_utc_datetime(&dt));
        }
    }

    let duration_text = lower
        .strip_suffix(" ago")
        .or_else(|| lower.strip_suffix("ago"))
        .unwrap_or(&lower);
    let compact: String = duration_text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if let Ok(dur) = humantime::parse_duration(&compact) {
        let dur = chrono::Duration::from_std(dur)
            .map_err(|e| ZdkError::Usage(format!("time '{input}' is out of range: {e}")))?;
        return Ok(now - dur);
    }

    Err(ZdkError::Usage(format!(
        "cannot parse time '{input}': use RFC 3339, YYYY-MM-DD, a duration like 24h/7d, '2 hours ago', or 'yesterday'"
    )))
}

/// Parse an RFC 3339 timestamp (the only shape Zendesk emits), returning `None` for anything else.
#[must_use]
pub fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Whether a string looks like a Zendesk timestamp (`2026-09-11T06:34:57Z`).
#[must_use]
pub fn looks_like_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[..4].iter().all(u8::is_ascii_digit)
        && parse_timestamp(s).is_some()
}

/// Local-time rendering used in tables: `YYYY-MM-DD HH:MM`.
#[must_use]
pub fn format_local(t: &DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn start_of_day(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.from_utc_datetime(&t.date_naive().and_hms_opt(0, 0, 0).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 11, 12, 0, 0).unwrap()
    }

    #[test]
    fn parses_every_supported_shape() {
        let n = now();
        let cases = [
            (
                "2026-09-11T06:34:57Z",
                Utc.with_ymd_and_hms(2026, 9, 11, 6, 34, 57).unwrap(),
            ),
            (
                "2026-09-11T08:34:57+02:00",
                Utc.with_ymd_and_hms(2026, 9, 11, 6, 34, 57).unwrap(),
            ),
            (
                "2026-09-01",
                Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
            ),
            (
                "2026-09-01 10:30",
                Utc.with_ymd_and_hms(2026, 9, 1, 10, 30, 0).unwrap(),
            ),
            ("24h", n - chrono::Duration::hours(24)),
            ("30m", n - chrono::Duration::minutes(30)),
            ("7d", n - chrono::Duration::days(7)),
            ("1h30m", n - chrono::Duration::minutes(90)),
            ("2 hours ago", n - chrono::Duration::hours(2)),
            ("3 days ago", n - chrono::Duration::days(3)),
            (
                "yesterday",
                Utc.with_ymd_and_hms(2026, 9, 10, 0, 0, 0).unwrap(),
            ),
            ("today", Utc.with_ymd_and_hms(2026, 9, 11, 0, 0, 0).unwrap()),
            ("now", n),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_human_time(input, n).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn rejects_garbage_with_a_usage_error() {
        let err = parse_human_time("last tuesday-ish", now()).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("cannot parse time"));
    }

    #[test]
    fn timestamp_detection_is_strict() {
        assert!(looks_like_timestamp("2026-09-11T06:34:57Z"));
        assert!(looks_like_timestamp("2026-09-11T06:34:57.123+01:00"));
        assert!(!looks_like_timestamp("2026-09-11"));
        assert!(!looks_like_timestamp("hello world 2026-09-11T06:34:57Z"));
        assert!(!looks_like_timestamp("12345678901234567890"));
    }

    #[test]
    fn local_format_has_minute_precision() {
        let s = format_local(&now());
        assert_eq!(s.len(), 16, "{s}");
        assert_eq!(&s[10..11], " ");
    }
}
