//! The rate governor (PRD §10): a GCRA bucket per family (Support / Help Center) that learns
//! the account limit from response headers, the static per-endpoint buckets of
//! [`endpoint_rules`] (keyed where Zendesk keys them), reserve headroom, sub-budget holds and
//! the in-flight concurrency ceiling.
//!
//! Strategies: `wait` sleeps until a bucket refills, `fail` returns [`ZdkError::RateLimited`]
//! without sleeping, `burst` skips every local bucket (only the concurrency ceiling applies)
//! and relies on the 429 handling in the client.

pub mod endpoint_rules;
pub mod headers;
pub mod state;

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use governor::clock::{Clock, FakeRelativeClock, Reference};
use governor::middleware::NoOpMiddleware;
use governor::nanos::Nanos;
use governor::state::InMemoryState;
use governor::state::direct::NotKeyed;
use governor::state::keyed::DashMapStateStore;
use governor::{Quota, RateLimiter};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub use endpoint_rules::{RULES, RateRule, RuleKey, match_rules};
pub use headers::{RateHeaders, SubBudget};

use crate::api::{Method, template};
use crate::config::{RateLimitSettings, RateLimitStrategy};
use crate::error::RateBudget;
use crate::{Result, ZdkError};

/// Account limit assumed until the first response teaches us better (the Team plan floor).
pub const DEFAULT_ACCOUNT_LIMIT: u32 = 200;
/// How long a reserve hold lasts when the response carried no `ratelimit-reset`.
const DEFAULT_HOLD: Duration = Duration::from_secs(60);

/// Which account budget a path draws from — Help Center requests do not count against the
/// Support budget and vice versa (PRD §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Support,
    HelpCenter,
}

impl Family {
    #[must_use]
    pub fn for_path(path: &str) -> Self {
        let p = template::normalize(path);
        if p.starts_with("/api/v2/help_center")
            || p.starts_with("/api/v2/guide")
            || p.starts_with("/api/v2/community")
        {
            Self::HelpCenter
        } else {
            Self::Support
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Support => "account (Support)",
            Self::HelpCenter => "account (Help Center)",
        }
    }
}

/// The governor's time source: real monotonic time in production, a fake clock in tests.
/// Both measure in [`Nanos`] so one limiter type serves both.
#[derive(Debug, Clone)]
pub enum GovClock {
    Real(Instant),
    Fake(FakeRelativeClock),
}

impl GovClock {
    #[must_use]
    pub fn real() -> Self {
        Self::Real(Instant::now())
    }

    #[must_use]
    pub fn fake() -> Self {
        Self::Fake(FakeRelativeClock::default())
    }

    /// Advance a fake clock (no-op on the real one).
    pub fn advance(&self, by: Duration) {
        if let Self::Fake(f) = self {
            f.advance(by);
        }
    }
}

impl Clock for GovClock {
    type Instant = Nanos;

    fn now(&self) -> Nanos {
        match self {
            Self::Real(start) => start.elapsed().into(),
            Self::Fake(f) => f.now(),
        }
    }
}

/// An un-keyed GCRA bucket on [`GovClock`].
pub type DirectLimiter = RateLimiter<NotKeyed, InMemoryState, GovClock, NoOpMiddleware<Nanos>>;
/// A per-key GCRA bucket on [`GovClock`].
pub type KeyedLimiter =
    RateLimiter<String, DashMapStateStore<String>, GovClock, NoOpMiddleware<Nanos>>;

enum RuleLimiter {
    Direct(DirectLimiter),
    Keyed(KeyedLimiter),
}

/// Held for the duration of one request; releases the concurrency slot on drop.
#[derive(Debug)]
pub struct Permit {
    _concurrency: Option<OwnedSemaphorePermit>,
}

/// What a request may wait on beyond the family bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum HoldKey {
    Family(Family),
    TicketsIndex,
}

#[derive(Debug, Clone, Copy)]
struct FamilyLimits {
    support: u32,
    help_center: u32,
}

/// A read-only view for `zdk doctor` / `zdk rate-limit status`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GovernorSnapshot {
    pub strategy: RateLimitStrategy,
    pub support_limit: u32,
    pub help_center_limit: u32,
    pub reserve_percent: u8,
    pub high_volume: bool,
    pub max_concurrency: usize,
    pub learned_from_headers: bool,
}

