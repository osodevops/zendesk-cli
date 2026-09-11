//! The rate-limited, retrying Zendesk HTTP client (plan A6).
//!
//! [`ZendeskClient::execute`] runs one [`RequestSpec`] through the pipeline:
//! scope pre-flight → `--dry-run` short-circuit → `Authorization` from the [`AuthProvider`] →
//! [`RateGovernor::acquire`] → send (with a stable `Idempotency-Key` on writes) →
//! [`RateGovernor::observe`] + observers → classify: 2xx returns; a first 401 invalidates the
//! credential and retries; 429 / 5xx / transport failures go through the pure
//! [`retry::decide`]; everything else maps onto a typed [`ZdkError`].

pub mod dry_run;
pub mod idempotency;
pub mod observer;
pub mod rate_limit;
pub mod redact;
pub mod request;
pub mod response;
pub mod retry;

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

pub use observer::{AuditLogObserver, MemoryObserver, RequestObserver, RequestRecord};
pub use rate_limit::{Family, RateGovernor, RateHeaders};
pub use request::{Body, RequestSpec};
pub use response::ApiResponse;
pub use retry::{Decision, Outcome, RetryPolicy};

use crate::api::{Method, Operation};
use crate::auth::{AuthProvider, scopes};
use crate::error::{AuthFailure, RateBudget};
use crate::models::ZendeskErrorBody;
use crate::output::OutputFormat;
use crate::{Result, ZdkError};

/// `User-Agent` on every request.
pub const USER_AGENT: &str = concat!(
    "zendesk-cli/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/osodevops/zendesk-cli)"
);
/// Hosts other than the profile's own that `zdk api` may target (unauthenticated).
pub const EXTERNAL_HOSTS: &[&str] = &["status.zendesk.com"];
/// How much of an error body is kept in `ZdkError::Server`.
pub const MAX_ERROR_BODY: usize = 4096;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// reqwest is built with `rustls-no-provider`; make the library safe to use without `main`.
fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Builds a [`ZendeskClient`].
pub struct ClientBuilder {
    base: Url,
    auth: Option<Arc<dyn AuthProvider>>,
    governor: Option<Arc<RateGovernor>>,
    retry: RetryPolicy,
    timeout: Duration,
    dry_run: bool,
    dry_run_format: OutputFormat,
    observers: Vec<Arc<dyn RequestObserver>>,
    cancel: CancellationToken,
    profile: String,
    command: String,
    no_preflight: bool,
    http: Option<reqwest::Client>,
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("base", &self.base.as_str())
            .field("dry_run", &self.dry_run)
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl ClientBuilder {
    /// The auth provider (required).
    #[must_use]
    pub fn auth(mut self, provider: Arc<dyn AuthProvider>) -> Self {
        self.auth = Some(provider);
        self
    }

    /// The governor (default: a fresh one with the built-in settings).
    #[must_use]
    pub fn governor(mut self, governor: Arc<RateGovernor>) -> Self {
        self.governor = Some(governor);
        self
    }

    #[must_use]
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Per-request timeout (default 30 s).
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Render requests instead of sending them, in `format`.
    #[must_use]
    pub fn dry_run(mut self, enabled: bool, format: OutputFormat) -> Self {
        self.dry_run = enabled;
        self.dry_run_format = format;
        self
    }

    #[must_use]
    pub fn observer(mut self, observer: Arc<dyn RequestObserver>) -> Self {
        self.observers.push(observer);
        self
    }

    /// Checked between attempts; cancelled → `ZdkError::Interrupted`.
    #[must_use]
    pub fn cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = token;
        self
    }

