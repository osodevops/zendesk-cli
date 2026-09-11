//! The HTTP pipeline against wiremock: auth invalidate-and-retry, 429/5xx backoff, stable
//! idempotency keys, error mapping, dry-run, scope pre-flight, observers and cancellation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use http::HeaderValue;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use zdk_core::auth::{AuthDescription, AuthProvider, GrantKind, StaticBearer};
use zdk_core::error::AuthFailure;
use zdk_core::http::{MemoryObserver, RateGovernor, RequestSpec, RetryPolicy, ZendeskClient};
use zdk_core::output::OutputFormat;
use zdk_core::{Result, ZdkError};

fn base(server: &MockServer) -> Url {
    Url::parse(&server.uri()).expect("mock url")
}

fn bearer() -> Arc<dyn AuthProvider> {
    Arc::new(StaticBearer::new("test-token", "p"))
}

fn client(server: &MockServer, auth: Arc<dyn AuthProvider>, attempts: u32) -> ZendeskClient {
    ZendeskClient::builder(base(server))
        .auth(auth)
        .governor(Arc::new(RateGovernor::unlimited()))
        .retry(RetryPolicy::instant(attempts))
        .profile("p")
        .command("api GET /x")
        .build()
        .expect("client")
}

/// `Bearer old` until `invalidate()` is called, then `Bearer new`.
#[derive(Debug)]
struct FlipProvider {
    invalidated: AtomicBool,
    scopes: Option<Vec<String>>,
}

impl FlipProvider {
    fn new(scopes: Option<Vec<&str>>) -> Arc<Self> {
        Arc::new(Self {
            invalidated: AtomicBool::new(false),
            scopes: scopes.map(|s| s.into_iter().map(str::to_string).collect()),
        })
    }
}

#[async_trait]
impl AuthProvider for FlipProvider {
    async fn authorization(&self) -> Result<HeaderValue> {
        Ok(HeaderValue::from_static(
            if self.invalidated.load(Ordering::SeqCst) {
                "Bearer new"
            } else {
                "Bearer old"
            },
        ))
    }

    async fn invalidate(&self) -> Result<bool> {
        self.invalidated.store(true, Ordering::SeqCst);
        Ok(true)
    }

    fn granted_scopes(&self) -> Option<Vec<String>> {
        self.scopes.clone()
    }

    fn description(&self) -> AuthDescription {
        AuthDescription {
            grant: GrantKind::AuthorizationCode,
            profile: "p".into(),
            subdomain: None,
            client_id: None,
            scopes: self.scopes.clone().unwrap_or_default(),
            expires_at: None,
            has_refresh_token: true,
            store: None,
            api_token_days_remaining: None,
        }
    }
}

/// Answers with the templates in order, repeating the last one.
struct Sequence {
    responses: Vec<ResponseTemplate>,
    calls: AtomicUsize,
}

impl Sequence {
    fn new(responses: Vec<ResponseTemplate>) -> Self {
        Self {
            responses,
            calls: AtomicUsize::new(0),
        }
    }
}

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses[i.min(self.responses.len() - 1)].clone()
    }
}

fn ok_json(body: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

#[derive(Debug, serde::Deserialize)]
struct Me {
    user: serde_json::Value,
}

#[tokio::test]
async fn get_returns_typed_json_request_id_and_rate_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .and(header("authorization", "Bearer test-token"))
        .and(header("accept", "application/json"))
        .respond_with(
            ok_json(json!({"user": {"id": 1, "name": "Ada"}}))
                .insert_header("x-request-id", "rid-1")
                .insert_header("x-rate-limit", "700")
                .insert_header("x-rate-limit-remaining", "699"),
        )
        .expect(2)
        .mount(&server)
        .await;

    let c = client(&server, bearer(), 3);
    let resp = c.get("/api/v2/users/me").await.expect("200");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.attempts, 1);
    assert_eq!(resp.request_id.as_deref(), Some("rid-1"));
    assert_eq!(resp.rate.limit, Some(700));
    assert_eq!(resp.rate.remaining, Some(699));
    assert_eq!(resp.value().expect("json")["user"]["name"], "Ada");

    let me: Me = c.get_json("/api/v2/users/me", &[]).await.expect("typed");
    assert_eq!(me.user["id"], 1);
    assert_eq!(
        c.governor().account_limit(zdk_core::http::Family::Support),
        700,
        "learned from headers"
    );
}