/// See the module docs.
pub struct RateGovernor {
    strategy: RateLimitStrategy,
    reserve_percent: u8,
    warn_threshold: u8,
    high_volume: bool,
    clock: GovClock,
    support: ArcSwap<DirectLimiter>,
    help_center: ArcSwap<DirectLimiter>,
    limits: Mutex<FamilyLimits>,
    rules: HashMap<&'static str, RuleLimiter>,
    holds: Mutex<HashMap<HoldKey, Nanos>>,
    concurrency: Arc<Semaphore>,
    max_concurrency: usize,
    persist: Option<(PathBuf, String)>,
    learned: AtomicBool,
    warned: AtomicBool,
}

impl fmt::Debug for RateGovernor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateGovernor")
            .field("strategy", &self.strategy)
            .field("reserve_percent", &self.reserve_percent)
            .field("high_volume", &self.high_volume)
            .field("limits", &self.limits.lock().map(|l| *l).ok())
            .field("rules", &self.rules.len())
            .field("max_concurrency", &self.max_concurrency)
            .finish_non_exhaustive()
    }
}

fn quota(limit: u32, per: Duration) -> Quota {
    let limit = NonZeroU32::new(limit.max(1)).unwrap_or(NonZeroU32::MIN);
    let period = per.max(Duration::from_millis(1));
    Quota::with_period(period / limit.get())
        .unwrap_or_else(|| Quota::per_minute(limit))
        .allow_burst(limit)
}

impl RateGovernor {
    /// A governor for the resolved settings, on the real clock, with nothing learned yet.
    #[must_use]
    pub fn new(settings: &RateLimitSettings) -> Self {
        Self::with_clock(settings, GovClock::real())
    }

    /// [`new`](Self::new) with an explicit clock.
    #[must_use]
    pub fn with_clock(settings: &RateLimitSettings, clock: GovClock) -> Self {
        let make = |limit: u32| {
            Arc::new(DirectLimiter::direct_with_clock(
                quota(limit, Duration::from_secs(60)),
                clock.clone(),
            ))
        };
        let mut rules = HashMap::new();
        for rule in endpoint_rules::all_rules() {
            let q = quota(rule.effective_limit(settings.high_volume_addon), rule.per);
            let limiter = match rule.key {
                RuleKey::Account => {
                    RuleLimiter::Direct(DirectLimiter::direct_with_clock(q, clock.clone()))
                }
                RuleKey::PathParam(_) | RuleKey::BodyField(_) => {
                    RuleLimiter::Keyed(KeyedLimiter::dashmap_with_clock(q, clock.clone()))
                }
            };
            rules.insert(rule.id, limiter);
        }
        let max_concurrency = usize::try_from(settings.max_concurrency.max(1)).unwrap_or(1);
        Self {
            strategy: settings.strategy,
            reserve_percent: settings.reserve_percent.min(90),
            warn_threshold: settings.warn_threshold.min(100),
            high_volume: settings.high_volume_addon,
            support: ArcSwap::new(make(DEFAULT_ACCOUNT_LIMIT)),
            help_center: ArcSwap::new(make(DEFAULT_ACCOUNT_LIMIT)),
            limits: Mutex::new(FamilyLimits {
                support: DEFAULT_ACCOUNT_LIMIT,
                help_center: DEFAULT_ACCOUNT_LIMIT,
            }),
            rules,
            holds: Mutex::new(HashMap::new()),
            concurrency: Arc::new(Semaphore::new(max_concurrency)),
            max_concurrency,
            persist: None,
            clock,
            learned: AtomicBool::new(false),
            warned: AtomicBool::new(false),
        }
    }

    /// Remember learned limits under `state_dir` for `profile`, and start from what a previous
    /// run learned.
    #[must_use]
    pub fn with_state(mut self, state_dir: &Path, profile: &str) -> Self {
        if let Some(saved) = state::load(state_dir, profile) {
            if let Some(l) = saved.support_limit.filter(|l| *l > 0) {
                self.swap_family(Family::Support, l);
            }
            if let Some(l) = saved.help_center_limit.filter(|l| *l > 0) {
                self.swap_family(Family::HelpCenter, l);
            }
        }
        self.persist = Some((state_dir.to_path_buf(), profile.to_string()));
        self
    }