    /// Profile name for observers and auth errors.
    #[must_use]
    pub fn profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = profile.into();
        self
    }

    /// The invoking command line for the audit log.
    #[must_use]
    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.command = command.into();
        self
    }

    /// Disable the scope pre-flight for every request.
    #[must_use]
    pub fn no_preflight(mut self, yes: bool) -> Self {
        self.no_preflight = yes;
        self
    }

    /// Use a pre-built reqwest client (tests, embedders).
    #[must_use]
    pub fn http(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    pub fn build(self) -> Result<ZendeskClient> {
        ensure_crypto_provider();
        let auth = self.auth.ok_or_else(|| {
            ZdkError::Config("ZendeskClient needs an auth provider (ClientBuilder::auth)".into())
        })?;
        let http = match self.http {
            Some(c) => c,
            None => reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .gzip(true)
                .timeout(self.timeout)
                .connect_timeout(CONNECT_TIMEOUT)
                .min_tls_version(reqwest::tls::Version::TLS_1_2)
                .build()?,
        };
        Ok(ZendeskClient {
            http,
            base: self.base,
            auth,
            governor: self
                .governor
                .unwrap_or_else(|| Arc::new(RateGovernor::new(&default_rate_settings()))),
            retry: self.retry,
            timeout: self.timeout,
            dry_run: self.dry_run,
            dry_run_format: self.dry_run_format,
            observers: self.observers,
            cancel: self.cancel,
            profile: self.profile,
            command: self.command,
            no_preflight: self.no_preflight,
        })
    }
}

fn default_rate_settings() -> crate::config::RateLimitSettings {
    let section = crate::config::RateLimitSection::default();
    crate::config::RateLimitSettings {
        strategy: section.strategy,
        max_concurrency: section.max_concurrency,
        reserve_percent: section.reserve_percent,
        warn_threshold: section.warn_threshold,
        respect_retry_after: section.respect_retry_after,
        high_volume_addon: section.high_volume_addon,
    }
}

/// See the module docs.
pub struct ZendeskClient {
    http: reqwest::Client,
    base: Url,
    auth: Arc<dyn AuthProvider>,
    governor: Arc<RateGovernor>,
    retry: RetryPolicy,
    timeout: Duration,
    dry_run: bool,
    dry_run_format: OutputFormat,
    observers: Vec<Arc<dyn RequestObserver>>,
    cancel: CancellationToken,
    profile: String,
    command: String,
    no_preflight: bool,
}

impl fmt::Debug for ZendeskClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZendeskClient")
            .field("base", &self.base.as_str())
            .field("auth", &self.auth)
            .field("governor", &self.governor)
            .field("retry", &self.retry)
            .field("dry_run", &self.dry_run)
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

/// Where a request is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The profile's own instance: authenticated and governed.
    Instance,
    /// `status.zendesk.com` etc.: no auth, no governor.
    External,
}

impl ZendeskClient {
    /// Start building a client for `base` (`https://{subdomain}.zendesk.com` or `--base-url`).
    #[must_use]
    pub fn builder(base: Url) -> ClientBuilder {
        ClientBuilder {
            base,
            auth: None,
            governor: None,
            retry: RetryPolicy::default(),
            timeout: Duration::from_secs(crate::config::settings::DEFAULT_TIMEOUT_SECS),
            dry_run: false,
            dry_run_format: OutputFormat::Json,
            observers: Vec::new(),
            cancel: CancellationToken::new(),
            profile: crate::config::DEFAULT_PROFILE.to_string(),
            command: String::new(),
            no_preflight: false,
            http: None,
        }
    }

    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    #[must_use]
    pub fn auth(&self) -> &Arc<dyn AuthProvider> {
        &self.auth
    }

    #[must_use]
    pub fn governor(&self) -> &Arc<RateGovernor> {
        &self.governor
    }

