//! Parsers for every rate-limit header Zendesk emits (PRD §4.3).
//!
//! Two spellings exist for the account budget (`X-Rate-Limit*` and the IETF `RateLimit-*`),
//! `Retry-After` may be seconds or an HTTP-date, and the two sub-budgets use a
//! `total=…; remaining=…; resets=…` structured form.

use std::time::Duration;

use chrono::{DateTime, Utc};
use http::HeaderMap;
use serde::Serialize;

/// A `total=; remaining=; resets=` sub-budget (`zendesk-ratelimit-tickets-index`,
/// `zendesk-ratelimit-inflight-jobs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SubBudget {
    pub total: Option<u32>,
    pub remaining: Option<u32>,
    #[serde(serialize_with = "serialize_secs_opt")]
    pub resets: Option<Duration>,
}

impl SubBudget {
    /// Parse `total=100; remaining=99; resets=41` (order and spacing free).
    #[must_use]
    pub fn parse(value: &str) -> Self {
        let mut out = Self::default();
        for part in value.split([';', ',']) {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim().to_ascii_lowercase().as_str() {
                "total" | "limit" => out.total = leading_u32(v),
                "remaining" => out.remaining = leading_u32(v),
                "resets" | "reset" => out.resets = leading_u64(v).map(Duration::from_secs),
                _ => {}
            }
        }
        out
    }

    /// The budget is spent for this window.
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.remaining == Some(0)
    }
}

/// The rate-limit view of one response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RateHeaders {
    /// `X-Rate-Limit` / `RateLimit-Limit`.
    pub limit: Option<u32>,
    /// `X-Rate-Limit-Remaining` / `RateLimit-Remaining`.
    pub remaining: Option<u32>,
    /// `RateLimit-Reset` (seconds until the window resets).
    #[serde(serialize_with = "serialize_secs_opt")]
    pub reset: Option<Duration>,
    /// `Retry-After` (seconds or HTTP-date, normalised to a duration from now).
    #[serde(serialize_with = "serialize_secs_opt")]
    pub retry_after: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tickets_index: Option<SubBudget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inflight_jobs: Option<SubBudget>,
}

impl RateHeaders {
    /// Parse from a response header map, using the wall clock for HTTP-date `Retry-After`.
    #[must_use]
    pub fn parse(headers: &HeaderMap) -> Self {
        Self::parse_at(headers, Utc::now())
    }

    /// [`parse`](Self::parse) with an explicit "now" (tests).
    #[must_use]
    pub fn parse_at(headers: &HeaderMap, now: DateTime<Utc>) -> Self {
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let first = |names: &[&str]| names.iter().find_map(|n| get(n));

        Self {
            limit: first(&["x-rate-limit", "ratelimit-limit", "x-ratelimit-limit"])
                .and_then(leading_u32),
            remaining: first(&[
                "x-rate-limit-remaining",
                "ratelimit-remaining",
                "x-ratelimit-remaining",
            ])
            .and_then(leading_u32),
            reset: first(&["ratelimit-reset", "x-rate-limit-reset", "x-ratelimit-reset"])
                .and_then(leading_u64)
                .map(Duration::from_secs),
            retry_after: get("retry-after").and_then(|v| parse_retry_after(v, now)),
            tickets_index: get("zendesk-ratelimit-tickets-index").map(SubBudget::parse),
            inflight_jobs: get("zendesk-ratelimit-inflight-jobs").map(SubBudget::parse),
        }
    }