    /// A governor that never waits or fails locally (`burst`): the choice for wiremock tests
    /// that are about the client, not the limiter.
    #[must_use]
    pub fn unlimited() -> Self {
        Self::new(&RateLimitSettings {
            strategy: RateLimitStrategy::Burst,
            max_concurrency: 64,
            reserve_percent: 0,
            warn_threshold: 0,
            respect_retry_after: true,
            high_volume_addon: false,
        })
    }

    /// A governor on a fake clock with a tiny account bucket, for tests of the limiter itself.
    /// The rule buckets keep their documented limits; the fake clock never advances on its
    /// own, so use [`RateLimitStrategy::Fail`] or [`GovClock::advance`] via [`clock`](Self::clock).
    #[must_use]
    pub fn for_tests(strategy: RateLimitStrategy, account_per_minute: u32) -> Self {
        let g = Self::with_clock(
            &RateLimitSettings {
                strategy,
                max_concurrency: 8,
                reserve_percent: 0,
                warn_threshold: 0,
                respect_retry_after: true,
                high_volume_addon: false,
            },
            GovClock::fake(),
        );
        g.swap_family(Family::Support, account_per_minute);
        g.swap_family(Family::HelpCenter, account_per_minute);
        g.learned.store(false, Ordering::Relaxed);
        g
    }

    /// The clock in use (tests advance the fake one).
    #[must_use]
    pub fn clock(&self) -> &GovClock {
        &self.clock
    }

    #[must_use]
    pub fn strategy(&self) -> RateLimitStrategy {
        self.strategy
    }