    #[must_use]
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry
    }

    #[must_use]
    pub const fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    #[must_use]
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// `GET path` (no query).
    pub async fn get(&self, path: &str) -> Result<ApiResponse> {
        self.execute(RequestSpec::get(path)).await
    }

    /// `GET path?query` decoded as `T`.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let mut spec = RequestSpec::get(path);
        for (k, v) in query {
            spec = spec.query(*k, v);
        }
        self.execute(spec).await?.json()
    }

    /// `GET path?query` as a JSON value.
    pub async fn get_value(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut spec = RequestSpec::get(path);
        for (k, v) in query {
            spec = spec.query(*k, v);
        }
        self.execute(spec).await?.value()
    }

    /// `POST`/`PUT`/`PATCH` a JSON body and decode the JSON response (Null on `204`).
    pub async fn send_json(&self, method: Method, path: &str, body: Value) -> Result<Value> {
        self.execute(RequestSpec::new(method, path).json(body))
            .await?
            .value()
    }

    /// `DELETE path`.
    pub async fn delete(&self, path: &str) -> Result<ApiResponse> {
        self.execute(RequestSpec::delete(path)).await
    }

    /// Run the full pipeline for one request.
    pub async fn execute(&self, mut spec: RequestSpec) -> Result<ApiResponse> {
        spec.resolve_op();
        let url = spec.url(&self.base)?;
        let target = self.target_for(&url)?;

        if target == Target::Instance
            && !self.no_preflight
            && !spec.no_preflight
            && let (Some(granted), Some(scope)) =
                (self.auth.granted_scopes(), spec.op.and_then(|o| o.scope))
        {
            scopes::preflight(&granted, &[scope])?;
        }

        let multipart = spec.body.as_ref().is_some_and(Body::is_multipart);
        let key = spec.idempotency_key.clone().or_else(|| {
            (spec.auto_idempotency_key && idempotency::needs_key(spec.method) && !multipart)
                .then(idempotency::generate)
        });

        if self.dry_run {
            dry_run::render(
                &spec,
                &url,
                self.dry_run_format,
                target == Target::Instance,
                key.as_deref(),
            )?;
            return Err(ZdkError::DryRun);
        }

        let method = spec.method;
        let family = Family::for_path(url.path());
        let replayable = (method.is_idempotent() || key.is_some()) && !multipart;
        let json_body = spec.json_body().cloned();
        let started = Instant::now();
        let mut attempt: u32 = 0;
        // `invalidate()` is offered once; `refresh_attempted` records whether it could do anything.
        let mut invalidate_offered = false;
        let mut refresh_attempted = false;

        loop {
            attempt += 1;
            if self.cancel.is_cancelled() {
                return Err(ZdkError::Interrupted);
            }
            let auth_header = match target {
                Target::Instance => Some(self.auth.authorization().await?),
                Target::External => None,
            };
            let _permit = match target {
                Target::Instance => Some(
                    self.governor
                        .acquire_with(method, url.path(), json_body.as_ref(), &spec.rate_rules)
                        .await?,
                ),
                Target::External => None,
            };
            let request =
                self.build_request(&mut spec, &url, auth_header.as_ref(), key.as_deref())?;
            Self::log_request(&request, attempt);

            let sent_at = Instant::now();
            let response = self.http.execute(request).await;
            let attempt_ms = elapsed_ms(sent_at);

            let (status, headers, body) = match response {
                Err(e) => {
                    let sent = !e.is_connect();
                    tracing::debug!(target: "zdk::http", %method, url = %redact::redact_url(&url), attempt, error = %e, "transport error");
                    self.notify(
                        &url,
                        method,
                        attempt,
                        None,
                        attempt_ms,
                        &RateHeaders::default(),
                        None,
                        0,
                        Some(e.to_string()),
                    );
                    let outcome = Outcome::Transport { sent, replayable };
                    match retry::decide(attempt, &outcome, &self.retry) {
                        Decision::Retry(wait) => {
                            self.pause(wait, attempt, &format!("network error: {e}"))
                                .await?;
                            continue;
                        }
                        Decision::Fail => return Err(ZdkError::Network(e)),
                    }
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let headers = resp.headers().clone();
                    match resp.bytes().await {
                        Ok(body) => (status, headers, body),
                        Err(e) => {
                            let rate = RateHeaders::parse(&headers);
                            if target == Target::Instance {
                                self.governor.observe(&rate, family);
                            }
                            self.notify(
                                &url,
                                method,
                                attempt,
                                Some(status),
                                attempt_ms,
                                &rate,
                                ApiResponse::request_id_from(&headers),
                                0,
                                Some(e.to_string()),
                            );
                            let outcome = Outcome::Transport {
                                sent: true,
                                replayable,
                            };
                            match retry::decide(attempt, &outcome, &self.retry) {
                                Decision::Retry(wait) => {
                                    self.pause(wait, attempt, &format!("body read failed: {e}"))
                                        .await?;
                                    continue;
                                }
                                Decision::Fail => return Err(ZdkError::Network(e)),
                            }
                        }
                    }
                }
            };

            let rate = RateHeaders::parse(&headers);
            let request_id = ApiResponse::request_id_from(&headers);
            if target == Target::Instance {
                self.governor.observe(&rate, family);
            }
            self.notify(
                &url,
                method,
                attempt,
                Some(status),
                attempt_ms,
                &rate,
                request_id.clone(),
                body.len() as u64,
                None,
            );
            Self::log_response(&url, method, status, &headers, &body, attempt, attempt_ms);

            if (200..300).contains(&status) {
                return Ok(ApiResponse {
                    status,
                    headers,
                    body,
                    request_id,
                    rate,
                    url,
                    elapsed: started.elapsed(),
                    attempts: attempt,
                });
            }

            if status == 401 && target == Target::Instance && !invalidate_offered {
                invalidate_offered = true;
                if self.auth.invalidate().await? {
                    refresh_attempted = true;
                    tracing::info!(target: "zdk::http", "401 from Zendesk: credential invalidated, retrying once with a fresh token");
                    continue;
                }
            }

            let outcome = Outcome::Status {
                status,
                retry_after: rate.retry_after,
                replayable,
            };
            if let Decision::Retry(wait) = retry::decide(attempt, &outcome, &self.retry) {
                self.pause(wait, attempt, &format!("HTTP {status}")).await?;
                continue;
            }
            return Err(self.map_error(
                status,
                &body,
                &rate,
                request_id,
                attempt,
                &spec,
                &url,
                refresh_attempted,
            ));
        }
    }

    fn target_for(&self, url: &Url) -> Result<Target> {
        let same_host = url.host_str() == self.base.host_str()
            && url.port_or_known_default() == self.base.port_or_known_default();
        if same_host {
            return Ok(Target::Instance);
        }
        if url
            .host_str()
            .is_some_and(|h| EXTERNAL_HOSTS.iter().any(|e| e.eq_ignore_ascii_case(h)))
        {
            return Ok(Target::External);
        }
        Err(ZdkError::Usage(format!(
            "{} is not on this profile's host ({}); only {} may be targeted as an external URL",
            redact::redact_url(url),
            self.base.host_str().unwrap_or("?"),
            EXTERNAL_HOSTS.join(", ")
        )))
    }

    fn build_request(
        &self,
        spec: &mut RequestSpec,
        url: &Url,
        auth: Option<&HeaderValue>,
        key: Option<&str>,
    ) -> Result<reqwest::Request> {
        let mut rb = self
            .http
            .request(spec.method.into(), url.clone())
            .header(ACCEPT, "application/json");
        if let Some(a) = auth {
            rb = rb.header(AUTHORIZATION, a.clone());
        }
        if let Some(k) = key {
            rb = rb.header(idempotency::HEADER, k);
        }
        for (k, v) in &spec.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        match spec.body.as_ref() {
            Some(Body::Json(v)) => {
                rb = rb
                    .header(CONTENT_TYPE, "application/json")
                    .body(serde_json::to_vec(v)?);
            }
            Some(Body::Bytes { data, content_type }) => {
                rb = rb
                    .header(CONTENT_TYPE, content_type.as_str())
                    .body(data.clone());
            }
            Some(Body::Multipart(_)) => {
                // Consumed on first send; multipart is never retried.
                if let Some(Body::Multipart(form)) = spec.body.take() {
                    rb = rb.multipart(form);
                }
            }
            None => {}
        }
        rb = rb.timeout(spec.timeout.unwrap_or(self.timeout));
        rb.build().map_err(ZdkError::Network)
    }

    /// Sleep between attempts, honouring ctrl-c.
    async fn pause(&self, wait: Duration, attempt: u32, why: &str) -> Result<()> {
        tracing::info!(
            target: "zdk::http",
            attempt,
            wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            "{why}; retrying"
        );
        if wait >= Duration::from_secs(2) {
            crate::output::warn(&format!(
                "{why}; retrying in {:.0}s (attempt {} of {})",
                wait.as_secs_f64(),
                attempt + 1,
                self.retry.max_attempts
            ));
        }
        if wait.is_zero() {
            return Ok(());
        }
        tokio::select! {
            () = self.cancel.cancelled() => Err(ZdkError::Interrupted),
            () = tokio::time::sleep(wait) => Ok(()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        url: &Url,
        method: Method,
        attempt: u32,
        status: Option<u16>,
        duration_ms: u64,
        rate: &RateHeaders,
        request_id: Option<String>,
        bytes: u64,
        error: Option<String>,
    ) {
        if self.observers.is_empty() {
            return;
        }
        let mut path = url.path().to_string();
        if url.query().is_some() {
            let redacted = redact::redact_url(url);
            if let Some(q) = redacted.split_once('?').map(|(_, q)| q) {
                path.push('?');
                path.push_str(q);
            }
        }
        let record = RequestRecord {
            ts: chrono::Utc::now(),
            profile: self.profile.clone(),
            method: method.as_str().to_string(),
            path,
            status,
            duration_ms,
            rate: rate.clone(),
            request_id,
            bytes,
            command: self.command.clone(),
            attempt,
            error,
        };
        for o in &self.observers {
            o.observe(&record);
        }
    }

    fn log_request(request: &reqwest::Request, attempt: u32) {
        tracing::debug!(
            target: "zdk::http",
            method = %request.method(),
            url = %redact::redact_url(request.url()),
            attempt,
            "request"
        );
        if tracing::enabled!(target: "zdk::http", tracing::Level::TRACE) {
            let body = request
                .body()
                .and_then(reqwest::Body::as_bytes)
                .map(redact::redact_body)
                .unwrap_or_default();
            tracing::trace!(
                target: "zdk::http",
                headers = ?redact::redact_headers(request.headers()),
                body = %body,
                "request detail"
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn log_response(
        url: &Url,
        method: Method,
        status: u16,
        headers: &HeaderMap,
        body: &Bytes,
        attempt: u32,
        duration_ms: u64,
    ) {
        tracing::debug!(
            target: "zdk::http",
            %method,
            url = %redact::redact_url(url),
            status,
            attempt,
            duration_ms,
            bytes = body.len(),
            "response"
        );
        if tracing::enabled!(target: "zdk::http", tracing::Level::TRACE) {
            tracing::trace!(
                target: "zdk::http",
                headers = ?redact::redact_headers(headers),
                body = %redact::redact_body(body),
                "response detail"
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn map_error(
        &self,
        status: u16,
        body: &Bytes,
        rate: &RateHeaders,
        request_id: Option<String>,
        attempts: u32,
        spec: &RequestSpec,
        url: &Url,
        refresh_attempted: bool,
    ) -> ZdkError {
        let parsed = ZendeskErrorBody::parse(body);
        let message = parsed
            .as_ref()
            .map_or_else(|| fallback_message(status, body), ZendeskErrorBody::message);
        let details = parsed
            .as_ref()
            .map(|p| p.details.clone())
            .unwrap_or_default();
        let op = spec.op;

        match status {
            400 if parsed
                .as_ref()
                .is_some_and(ZendeskErrorBody::is_invalid_scope) =>
            {
                ZdkError::Auth(AuthFailure::InvalidScope {
                    scope: op.and_then(|o| o.scope).unwrap_or("(unknown)").to_string(),
                    detail: message,
                })
            }
            400 if parsed
                .as_ref()
                .is_some_and(ZendeskErrorBody::is_offset_limit) =>
            {
                ZdkError::PaginationLimit {
                    endpoint: url.path().to_string(),
                    alternatives: crate::pagination::offset::alternatives_for(url.path()),
                }
            }
            401 => {
                tracing::warn!(target: "zdk::http", "Zendesk returned 401: {message}");
                ZdkError::Auth(if refresh_attempted {
                    AuthFailure::Expired {
                        profile: self.profile.clone(),
                        detail: message,
                    }
                } else {
                    AuthFailure::NotLoggedIn {
                        profile: self.profile.clone(),
                    }
                })
            }
            403 => ZdkError::Forbidden {
                message,
                required_scope: op.and_then(|o| o.scope).map(str::to_string),
                granted: self.auth.granted_scopes().unwrap_or_default(),
                request_id,
            },
            404 => {
                let (resource, id) = not_found_target(op, url);
                ZdkError::NotFound {
                    resource,
                    id,
                    request_id,
                }
            }
            429 => {
                let budget_name = rate_limit::match_rules(spec.method, url.path())
                    .first()
                    .map_or_else(
                        || Family::for_path(url.path()).name().to_string(),
                        |(rule, _)| rule.name.to_string(),
                    );
                ZdkError::RateLimited {
                    budget: RateBudget {
                        name: budget_name,
                        limit: rate.limit,
                        remaining: rate.remaining,
                    },
                    retry_after: rate.retry_after,
                    request_id,
                }
            }
            500..=599 => ZdkError::Server {
                status,
                attempts,
                request_id,
                body: truncate_text(body, MAX_ERROR_BODY),
            },
            _ => ZdkError::Validation {
                status,
                message,
                details,
                request_id,
            },
        }
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn fallback_message(status: u16, body: &Bytes) -> String {
    let text = truncate_text(body, 200);
    let text = text.trim();
    let reason = http::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("error");
    if text.is_empty() {
        format!("HTTP {status} {reason}")
    } else {
        format!("HTTP {status} {reason}: {}", redact::redact_text(text))
    }
}

fn truncate_text(body: &[u8], max: usize) -> String {
    let text = String::from_utf8_lossy(body);
    if text.len() <= max {
        return text.into_owned();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// `("ticket", "42")` for a 404 on `/api/v2/tickets/42`.
fn not_found_target(op: Option<&'static Operation>, url: &Url) -> (String, String) {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let last = segments
        .last()
        .map(|s| s.trim_end_matches(".json"))
        .unwrap_or_default();
    if let Some(op) = op {
        let tpl: Vec<&str> = op.path.trim_matches('/').split('/').collect();
        let params = crate::api::path_params(op, url.path());
        let resource = tpl
            .iter()
            .rev()
            .find(|s| !s.starts_with('{'))
            .map_or_else(|| "resource".into(), |s| singular(s));
        let id = params
            .last()
            .map_or_else(|| last.to_string(), |(_, v)| v.clone());
        return (resource, id);
    }
    if last.chars().all(|c| c.is_ascii_digit()) && segments.len() >= 2 {
        (singular(segments[segments.len() - 2]), last.to_string())
    } else {
        ("resource".into(), url.path().to_string())
    }
}

fn singular(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies") {
        format!("{stem}y")
    } else if word.ends_with("ss") || !word.ends_with('s') {
        word.to_string()
    } else {
        word.trim_end_matches('s').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_names_resource_and_id() {
        let url = Url::parse("https://acme.zendesk.com/api/v2/tickets/42.json").unwrap();
        let op = crate::api::match_path(Method::Get, "/api/v2/tickets/42.json");
        assert_eq!(not_found_target(op, &url), ("ticket".into(), "42".into()));
        let url = Url::parse("https://acme.zendesk.com/api/v2/organizations/7").unwrap();
        assert_eq!(
            not_found_target(None, &url),
            ("organization".into(), "7".into())
        );
        let url = Url::parse("https://acme.zendesk.com/api/v2/nothing/here").unwrap();
        assert_eq!(not_found_target(None, &url).0, "resource");
        assert_eq!(singular("categories"), "category");
        assert_eq!(singular("business"), "business");
    }

    #[test]
    fn fallback_message_and_truncation() {
        assert_eq!(fallback_message(502, &Bytes::new()), "HTTP 502 Bad Gateway");
        assert_eq!(
            fallback_message(418, &Bytes::from_static(b"short")),
            "HTTP 418 I'm a teapot: short"
        );
        assert_eq!(truncate_text("héllo".as_bytes(), 2), "h…");
    }
}