#[tokio::test]
async fn unauthorized_invalidates_the_credential_and_retries_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .and(header("authorization", "Bearer old"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": "Couldn't authenticate you"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .and(header("authorization", "Bearer new"))
        .respond_with(ok_json(json!({"user": {"id": 2}})))
        .expect(1)
        .mount(&server)
        .await;

    let provider = FlipProvider::new(None);
    let c = client(&server, provider.clone(), 3);
    let resp = c
        .get("/api/v2/users/me")
        .await
        .expect("retried with the new token");
    assert_eq!(resp.attempts, 2);
    assert!(provider.invalidated.load(Ordering::SeqCst));
}

#[tokio::test]
async fn unauthorized_static_token_is_an_auth_error_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": "Couldn't authenticate you"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, bearer(), 5)
        .get("/api/v2/users/me")
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 3);
    assert!(
        matches!(err, ZdkError::Auth(AuthFailure::NotLoggedIn { .. })),
        "{err:?}"
    );
}

#[tokio::test]
async fn rate_limited_with_retry_after_zero_then_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .insert_header("x-rate-limit", "200")
                .insert_header("x-rate-limit-remaining", "0"),
            ok_json(json!({"tickets": []})),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    let resp = client(&server, bearer(), 4)
        .get("/api/v2/tickets")
        .await
        .expect("recovered");
    assert_eq!(resp.attempts, 2);
}

#[tokio::test]
async fn rate_limited_without_header_uses_zero_backoff_in_tests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(429),
            ResponseTemplate::new(429),
            ok_json(json!({"tickets": []})),
        ]))
        .expect(3)
        .mount(&server)
        .await;
    let started = Instant::now();
    let resp = client(&server, bearer(), 6)
        .get("/api/v2/tickets")
        .await
        .expect("recovered");
    assert_eq!(resp.attempts, 3);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "base_ms=0 means no real sleeping"
    );
}

#[tokio::test]
async fn rate_limited_under_fail_strategy_is_exit_7_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "30")
                .insert_header("x-rate-limit", "200")
                .insert_header("x-rate-limit-remaining", "0"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = ZendeskClient::builder(base(&server))
        .auth(bearer())
        .governor(Arc::new(RateGovernor::unlimited()))
        .retry(RetryPolicy {
            fail_fast_on_429: true,
            ..RetryPolicy::instant(6)
        })
        .build()
        .expect("client");
    let err = c.get("/api/v2/tickets").await.unwrap_err();
    assert_eq!(err.exit_code(), 7);
    match err {
        ZdkError::RateLimited {
            budget,
            retry_after,
            ..
        } => {
            assert_eq!(budget.limit, Some(200));
            assert_eq!(budget.remaining, Some(0));
            assert_eq!(retry_after, Some(Duration::from_secs(30)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn server_errors_exhaust_attempts_then_exit_9() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_string("upstream unavailable")
                .insert_header("x-request-id", "rid-503"),
        )
        .expect(3)
        .mount(&server)
        .await;
    let err = client(&server, bearer(), 3)
        .get("/api/v2/tickets")
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 9);
    match err {
        ZdkError::Server {
            status,
            attempts,
            request_id,
            body,
        } => {
            assert_eq!(status, 503);
            assert_eq!(attempts, 3);
            assert_eq!(request_id.as_deref(), Some("rid-503"));
            assert!(body.contains("upstream"));
        }
        other => panic!("expected Server, got {other:?}"),
    }
}

#[tokio::test]
async fn dry_run_renders_and_sends_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .respond_with(ok_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    let c = ZendeskClient::builder(base(&server))
        .auth(bearer())
        .dry_run(true, OutputFormat::Ndjson)
        .build()
        .expect("client");
    let spec = RequestSpec::post("/api/v2/tickets").json(json!({"ticket": {"subject": "x"}}));
    let err = c.execute(spec).await.unwrap_err();
    assert!(matches!(err, ZdkError::DryRun));
    assert_eq!(err.exit_code(), 0);
    server.verify().await;
}

#[tokio::test]
async fn post_without_key_is_not_retried_after_500() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    let spec = RequestSpec::post("/api/v2/tickets")
        .json(json!({"ticket": {"subject": "x"}}))
        .without_idempotency_key();
    let err = client(&server, bearer(), 4)
        .execute(spec)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ZdkError::Server { attempts: 1, .. }),
        "{err:?}"
    );
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].headers.get("idempotency-key").is_none());
}

