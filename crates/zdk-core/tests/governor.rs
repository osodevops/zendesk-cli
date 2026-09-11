//! The rate governor: learning limits from headers, `fail` without sleeping, keyed
//! per-ticket buckets, and per-profile persistence.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zdk_core::ZdkError;
use zdk_core::api::Method;
use zdk_core::auth::StaticBearer;
use zdk_core::config::{RateLimitSettings, RateLimitStrategy};
use zdk_core::http::rate_limit::{Family, RateHeaders, state};
use zdk_core::http::{RateGovernor, RetryPolicy, ZendeskClient};

fn settings(strategy: RateLimitStrategy) -> RateLimitSettings {
    RateLimitSettings {
        strategy,
        max_concurrency: 4,
        reserve_percent: 0,
        warn_threshold: 0,
        respect_retry_after: true,
        high_volume_addon: false,
    }
}

#[tokio::test]
async fn learns_x_rate_limit_from_a_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"user": {"id": 1}}))
                .insert_header("x-rate-limit", "3")
                .insert_header("x-rate-limit-remaining", "2"),
        )
        .expect(4)
        .mount(&server)
        .await;
    let governor = Arc::new(RateGovernor::for_tests(RateLimitStrategy::Fail, 1000));
    assert!(!governor.snapshot().learned_from_headers);
    let client = ZendeskClient::builder(Url::parse(&server.uri()).expect("url"))
        .auth(Arc::new(StaticBearer::new("t", "p")))
        .governor(governor.clone())
        .retry(RetryPolicy::instant(1))
        .build()
        .expect("client");

    client.get("/api/v2/users/me").await.expect("first call");
    assert_eq!(governor.account_limit(Family::Support), 3);
    assert!(governor.snapshot().learned_from_headers);
    // The new 3/min bucket starts full: three more calls pass on the fake clock, the fourth fails.
    for _ in 0..3 {
        client
            .get("/api/v2/users/me")
            .await
            .expect("within the learned budget");
    }
    let err = client.get("/api/v2/users/me").await.unwrap_err();
    assert_eq!(err.exit_code(), 7, "{err}");
    server.verify().await;
}

#[tokio::test]
async fn fail_strategy_returns_rate_limited_without_sleeping() {
    let g = RateGovernor::for_tests(RateLimitStrategy::Fail, 2);
    g.acquire(Method::Get, "/api/v2/tickets", None)
        .await
        .expect("1");
    g.acquire(Method::Get, "/api/v2/tickets", None)
        .await
        .expect("2");
    let started = Instant::now();
    let err = g
        .acquire(Method::Get, "/api/v2/tickets", None)
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(err.exit_code(), 7);
    match err {
        ZdkError::RateLimited {
            budget,
            retry_after,
            ..
        } => {
            assert_eq!(budget.name, "account (Support)");
            assert_eq!(budget.limit, Some(2));
            assert!(retry_after.is_some_and(|d| d > Duration::ZERO));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
    // Advancing the fake clock refills the bucket.
    g.clock().advance(Duration::from_secs(60));
    g.acquire(Method::Get, "/api/v2/tickets", None)
        .await
        .expect("refilled");
}

#[tokio::test]
async fn keyed_per_ticket_bucket_separates_two_ticket_ids() {
    let g = RateGovernor::for_tests(RateLimitStrategy::Fail, 100_000);
    for i in 0..30 {
        g.acquire(Method::Put, "/api/v2/tickets/1.json", None)
            .await
            .unwrap_or_else(|e| panic!("update {i} of ticket 1: {e}"));
    }
    let err = g
        .acquire(Method::Put, "/api/v2/tickets/1", None)
        .await
        .unwrap_err();
    match err {
        ZdkError::RateLimited { budget, .. } => {
            assert!(budget.name.contains("per ticket"), "{}", budget.name);
            assert!(budget.name.contains("[1]"), "{}", budget.name);
            assert_eq!(budget.limit, Some(30));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
    g.acquire(Method::Put, "/api/v2/tickets/2", None)
        .await
        .expect("ticket 2 has its own bucket");
    g.acquire(Method::Get, "/api/v2/tickets/1", None)
        .await
        .expect("reads are not throttled by the update rule");
}

#[tokio::test]
async fn burst_strategy_only_keeps_the_concurrency_ceiling() {
    let g = RateGovernor::for_tests(RateLimitStrategy::Burst, 1);
    for _ in 0..20 {
        g.acquire(Method::Get, "/api/v2/incremental/tickets/cursor", None)
            .await
            .expect("burst never blocks locally");
    }
}

#[tokio::test]
async fn learned_limits_persist_per_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    let g = RateGovernor::new(&settings(RateLimitStrategy::Wait)).with_state(dir.path(), "prod");
    assert_eq!(g.account_limit(Family::Support), 200);
    g.observe(
        &RateHeaders {
            limit: Some(700),
            remaining: Some(650),
            ..Default::default()
        },
        Family::Support,
    );
    g.observe(
        &RateHeaders {
            limit: Some(400),
            ..Default::default()
        },
        Family::HelpCenter,
    );
    let saved = state::load(dir.path(), "prod").expect("state file written");
    assert_eq!(saved.support_limit, Some(700));
    assert_eq!(saved.help_center_limit, Some(400));

    let again =
        RateGovernor::new(&settings(RateLimitStrategy::Wait)).with_state(dir.path(), "prod");
    assert_eq!(again.account_limit(Family::Support), 700);
    assert_eq!(again.account_limit(Family::HelpCenter), 400);
    let other =
        RateGovernor::new(&settings(RateLimitStrategy::Wait)).with_state(dir.path(), "sandbox");
    assert_eq!(
        other.account_limit(Family::Support),
        200,
        "state is per profile"
    );
}
