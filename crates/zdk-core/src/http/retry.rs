//! The pure retry decision (PRD §10.2 "429 handling"): `Retry-After` wins, then exponential
//! backoff with full jitter; 5xx and transport errors retry only when the request can be
//! replayed safely.

use std::time::Duration;

use crate::config::{Jitter, RateLimitSettings, RateLimitStrategy, RetrySettings};

/// Everything [`decide`] needs, resolved from `[retry]` and `[rate_limit]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first (default 6).
    pub max_attempts: u32,
    pub base_ms: u64,
    pub max_ms: u64,
    pub jitter: Jitter,
    /// Statuses that are retried when the request is replayable (default 429, 500, 502, 503, 504).
    pub retry_on: Vec<u16>,
    /// Honour `Retry-After` over local backoff (default true).
    pub respect_retry_after: bool,
    /// `--rate-limit-strategy fail`: a 429 fails immediately instead of retrying.
    pub fail_fast_on_429: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_ms: 1000,
            max_ms: 60_000,
            jitter: Jitter::Full,
            retry_on: vec![429, 500, 502, 503, 504],
            respect_retry_after: true,
            fail_fast_on_429: false,
        }
    }
}

impl RetryPolicy {
    /// Build from the resolved settings.
    #[must_use]
    pub fn from_settings(retry: &RetrySettings, rate: &RateLimitSettings) -> Self {
        Self {
            max_attempts: retry.max_attempts.max(1),
            base_ms: retry.base_ms,
            max_ms: retry.max_ms,
            jitter: retry.jitter,
            retry_on: retry.retry_on.clone(),
            respect_retry_after: rate.respect_retry_after,
            fail_fast_on_429: rate.strategy == RateLimitStrategy::Fail,
        }
    }

    /// Default statuses, `max_attempts` attempts and no sleeping between them (tests).
    #[must_use]
    pub fn instant(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            base_ms: 0,
            max_ms: 0,
            ..Self::default()
        }
    }
}

/// How an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A response arrived.
    Status {
        status: u16,
        /// Parsed `Retry-After`, if the response carried one.
        retry_after: Option<Duration>,
        /// The request may be sent again after having been sent (idempotent method, or an
        /// `Idempotency-Key` was attached; never for multipart).
        replayable: bool,
    },
    /// No response: connect failure, timeout, or the body could not be read.
    Transport {
        /// Whether the request may have reached the server (`false` for connect failures).
        sent: bool,
        replayable: bool,
    },
}

/// What to do after `attempt` ended with an [`Outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Sleep this long, then try again.
    Retry(Duration),
    /// Give up and surface the error.
    Fail,
}

/// Decide for the `attempt`-th attempt (1-based) that just failed.
#[must_use]
pub fn decide(attempt: u32, outcome: &Outcome, policy: &RetryPolicy) -> Decision {
    if attempt >= policy.max_attempts {
        return Decision::Fail;
    }
    match *outcome {
        Outcome::Status {
            status: 429,
            retry_after,
            ..
        } => {
            if policy.fail_fast_on_429 {
                return Decision::Fail;
            }
            // A 429 was rejected before processing, so even a keyless POST is safe to resend.
            match retry_after.filter(|_| policy.respect_retry_after) {
                Some(d) => Decision::Retry(d),
                None => Decision::Retry(backoff(attempt, policy)),
            }
        }
        Outcome::Status {
            status,
            retry_after,
            replayable,
        } => {
            if !replayable || !policy.retry_on.contains(&status) {
                return Decision::Fail;
            }
            match retry_after.filter(|_| policy.respect_retry_after) {
                Some(d) => Decision::Retry(d),
                None => Decision::Retry(backoff(attempt, policy)),
            }
        }
        Outcome::Transport { sent, replayable } => {
            if sent && !replayable {
                Decision::Fail
            } else {
                Decision::Retry(backoff(attempt, policy))
            }
        }
    }
}

/// `min(max_ms, base_ms · 2^(attempt-1))` — the upper bound of the jittered wait.
#[must_use]
pub fn backoff_ceiling(attempt: u32, policy: &RetryPolicy) -> Duration {
    let exp = attempt.saturating_sub(1).min(30);
    let raw = policy.base_ms.saturating_mul(1u64 << exp);
    Duration::from_millis(raw.min(policy.max_ms))
}