    /// No rate-limit information at all (e.g. a transport error or a non-Zendesk host).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `Retry-After` is either an integer number of seconds or an IMF-fixdate (RFC 7231 §7.1.3).
/// A date in the past yields zero.
#[must_use]
pub fn parse_retry_after(value: &str, now: DateTime<Utc>) -> Option<Duration> {
    let value = value.trim();
    if let Some(secs) = leading_u64(value) {
        return Some(Duration::from_secs(secs));
    }
    if let Some(millis) = value
        .parse::<f64>()
        .ok()
        .filter(|f| f.is_finite() && *f >= 0.0)
    {
        return Some(Duration::from_secs_f64(millis));
    }
    let date = parse_http_date(value)?;
    let delta = date - now;
    Some(delta.to_std().unwrap_or(Duration::ZERO))
}

/// RFC 7231 IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), plus the RFC 850 and asctime
/// forms the spec still tolerates.
#[must_use]
pub fn parse_http_date(value: &str) -> Option<DateTime<Utc>> {
    // RFC 2822 is a superset of IMF-fixdate; normalise the obsolete zone names chrono rejects.
    let normalised = value
        .trim()
        .trim_end_matches(" GMT")
        .trim_end_matches(" UTC")
        .trim_end_matches(" UT")
        .to_string()
        + " +0000";
    if let Ok(d) = DateTime::parse_from_rfc2822(&normalised) {
        return Some(d.with_timezone(&Utc));
    }
    for fmt in ["%A, %d-%b-%y %H:%M:%S", "%a %b %e %H:%M:%S %Y"] {
        if let Ok(naive) =
            chrono::NaiveDateTime::parse_from_str(normalised.trim_end_matches(" +0000"), fmt)
        {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
        }
    }
    None
}

fn leading_u64(s: &str) -> Option<u64> {
    let digits: String = s.trim().chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn leading_u32(s: &str) -> Option<u32> {
    leading_u64(s).and_then(|n| u32::try_from(n).ok())
}

// serde needs the `&Option<T>` shape.
#[allow(clippy::ref_option)]
fn serialize_secs_opt<S: serde::Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    match d {
        Some(d) => s.serialize_some(&d.as_secs()),
        None => s.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use http::HeaderValue;

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn x_rate_limit_family() {
        let h = map(&[
            ("X-Rate-Limit", "700"),
            ("X-Rate-Limit-Remaining", "699"),
            ("Retry-After", "12"),
        ]);
        let r = RateHeaders::parse(&h);
        assert_eq!(r.limit, Some(700));
        assert_eq!(r.remaining, Some(699));
        assert_eq!(r.retry_after, Some(Duration::from_secs(12)));
        assert!(r.reset.is_none());
    }

    #[test]
    fn ietf_ratelimit_family_including_reset() {
        let h = map(&[
            ("ratelimit-limit", "400"),
            ("ratelimit-remaining", "5"),
            ("ratelimit-reset", "41"),
        ]);
        let r = RateHeaders::parse(&h);
        assert_eq!(r.limit, Some(400));
        assert_eq!(r.remaining, Some(5));
        assert_eq!(r.reset, Some(Duration::from_secs(41)));
        assert!(!r.is_empty());
    }

    #[test]
    fn structured_values_take_the_leading_integer() {
        let h = map(&[("ratelimit-limit", "700, 700;w=60")]);
        assert_eq!(RateHeaders::parse(&h).limit, Some(700));
    }

    #[test]
    fn sub_budgets_parse_total_remaining_resets() {
        let h = map(&[
            (
                "zendesk-ratelimit-tickets-index",
                "total=100; remaining=99; resets=41",
            ),
            (
                "zendesk-ratelimit-inflight-jobs",
                "total=30;remaining=0;resets=60",
            ),
        ]);
        let r = RateHeaders::parse(&h);
        let t = r.tickets_index.unwrap();
        assert_eq!((t.total, t.remaining), (Some(100), Some(99)));
        assert_eq!(t.resets, Some(Duration::from_secs(41)));
        assert!(!t.exhausted());
        let j = r.inflight_jobs.unwrap();
        assert!(j.exhausted());
        assert_eq!(j.total, Some(30));
    }

    #[test]
    fn retry_after_http_date_is_relative_to_now_and_never_negative() {
        let now = Utc.with_ymd_and_hms(1994, 11, 6, 8, 49, 0).unwrap();
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now),
            Some(Duration::from_secs(37))
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:40:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_retry_after("Sunday, 06-Nov-94 08:49:37 GMT", now),
            Some(Duration::from_secs(37))
        );
        assert_eq!(
            parse_retry_after("Sun Nov  6 08:49:37 1994", now),
            Some(Duration::from_secs(37))
        );
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("garbage", now), None);
        let h = map(&[("Retry-After", "Sun, 06 Nov 1994 08:49:37 GMT")]);
        assert_eq!(
            RateHeaders::parse_at(&h, now).retry_after,
            Some(Duration::from_secs(37))
        );
    }

    #[test]
    fn empty_map_is_empty_and_serialises_seconds() {
        let r = RateHeaders::parse(&HeaderMap::new());
        assert!(r.is_empty());
        let h = map(&[("ratelimit-reset", "9"), ("x-rate-limit", "1")]);
        let json = serde_json::to_value(RateHeaders::parse(&h)).unwrap();
        assert_eq!(json["reset"], 9);
        assert_eq!(json["limit"], 1);
        assert!(json["retry_after"].is_null());
    }
}