    /// The current account limit for a family (learned, restored or the default).
    #[must_use]
    pub fn account_limit(&self, family: Family) -> u32 {
        let l = self.limits.lock().map_or(
            FamilyLimits {
                support: DEFAULT_ACCOUNT_LIMIT,
                help_center: DEFAULT_ACCOUNT_LIMIT,
            },
            |l| *l,
        );
        match family {
            Family::Support => l.support,
            Family::HelpCenter => l.help_center,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> GovernorSnapshot {
        GovernorSnapshot {
            strategy: self.strategy,
            support_limit: self.account_limit(Family::Support),
            help_center_limit: self.account_limit(Family::HelpCenter),
            reserve_percent: self.reserve_percent,
            high_volume: self.high_volume,
            max_concurrency: self.max_concurrency,
            learned_from_headers: self.learned.load(Ordering::Relaxed),
        }
    }

    /// Wait for (or, under `fail`, check) every budget a request draws on, then take a
    /// concurrency slot. Hold the returned [`Permit`] until the response arrives.
    pub async fn acquire(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Permit> {
        self.acquire_with(method, path, body, &[]).await
    }

    /// [`acquire`](Self::acquire) plus explicitly requested rule ids (e.g. `tickets_index_deep`).
    pub async fn acquire_with(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        extra_rules: &[&str],
    ) -> Result<Permit> {
        let permit = Arc::clone(&self.concurrency)
            .acquire_owned()
            .await
            .map_err(|_| ZdkError::Other("rate governor is closed".into()))?;
        let permit = Permit {
            _concurrency: Some(permit),
        };
        if self.strategy == RateLimitStrategy::Burst {
            return Ok(permit);
        }

        let family = Family::for_path(path);
        self.wait_hold(HoldKey::Family(family), family.name())
            .await?;
        if template::normalize(path) == "/api/v2/tickets" {
            self.wait_hold(HoldKey::TicketsIndex, "ticket index")
                .await?;
        }
        self.wait_family(family).await?;

        for (rule, key) in endpoint_rules::match_rules_with_body(method, path, body) {
            self.wait_rule(rule, key).await?;
        }
        for id in extra_rules {
            if let Some(rule) = endpoint_rules::rule(id) {
                self.wait_rule(rule, None).await?;
            }
        }
        Ok(permit)
    }

    /// Feed the headers of a response back: learn the account limit, apply reserve headroom
    /// and sub-budget holds, warn once when the budget runs low.
    pub fn observe(&self, headers: &RateHeaders, family: Family) {
        if let Some(limit) = headers.limit.filter(|l| *l > 0) {
            if self.account_limit(family) != limit {
                tracing::info!(
                    target: "zdk::http",
                    family = family.name(),
                    limit,
                    "learned account rate limit from response headers"
                );
                self.swap_family(family, limit);
                self.persist_state();
            }
            self.learned.store(true, Ordering::Relaxed);
        }
        if self.strategy == RateLimitStrategy::Burst {
            return;
        }
        let limit = self.account_limit(family);
        if let Some(remaining) = headers.remaining {
            let reserve = u64::from(limit) * u64::from(self.reserve_percent) / 100;
            if u64::from(remaining) <= reserve {
                let hold = headers.reset.unwrap_or(DEFAULT_HOLD);
                tracing::info!(
                    target: "zdk::http",
                    remaining,
                    limit,
                    hold_secs = hold.as_secs(),
                    "inside the reserve headroom; holding the {} budget",
                    family.name()
                );
                self.set_hold(HoldKey::Family(family), hold);
            }
            if self.warn_threshold > 0
                && u64::from(remaining) * 100 < u64::from(limit) * u64::from(self.warn_threshold)
                && !self.warned.swap(true, Ordering::Relaxed)
            {
                crate::output::warn(&format!(
                    "warning: {} rate-limit budget is low ({remaining} of {limit} requests left this minute)",
                    family.name()
                ));
            }
        }
        if let Some(t) = headers.tickets_index.filter(SubBudget::exhausted) {
            self.set_hold(HoldKey::TicketsIndex, t.resets.unwrap_or(DEFAULT_HOLD));
        }
    }

    fn family_limiter(&self, family: Family) -> &ArcSwap<DirectLimiter> {
        match family {
            Family::Support => &self.support,
            Family::HelpCenter => &self.help_center,
        }
    }

    fn swap_family(&self, family: Family, limit: u32) {
        let limiter = DirectLimiter::direct_with_clock(
            quota(limit, Duration::from_secs(60)),
            self.clock.clone(),
        );
        self.family_limiter(family).store(Arc::new(limiter));
        if let Ok(mut l) = self.limits.lock() {
            match family {
                Family::Support => l.support = limit,
                Family::HelpCenter => l.help_center = limit,
            }
        }
    }

    fn persist_state(&self) {
        let Some((dir, profile)) = &self.persist else {
            return;
        };
        let st = state::RateState {
            support_limit: Some(self.account_limit(Family::Support)),
            help_center_limit: Some(self.account_limit(Family::HelpCenter)),
            updated_at: None,
        };
        if let Err(e) = state::save(dir, profile, &st) {
            tracing::debug!(error = %e, "could not persist rate-limit state");
        }
    }

    fn set_hold(&self, key: HoldKey, for_: Duration) {
        let until = self.clock.now() + Nanos::from(for_);
        if let Ok(mut h) = self.holds.lock() {
            let entry = h.entry(key).or_insert(until);
            if until > *entry {
                *entry = until;
            }
        }
    }

    fn hold_remaining(&self, key: HoldKey) -> Option<Duration> {
        let mut holds = self.holds.lock().ok()?;
        let until = *holds.get(&key)?;
        let now = self.clock.now();
        if until <= now {
            holds.remove(&key);
            return None;
        }
        Some(until.duration_since(now).into())
    }

    async fn wait_hold(&self, key: HoldKey, budget: &str) -> Result<()> {
        if let Some(wait) = self.hold_remaining(key) {
            self.pause(wait, budget, None, None).await?;
        }
        Ok(())
    }

    async fn wait_family(&self, family: Family) -> Result<()> {
        loop {
            let limiter = self.family_limiter(family).load();
            match limiter.check() {
                Ok(()) => return Ok(()),
                Err(not_until) => {
                    let wait = not_until.wait_time_from(self.clock.now());
                    drop(limiter);
                    self.pause(
                        wait,
                        family.name(),
                        Some(self.account_limit(family)),
                        Some(0),
                    )
                    .await?;
                }
            }
        }
    }

    async fn wait_rule(&self, rule: &'static RateRule, key: Option<String>) -> Result<()> {
        let Some(limiter) = self.rules.get(rule.id) else {
            return Ok(());
        };
        let limit = rule.effective_limit(self.high_volume);
        loop {
            let outcome = match (limiter, &key) {
                (RuleLimiter::Direct(l), _) => l.check(),
                (RuleLimiter::Keyed(l), Some(k)) => l.check_key(k),
                // A keyed rule without a key (body field absent): cannot bucket it, let it through.
                (RuleLimiter::Keyed(_), None) => return Ok(()),
            };
            match outcome {
                Ok(()) => return Ok(()),
                Err(not_until) => {
                    let wait = not_until.wait_time_from(self.clock.now());
                    let name = match &key {
                        Some(k) => format!("{} [{k}]", rule.name),
                        None => rule.name.to_string(),
                    };
                    self.pause(wait, &name, Some(limit), Some(0)).await?;
                }
            }
        }
    }

    /// Sleep under `wait`; error under `fail`.
    async fn pause(
        &self,
        wait: Duration,
        budget: &str,
        limit: Option<u32>,
        remaining: Option<u32>,
    ) -> Result<()> {
        match self.strategy {
            RateLimitStrategy::Fail => Err(ZdkError::RateLimited {
                budget: RateBudget {
                    name: budget.to_string(),
                    limit,
                    remaining,
                },
                retry_after: Some(wait),
                request_id: None,
            }),
            RateLimitStrategy::Wait | RateLimitStrategy::Burst => {
                if wait >= Duration::from_secs(2) {
                    crate::output::warn(&format!(
                        "rate limit: waiting {:.0}s for the {budget} budget",
                        wait.as_secs_f64()
                    ));
                } else {
                    tracing::debug!(
                        target: "zdk::http",
                        budget,
                        wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                        "rate limit pause"
                    );
                }
                tokio::time::sleep(wait).await;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_is_decided_by_path_prefix() {
        assert_eq!(Family::for_path("/api/v2/tickets"), Family::Support);
        assert_eq!(
            Family::for_path("/api/v2/help_center/en-us/articles"),
            Family::HelpCenter
        );
        assert_eq!(Family::for_path("api/v2/guide/x"), Family::HelpCenter);
        assert_eq!(
            Family::for_path("/api/v2/community/posts"),
            Family::HelpCenter
        );
        assert_eq!(Family::for_path("/api/v2/help_centerx"), Family::HelpCenter);
    }

    #[test]
    fn quota_math_never_panics() {
        let q = quota(0, Duration::ZERO);
        assert_eq!(q.burst_size().get(), 1);
        let q = quota(100, Duration::from_secs(60));
        assert_eq!(q.burst_size().get(), 100);
        assert_eq!(q.replenish_interval(), Duration::from_millis(600));
    }

    #[tokio::test]
    async fn fake_clock_governor_fails_fast_then_recovers_after_advance() {
        let g = RateGovernor::for_tests(RateLimitStrategy::Fail, 2);
        assert!(
            g.acquire(Method::Get, "/api/v2/tickets", None)
                .await
                .is_ok()
        );
        assert!(g.acquire(Method::Get, "/api/v2/users", None).await.is_ok());
        let err = g
            .acquire(Method::Get, "/api/v2/users", None)
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), 7);
        assert!(
            g.acquire(Method::Get, "/api/v2/help_center/en-us/articles", None)
                .await
                .is_ok(),
            "separate family budget"
        );
        g.clock().advance(Duration::from_secs(61));
        assert!(
            g.acquire(Method::Get, "/api/v2/tickets", None)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn burst_skips_local_buckets() {
        let g = RateGovernor::for_tests(RateLimitStrategy::Burst, 1);
        for _ in 0..5 {
            assert!(
                g.acquire(Method::Put, "/api/v2/tickets/1", None)
                    .await
                    .is_ok()
            );
        }
    }

    #[tokio::test]
    async fn reserve_headroom_holds_the_family_under_fail() {
        let settings = RateLimitSettings {
            strategy: RateLimitStrategy::Fail,
            max_concurrency: 2,
            reserve_percent: 10,
            warn_threshold: 0,
            respect_retry_after: true,
            high_volume_addon: false,
        };
        let g = RateGovernor::with_clock(&settings, GovClock::fake());
        let h = RateHeaders {
            limit: Some(100),
            remaining: Some(5),
            reset: Some(Duration::from_secs(30)),
            ..Default::default()
        };
        g.observe(&h, Family::Support);
        assert_eq!(g.account_limit(Family::Support), 100);
        assert!(g.snapshot().learned_from_headers);
        let err = g
            .acquire(Method::Get, "/api/v2/tickets", None)
            .await
            .unwrap_err();
        assert!(matches!(err, ZdkError::RateLimited { .. }), "{err}");
        assert!(
            g.acquire(Method::Get, "/api/v2/help_center/x", None)
                .await
                .is_ok()
        );
        g.clock().advance(Duration::from_secs(31));
        assert!(
            g.acquire(Method::Get, "/api/v2/tickets", None)
                .await
                .is_ok()
        );
    }
}