#[tokio::test]
async fn idempotency_key_is_present_and_stable_across_a_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(503),
            ResponseTemplate::new(201).set_body_json(json!({"ticket": {"id": 7}})),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    let c = client(&server, bearer(), 4);
    let body = c
        .send_json(
            zdk_core::api::Method::Post,
            "/api/v2/tickets",
            json!({"ticket": {"subject": "x"}}),
        )
        .await
        .expect("created after retry");
    assert_eq!(body["ticket"]["id"], 7);
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 2);
    let keys: Vec<&str> = requests
        .iter()
        .map(|r| {
            r.headers
                .get("idempotency-key")
                .expect("key")
                .to_str()
                .expect("ascii")
        })
        .collect();
    assert_eq!(keys[0], keys[1], "same key on every attempt");
    assert_eq!(keys[0].len(), 36, "uuid v4");
    assert_eq!(
        requests[0]
            .headers
            .get("content-type")
            .expect("ct")
            .to_str()
            .expect("ascii"),
        "application/json"
    );

    // An explicit key is honoured verbatim.
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/7"))
        .and(header("idempotency-key", "my-key"))
        .respond_with(ok_json(json!({"ticket": {"id": 7}})))
        .expect(1)
        .mount(&server)
        .await;
    c.execute(
        RequestSpec::put("/api/v2/tickets/7")
            .json(json!({}))
            .idempotency_key("my-key"),
    )
    .await
    .expect("explicit key");
}