/// The jittered wait before the next attempt.
#[must_use]
pub fn backoff(attempt: u32, policy: &RetryPolicy) -> Duration {
    let cap = backoff_ceiling(attempt, policy).as_millis();
    let cap = u64::try_from(cap).unwrap_or(u64::MAX);
    if cap == 0 {
        return Duration::ZERO;
    }
    let ms = match policy.jitter {
        Jitter::Full => rand::random_range(0..=cap),
        Jitter::Equal => cap / 2 + rand::random_range(0..=cap / 2),
        Jitter::None => cap,
    };
    Duration::from_millis(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_ms: 100,
            max_ms: 250,
            jitter: Jitter::None,
            ..RetryPolicy::default()
        }
    }

    const fn status(status: u16, retry_after: Option<Duration>, replayable: bool) -> Outcome {
        Outcome::Status {
            status,
            retry_after,
            replayable,
        }
    }

    #[test]
    fn decision_table() {
        let p = policy();
        let ra = Some(Duration::from_secs(7));
        let cases: Vec<(u32, Outcome, Decision)> = vec![
            // 429: Retry-After wins, else backoff; keyless POST still retried.
            (
                1,
                status(429, ra, true),
                Decision::Retry(Duration::from_secs(7)),
            ),
            (
                1,
                status(429, None, true),
                Decision::Retry(Duration::from_millis(100)),
            ),
            (
                2,
                status(429, None, false),
                Decision::Retry(Duration::from_millis(200)),
            ),
            // 5xx retried only when replayable; Retry-After honoured there too.
            (
                1,
                status(503, None, true),
                Decision::Retry(Duration::from_millis(100)),
            ),
            (
                1,
                status(503, ra, true),
                Decision::Retry(Duration::from_secs(7)),
            ),
            (1, status(500, None, false), Decision::Fail),
            // Non-retryable statuses fail at once.
            (1, status(400, None, true), Decision::Fail),
            (1, status(404, None, true), Decision::Fail),
            (1, status(401, None, true), Decision::Fail),
            // Transport: unsent is always safe; sent needs replayability.
            (
                1,
                Outcome::Transport {
                    sent: false,
                    replayable: false,
                },
                Decision::Retry(Duration::from_millis(100)),
            ),
            (
                1,
                Outcome::Transport {
                    sent: true,
                    replayable: true,
                },
                Decision::Retry(Duration::from_millis(100)),
            ),
            (
                1,
                Outcome::Transport {
                    sent: true,
                    replayable: false,
                },
                Decision::Fail,
            ),
            // Budget exhausted.
            (3, status(429, ra, true), Decision::Fail),
            (3, status(503, None, true), Decision::Fail),
        ];
        for (attempt, outcome, expected) in cases {
            assert_eq!(
                decide(attempt, &outcome, &p),
                expected,
                "attempt {attempt} {outcome:?}"
            );
        }
    }

    #[test]
    fn fail_strategy_and_ignored_retry_after() {
        let p = RetryPolicy {
            fail_fast_on_429: true,
            ..policy()
        };
        assert_eq!(
            decide(1, &status(429, Some(Duration::ZERO), true), &p),
            Decision::Fail
        );
        assert_eq!(
            decide(1, &status(503, None, true), &p),
            Decision::Retry(Duration::from_millis(100))
        );
        let p = RetryPolicy {
            respect_retry_after: false,
            ..policy()
        };
        assert_eq!(
            decide(1, &status(429, Some(Duration::from_secs(99)), true), &p),
            Decision::Retry(Duration::from_millis(100))
        );
    }

    #[test]
    fn backoff_is_exponential_capped_and_within_bounds() {
        let p = RetryPolicy {
            max_attempts: 10,
            base_ms: 100,
            max_ms: 350,
            jitter: Jitter::Full,
            ..RetryPolicy::default()
        };
        assert_eq!(backoff_ceiling(1, &p), Duration::from_millis(100));
        assert_eq!(backoff_ceiling(2, &p), Duration::from_millis(200));
        assert_eq!(backoff_ceiling(3, &p), Duration::from_millis(350));
        assert_eq!(
            backoff_ceiling(40, &p),
            Duration::from_millis(350),
            "no overflow"
        );
        for attempt in 1..8 {
            for _ in 0..50 {
                assert!(backoff(attempt, &p) <= backoff_ceiling(attempt, &p));
            }
        }
        let equal = RetryPolicy {
            jitter: Jitter::Equal,
            ..p.clone()
        };
        for _ in 0..50 {
            let d = backoff(2, &equal);
            assert!(
                d >= Duration::from_millis(100) && d <= Duration::from_millis(200),
                "{d:?}"
            );
        }
        let zero = RetryPolicy::instant(3);
        assert_eq!(backoff(5, &zero), Duration::ZERO);
        assert_eq!(zero.max_attempts, 3);
    }

    #[test]
    fn from_settings_maps_strategy_and_respect_retry_after() {
        let retry = RetrySettings {
            max_attempts: 0,
            base_ms: 5,
            max_ms: 6,
            jitter: Jitter::Equal,
            retry_on: vec![503],
        };
        let rate = RateLimitSettings {
            strategy: RateLimitStrategy::Fail,
            max_concurrency: 1,
            reserve_percent: 0,
            warn_threshold: 0,
            respect_retry_after: false,
            high_volume_addon: false,
        };
        let p = RetryPolicy::from_settings(&retry, &rate);
        assert_eq!(p.max_attempts, 1, "at least one attempt");
        assert!(p.fail_fast_on_429);
        assert!(!p.respect_retry_after);
        assert_eq!(p.retry_on, vec![503]);
    }
}