#[tokio::test]
async fn forbidden_maps_the_required_scope_from_the_registry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"error": {"title": "Forbidden", "message": "You do not have access to this page."}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, bearer(), 3)
        .get("/api/v2/tickets")
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 4);
    assert_eq!(err.error_code(), "SCOPE_MISSING");
    match err {
        ZdkError::Forbidden {
            required_scope,
            message,
            ..
        } => {
            assert_eq!(required_scope.as_deref(), Some("tickets:read"));
            assert!(message.contains("You do not have access"), "{message}");
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
}

#[tokio::test]
async fn not_found_names_the_resource_and_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/999"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": "RecordNotFound", "description": "Not found"}))
                .insert_header("x-request-id", "rid-404"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, bearer(), 3)
        .get("/api/v2/tickets/999")
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 5);
    assert_eq!(err.request_id(), Some("rid-404"));
    match err {
        ZdkError::NotFound { resource, id, .. } => {
            assert_eq!(resource, "ticket");
            assert_eq!(id, "999");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn offset_limit_400_maps_to_pagination_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .and(query_param("page", "101"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "InvalidPaginationParameter",
            "description": "Offset pagination is limited to 10,000 records (page 100); use cursor pagination"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, bearer(), 3)
        .execute(RequestSpec::get("/api/v2/tickets").query("page", 101))
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 12);
    match err {
        ZdkError::PaginationLimit {
            endpoint,
            alternatives,
        } => {
            assert_eq!(endpoint, "/api/v2/tickets");
            assert!(!alternatives.is_empty());
        }
        other => panic!("expected PaginationLimit, got {other:?}"),
    }
}

#[tokio::test]
async fn validation_errors_carry_field_details_and_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "error": "RecordInvalid",
            "description": "Record validation errors",
            "details": {"subject": [{"description": "Subject: cannot be blank"}]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, bearer(), 4)
        .execute(RequestSpec::post("/api/v2/tickets").json(json!({"ticket": {}})))
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 6);
    match err {
        ZdkError::Validation {
            status, details, ..
        } => {
            assert_eq!(status, 422);
            assert_eq!(details.len(), 1);
            assert_eq!(details[0].field, "subject");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn scope_preflight_blocks_before_any_request_unless_disabled() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .respond_with(ok_json(json!({"tickets": []})))
        .expect(1)
        .mount(&server)
        .await;
    let provider = FlipProvider::new(Some(vec!["users:read"]));
    let c = client(&server, provider, 3);
    let err = c.get("/api/v2/tickets").await.unwrap_err();
    match err {
        ZdkError::Forbidden {
            required_scope,
            granted,
            ..
        } => {
            assert_eq!(required_scope.as_deref(), Some("tickets:read"));
            assert_eq!(granted, vec!["users:read"]);
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
    c.execute(RequestSpec::get("/api/v2/tickets").no_preflight(true))
        .await
        .expect("sent without preflight");
}

#[tokio::test]
async fn foreign_hosts_are_refused_before_sending() {
    let server = MockServer::start().await;
    let err = client(&server, bearer(), 3)
        .execute(RequestSpec::get("https://evil.example/api/v2/users/me"))
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("status.zendesk.com"), "{err}");
}

#[tokio::test]
async fn observers_see_every_attempt() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(Sequence::new(vec![
            ResponseTemplate::new(503),
            ok_json(json!({"user": {"id": 1}})).insert_header("x-rate-limit", "700"),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    let observer = Arc::new(MemoryObserver::new());
    let c = ZendeskClient::builder(base(&server))
        .auth(bearer())
        .governor(Arc::new(RateGovernor::unlimited()))
        .retry(RetryPolicy::instant(3))
        .observer(observer.clone())
        .profile("prod")
        .command("auth whoami")
        .build()
        .expect("client");
    c.execute(RequestSpec::get("/api/v2/users/me").query("include", "roles"))
        .await
        .expect("ok");
    let records = observer.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].status, Some(503));
    assert_eq!(records[0].attempt, 1);
    assert_eq!(records[1].status, Some(200));
    assert_eq!(records[1].attempt, 2);
    assert_eq!(records[1].rate.limit, Some(700));
    assert_eq!(records[1].profile, "prod");
    assert_eq!(records[1].command, "auth whoami");
    assert_eq!(records[1].method, "GET");
    assert_eq!(records[1].path, "/api/v2/users/me?include=roles");
    assert!(records[1].bytes > 0);
}

#[tokio::test]
async fn cancelled_token_interrupts_before_sending() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(ok_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let c = ZendeskClient::builder(base(&server))
        .auth(bearer())
        .cancel(cancel)
        .build()
        .expect("client");
    let err = c.get("/api/v2/users/me").await.unwrap_err();
    assert!(matches!(err, ZdkError::Interrupted));
    assert_eq!(err.exit_code(), 130);
}

#[tokio::test]
async fn connection_refused_is_retried_then_a_network_error() {
    let c = ZendeskClient::builder(Url::parse("http://127.0.0.1:1/").expect("url"))
        .auth(bearer())
        .governor(Arc::new(RateGovernor::unlimited()))
        .retry(RetryPolicy::instant(2))
        .timeout(Duration::from_secs(2))
        .build()
        .expect("client");
    let err = c.get("/api/v2/users/me").await.unwrap_err();
    assert_eq!(err.exit_code(), 11, "{err}");
    assert!(matches!(err, ZdkError::Network(_)));
}
