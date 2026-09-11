//! Authentication: OAuth 2.0 (authorization code + PKCE, client credentials) and legacy API tokens.
//!
//! The HTTP layer only ever talks to an [`AuthProvider`]; concrete providers live in this
//! module's submodules and are selected by [`resolve_provider`] from the active profile and
//! environment. The `login_*`, [`logout`], [`status`] and [`refresh_now`] functions are the
//! entry points behind `zdk auth …`; they return data and never print (the CLI renders).

pub mod api_token;
pub mod authorization_code;
pub mod client_credentials;
pub mod oauth;
pub mod refresh;
pub mod revoke;
pub mod scopes;
pub mod token;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use http::HeaderValue;
use secrecy::{ExposeSecret, SecretString};
use url::Url;

pub use api_token::ApiTokenProvider;
use authorization_code::{BrowserOpener, LineReader, LoginFlow, UrlHook};
use refresh::TokenRefresher;
use token::{Credential, TokenSet};

use crate::config::{AuthSettings, EnvOverrides, Settings};
use crate::error::AuthFailure;
use crate::store::SharedStore;
use crate::{Result, ZdkError};

/// How the current credential was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    AuthorizationCode,
    ClientCredentials,
    ApiToken,
    /// `ZENDESK_ACCESS_TOKEN` supplied directly.
    StaticToken,
}

impl GrantKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorizationCode => "authorization_code",
            Self::ClientCredentials => "client_credentials",
            Self::ApiToken => "api_token",
            Self::StaticToken => "static_token",
        }
    }
}

/// What `zdk auth status` and `zdk doctor` display about the active provider.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthDescription {
    pub grant: GrantKind,
    pub profile: String,
    pub subdomain: Option<String>,
    pub client_id: Option<String>,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub has_refresh_token: bool,
    pub store: Option<String>,
    /// Days until 30 April 2027 when using a legacy API token.
    pub api_token_days_remaining: Option<i64>,
}

/// Source of the `Authorization` header for every request.
#[async_trait]
pub trait AuthProvider: Send + Sync + std::fmt::Debug {
    /// Value for the `Authorization` header. Performs a pre-emptive refresh or re-mint when due.
    async fn authorization(&self) -> Result<HeaderValue>;

    /// Called by the HTTP core after a 401 so the next `authorization()` forces a refresh.
    /// Returns `false` when nothing can be done (static tokens, API tokens, no refresh token).
    async fn invalidate(&self) -> Result<bool>;

    /// Granted scopes when known (OAuth), used for pre-flight checks. `None` = unknown/not applicable.
    fn granted_scopes(&self) -> Option<Vec<String>>;

    fn description(&self) -> AuthDescription;

    /// A refresh-token rotation that could not be persisted during this process (exit 10 at
    /// the end of the command). Only OAuth providers ever report one.
    fn rotation_error(&self) -> Option<String> {
        None
    }
}

/// `Bearer {token}`, marked sensitive so it never appears in logs.
pub fn bearer_header(token: &SecretString) -> Result<HeaderValue> {
    let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
        .map_err(|e| ZdkError::Config(format!("access token is not a valid header value: {e}")))?;
    value.set_sensitive(true);
    Ok(value)
}

/// `User-Agent` sent to the token endpoint.
pub const USER_AGENT: &str = concat!(
    "zendesk-cli/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/osodevops/zendesk-cli)"
);

/// reqwest is built with `rustls-no-provider` and panics without a process-wide provider.
/// `main` installs ring first thing; this makes the library safe on its own (tests, embedders).
fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Err = a provider was already installed (by `main`), which is exactly what we want.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A plain client for the token/revocation endpoints (TLS 1.2+, 10 s connect).
pub fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    ensure_crypto_provider();
    Ok(reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .user_agent(USER_AGENT)
        .min_tls_version(reqwest::tls::Version::TLS_1_2)
        .build()?)
}

// ---------------------------------------------------------------------------------------------
// StaticBearer
// ---------------------------------------------------------------------------------------------

/// A fixed bearer token (`ZENDESK_ACCESS_TOKEN`); never refreshed.
#[derive(Clone)]
pub struct StaticBearer {
    token: SecretString,
    profile: String,
}

impl StaticBearer {
    #[must_use]
    pub fn new(token: impl Into<String>, profile: impl Into<String>) -> Self {
        Self {
            token: SecretString::from(token.into()),
            profile: profile.into(),
        }
    }
}

impl std::fmt::Debug for StaticBearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticBearer")
            .field("token", &"[redacted]")
            .field("profile", &self.profile)
            .finish()
    }
}

#[async_trait]
impl AuthProvider for StaticBearer {
    async fn authorization(&self) -> Result<HeaderValue> {
        bearer_header(&self.token)
    }

    async fn invalidate(&self) -> Result<bool> {
        Ok(false)
    }

    fn granted_scopes(&self) -> Option<Vec<String>> {
        None
    }

    fn description(&self) -> AuthDescription {
        AuthDescription {
            grant: GrantKind::StaticToken,
            profile: self.profile.clone(),
            subdomain: None,
            client_id: None,
            scopes: vec![],
            expires_at: None,
            has_refresh_token: false,
            store: Some("env".into()),
            api_token_days_remaining: None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// OAuthProvider
// ---------------------------------------------------------------------------------------------

/// After a failed *pre-emptive* renewal (token still valid) wait this long before trying again,
/// so a flapping token endpoint does not add a POST to every request.
const PREEMPTIVE_RETRY_BACKOFF: Duration = Duration::from_secs(60);

/// Store-backed OAuth provider: refreshes (authorization code) or re-mints (client
/// credentials) when due, persists rotations before using them.
pub struct OAuthProvider {
    profile: String,
    store: SharedStore,
    refresher: Arc<TokenRefresher>,
    cache: Mutex<Option<TokenSet>>,
    settings: AuthSettings,
    /// `ZENDESK_CLIENT_SECRET` / `--client-secret` for client-credentials re-mint.
    client_secret: Option<SecretString>,
    base: Url,
    http: reqwest::Client,
    gate: tokio::sync::Mutex<()>,
    force_renew: AtomicBool,
    preemptive_retry_after: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for OAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthProvider")
            .field("profile", &self.profile)
            .field("store", &self.store.kind())
            .field("base", &self.base.as_str())
            .field("settings", &self.settings)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[redacted]"),
            )
            .field("cached", &self.cached())
            .finish_non_exhaustive()
    }
}

impl OAuthProvider {
    /// `initial` seeds the cache (what `resolve_provider` just loaded) so `description()` and
    /// `granted_scopes()` work before the first request.
    pub fn new(
        profile: impl Into<String>,
        store: SharedStore,
        base: Url,
        http: reqwest::Client,
        settings: AuthSettings,
        client_secret: Option<SecretString>,
        initial: Option<TokenSet>,
    ) -> Self {
        let profile = profile.into();
        let refresher = Arc::new(TokenRefresher::new(
            http.clone(),
            base.clone(),
            profile.clone(),
            store.clone(),
        ));
        Self {
            profile,
            store,
            refresher,
            cache: Mutex::new(initial),
            settings,
            client_secret,
            base,
            http,
            gate: tokio::sync::Mutex::new(()),
            force_renew: AtomicBool::new(false),
            preemptive_retry_after: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn refresher(&self) -> &Arc<TokenRefresher> {
        &self.refresher
    }

    /// The token set currently in use (a clone; secrets stay wrapped).
    #[must_use]
    pub fn cached(&self) -> Option<TokenSet> {
        self.cache.lock().expect("token cache poisoned").clone()
    }

    fn set_cached(&self, token: TokenSet) {
        *self.cache.lock().expect("token cache poisoned") = Some(token);
    }

    fn not_logged_in(&self) -> ZdkError {
        ZdkError::Auth(AuthFailure::NotLoggedIn {
            profile: self.profile.clone(),
        })
    }

    /// Cache, else the store.
    fn current(&self) -> Result<TokenSet> {
        if let Some(t) = self.cached() {
            return Ok(t);
        }
        match self.store.load(&self.profile)? {
            Some(Credential::OAuth(t)) => {
                self.set_cached(t.clone());
                Ok(t)
            }
            Some(Credential::ApiToken { .. }) | None => Err(self.not_logged_in()),
        }
    }

    fn can_renew(&self, current: &TokenSet) -> bool {
        match current.grant {
            GrantKind::ClientCredentials => {
                client_credentials::secret_for(current, self.client_secret.as_ref()).is_some()
            }
            _ => current.refresh_token.is_some(),
        }
    }

    /// Refresh (authorization code) or re-mint (client credentials).
    async fn renew(&self, current: &TokenSet, now: DateTime<Utc>) -> Result<TokenSet> {
        match current.grant {
            GrantKind::ClientCredentials => {
                let secret = client_credentials::secret_for(current, self.client_secret.as_ref())
                    .ok_or_else(|| {
                    ZdkError::Auth(AuthFailure::ClientSecretRequired {
                        profile: self.profile.clone(),
                    })
                })?;
                let mut fresh = client_credentials::mint(
                    &self.http,
                    &self.base,
                    &current.subdomain,
                    &current.client_id,
                    &secret,
                    &current.scopes,
                    client_credentials::original_lifetime_secs(current),
                    now,
                )
                .await?;
                fresh.client_secret.clone_from(&current.client_secret);
                // No rotation is involved: a save failure just means the next process re-mints.
                if let Err(e) = self
                    .store
                    .save(&self.profile, &Credential::OAuth(fresh.clone()))
                {
                    tracing::warn!(target: "zdk::auth", error = %e, "re-minted token could not be saved");
                }
                Ok(fresh)
            }
            _ => {
                self.refresher
                    .refresh(current, self.client_secret.as_ref())
                    .await
            }
        }
    }

    fn in_preemptive_backoff(&self) -> bool {
        self.preemptive_retry_after
            .lock()
            .expect("backoff poisoned")
            .is_some_and(|until| Instant::now() < until)
    }

    fn set_preemptive_backoff(&self) {
        *self
            .preemptive_retry_after
            .lock()
            .expect("backoff poisoned") = Some(Instant::now() + PREEMPTIVE_RETRY_BACKOFF);
    }
}

#[async_trait]
impl AuthProvider for OAuthProvider {
    async fn authorization(&self) -> Result<HeaderValue> {
        let _serialised = self.gate.lock().await;
        let now = Utc::now();
        let mut current = self.current()?;
        let forced = self.force_renew.swap(false, Ordering::SeqCst);
        let expired = current.is_expired(now);
        let due = self.settings.auto_refresh
            && (expired || current.refresh_due(self.settings.refresh_at_percent, now));

        if forced || expired || (due && !self.in_preemptive_backoff()) {
            if !self.can_renew(&current) {
                if forced || expired {
                    return Err(match current.grant {
                        GrantKind::ClientCredentials => {
                            ZdkError::Auth(AuthFailure::ClientSecretRequired {
                                profile: self.profile.clone(),
                            })
                        }
                        _ => ZdkError::Auth(AuthFailure::Expired {
                            profile: self.profile.clone(),
                            detail: "no refresh token was issued for this token".into(),
                        }),
                    });
                }
                // Due but still valid and nothing to renew with: use it until it expires.
                return bearer_header(&current.access_token);
            }
            match self.renew(&current, now).await {
                Ok(fresh) => {
                    self.set_cached(fresh.clone());
                    current = fresh;
                }
                Err(e) if forced || expired => return Err(e),
                Err(e) => {
                    // Pre-emptive renewal failed but the token is still valid: keep going.
                    tracing::warn!(target: "zdk::auth", error = %e, "pre-emptive token renewal failed; using the current token");
                    self.set_preemptive_backoff();
                }
            }
        }
        bearer_header(&current.access_token)
    }

    async fn invalidate(&self) -> Result<bool> {
        let current = self.current()?;
        let can = self.can_renew(&current);
        if can {
            self.force_renew.store(true, Ordering::SeqCst);
        }
        Ok(can)
    }

    fn granted_scopes(&self) -> Option<Vec<String>> {
        self.cached().map(|t| t.scopes).filter(|s| !s.is_empty())
    }

    fn description(&self) -> AuthDescription {
        let t = self.cached();
        AuthDescription {
            grant: t.as_ref().map_or(self.settings.method, |t| t.grant),
            profile: self.profile.clone(),
            subdomain: t.as_ref().map(|t| t.subdomain.clone()),
            client_id: t.as_ref().map(|t| t.client_id.clone()),
            scopes: t.as_ref().map(|t| t.scopes.clone()).unwrap_or_default(),
            expires_at: t.as_ref().and_then(|t| t.expires_at),
            has_refresh_token: t.as_ref().is_some_and(|t| t.refresh_token.is_some()),
            store: Some(self.store.kind().to_string()),
            api_token_days_remaining: None,
        }
    }

    fn rotation_error(&self) -> Option<String> {
        self.refresher.rotation_error()
    }
}

// ---------------------------------------------------------------------------------------------
// Provider resolution
// ---------------------------------------------------------------------------------------------

/// The instance base URL: the configured one (which may be a `ZENDESK_BASE_URL` override),
/// else derived from a credential's subdomain.
fn base_url_for(settings: &Settings, subdomain: &str) -> Result<Url> {
    if let Some(b) = &settings.base_url {
        return Ok(b.clone());
    }
    if subdomain.is_empty() {
        return settings.require_base_url().cloned();
    }
    Ok(Url::parse(&format!("https://{subdomain}.zendesk.com"))?)
}

/// Pick the provider for the active profile: `ZENDESK_ACCESS_TOKEN` → `ZENDESK_EMAIL`+`ZENDESK_API_TOKEN`
/// → the credential store → not logged in.
pub fn resolve_provider(
    settings: &Settings,
    env: &EnvOverrides,
    store: SharedStore,
) -> Result<Arc<dyn AuthProvider>> {
    let profile = settings.profile_name.clone();

    if let Some(token) = env.access_token.as_ref() {
        return Ok(Arc::new(StaticBearer::new(token.expose_secret(), profile)));
    }

    if let Some(token) = env.api_token.as_ref() {
        let email = settings.profile.email.clone().ok_or_else(|| {
            ZdkError::Config(
                "ZENDESK_API_TOKEN is set but no email is configured: set ZENDESK_EMAIL or the profile's `email`"
                    .into(),
            )
        })?;
        return Ok(Arc::new(ApiTokenProvider::new(
            profile,
            email,
            token.clone(),
            settings.subdomain.clone().unwrap_or_default(),
            settings.auth.suppress_deprecation,
            Some("env".into()),
        )));
    }

    match store.load(&profile)? {
        Some(Credential::OAuth(token)) => {
            let base = base_url_for(settings, &token.subdomain)?;
            Ok(Arc::new(OAuthProvider::new(
                profile,
                store,
                base,
                http_client(settings.timeout)?,
                settings.auth.clone(),
                env.client_secret.clone(),
                Some(token),
            )))
        }
        Some(Credential::ApiToken {
            email,
            token,
            subdomain,
        }) => Ok(Arc::new(ApiTokenProvider::new(
            profile,
            email,
            token,
            settings.subdomain.clone().unwrap_or(subdomain),
            settings.auth.suppress_deprecation,
            Some(store.kind().to_string()),
        ))),
        None => Err(ZdkError::Auth(AuthFailure::NotLoggedIn { profile })),
    }
}

// ---------------------------------------------------------------------------------------------
// Login / logout / status entry points (called by `zdk auth …`)
// ---------------------------------------------------------------------------------------------

/// Fixed redirect port used when no listener runs (`--no-browser`) and nothing configures one.
pub const DEFAULT_MANUAL_CALLBACK_PORT: u16 = 8484;

/// Inputs for `zdk auth login`. Unset fields fall back to the profile / environment.
#[derive(Default)]
pub struct LoginOptions {
    pub subdomain: Option<String>,
    pub client_id: Option<String>,
    /// `--client-secret` (the environment's `ZENDESK_CLIENT_SECRET` is the fallback).
    pub client_secret: Option<SecretString>,
    /// Already expanded (`scopes::expand`); empty = use the profile's scopes.
    pub scopes: Vec<String>,
    /// Loopback port (`0` = ephemeral); falls back to `auth.callback_port`.
    pub port: Option<u16>,
    /// `--redirect-uri`: paste-from-address-bar flow with this registered redirect.
    pub redirect_uri: Option<String>,
    /// `--no-browser`: print the URL, read the code from stdin.
    pub no_browser: bool,
    /// `--expires-in` seconds (300–172 800).
    pub expires_in: Option<u64>,
    /// `--store-secret`: keep the client secret in the credential store for re-mint.
    pub store_secret: bool,
    /// How long the browser flow waits (default 300 s).
    pub timeout: Option<Duration>,
    /// Test seam; default uses the `open` crate.
    pub open_browser: Option<BrowserOpener>,
    /// Test seam; default reads one line from stdin.
    pub stdin_reader: Option<LineReader>,
    /// Receives the authorize URL so the CLI can print it (core never prints).
    pub on_authorize_url: Option<UrlHook>,
}

impl std::fmt::Debug for LoginOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginOptions")
            .field("subdomain", &self.subdomain)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[redacted]"),
            )
            .field("scopes", &self.scopes)
            .field("port", &self.port)
            .field("redirect_uri", &self.redirect_uri)
            .field("no_browser", &self.no_browser)
            .field("expires_in", &self.expires_in)
            .field("store_secret", &self.store_secret)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Subdomain + base URL for a login: an explicit `--subdomain` that differs from the
/// configured one targets `https://{sub}.zendesk.com`; otherwise the configured base
/// (which honours `ZENDESK_BASE_URL`).
fn login_target(settings: &Settings, subdomain: Option<&str>) -> Result<(String, Url)> {
    let sub = subdomain
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| settings.subdomain.clone())
        .ok_or_else(|| {
            ZdkError::Config(
                "no subdomain: pass --subdomain, set ZENDESK_SUBDOMAIN, or run `zdk config init`"
                    .into(),
            )
        })?;
    let base = if settings.subdomain.as_deref() == Some(sub.as_str()) || settings.base_url.is_none()
    {
        base_url_for(settings, &sub)?
    } else {
        Url::parse(&format!("https://{sub}.zendesk.com"))?
    };
    Ok((sub, base))
}

fn login_client_id(settings: &Settings, explicit: Option<&str>) -> Result<String> {
    explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| settings.profile.client_id.clone())
        .ok_or_else(|| {
            ZdkError::Config(
                "no OAuth client id: pass --client-id, set ZENDESK_CLIENT_ID, or add `client_id` to the profile"
                    .into(),
            )
        })
}

fn login_scopes(settings: &Settings, explicit: &[String]) -> Result<Vec<String>> {
    let scopes = if explicit.is_empty() {
        settings.profile.scopes.clone()
    } else {
        explicit.to_vec()
    };
    scopes::validate_requested(&scopes)?;
    Ok(scopes)
}

fn default_browser_opener() -> BrowserOpener {
    Box::new(|url: &Url| {
        open::that_detached(url.as_str())
            .map_err(|e| ZdkError::Other(format!("could not open a browser: {e}")))
    })
}

fn default_stdin_reader() -> LineReader {
    Box::new(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(line)
    })
}

/// `zdk auth login` (authorization code + PKCE). Saves the token under the active profile
/// and returns it. The caller verifies it with `GET /api/v2/users/me` if it wants to.
pub async fn login_authorization_code(
    settings: &Settings,
    env: &EnvOverrides,
    store: &SharedStore,
    mut opts: LoginOptions,
) -> Result<TokenSet> {
    let (subdomain, base) = login_target(settings, opts.subdomain.as_deref())?;
    let client_id = login_client_id(settings, opts.client_id.as_deref())?;
    let scopes = login_scopes(settings, &opts.scopes)?;
    let flow = LoginFlow {
        http: http_client(settings.timeout)?,
        base,
        subdomain,
        client_id,
        client_secret: opts
            .client_secret
            .take()
            .or_else(|| env.client_secret.clone()),
        scopes,
        expires_in: opts.expires_in,
        timeout: opts.timeout.unwrap_or(authorization_code::DEFAULT_TIMEOUT),
    };
    let announce = opts.on_authorize_url.take();

    let mut token = if opts.no_browser || opts.redirect_uri.is_some() {
        let redirect = if let Some(u) = &opts.redirect_uri {
            Url::parse(u)
                .map_err(|e| ZdkError::Usage(format!("--redirect-uri '{u}' is not a URL: {e}")))?
        } else {
            let port = opts
                .port
                .or(settings.auth.callback_port)
                .filter(|p| *p != 0)
                .unwrap_or(DEFAULT_MANUAL_CALLBACK_PORT);
            Url::parse(&format!(
                "http://127.0.0.1:{port}{}",
                authorization_code::CALLBACK_PATH
            ))?
        };
        let mut reader = opts
            .stdin_reader
            .take()
            .unwrap_or_else(default_stdin_reader);
        flow.run_manual(&redirect, &mut reader, announce.as_ref())
            .await?
    } else {
        let opener = opts
            .open_browser
            .take()
            .unwrap_or_else(default_browser_opener);
        let port = opts.port.or(settings.auth.callback_port).unwrap_or(0);
        flow.run_browser(port, &opener, announce.as_ref()).await?
    };

    if !opts.store_secret {
        token.client_secret = None;
    }
    store.save(&settings.profile_name, &Credential::OAuth(token.clone()))?;
    Ok(token)
}

/// `zdk auth login --client-credentials`. The secret comes from `opts.client_secret`, else
/// `ZENDESK_CLIENT_SECRET`; it is only persisted with `store_secret`.
pub async fn login_client_credentials(
    settings: &Settings,
    env: &EnvOverrides,
    store: &SharedStore,
    opts: LoginOptions,
) -> Result<TokenSet> {
    let (subdomain, base) = login_target(settings, opts.subdomain.as_deref())?;
    let client_id = login_client_id(settings, opts.client_id.as_deref())?;
    let scopes = login_scopes(settings, &opts.scopes)?;
    let secret = opts
        .client_secret
        .clone()
        .or_else(|| env.client_secret.clone())
        .ok_or_else(|| {
            ZdkError::Auth(AuthFailure::ClientSecretRequired {
                profile: settings.profile_name.clone(),
            })
        })?;
    let http = http_client(settings.timeout)?;
    let mut token = client_credentials::mint(
        &http,
        &base,
        &subdomain,
        &client_id,
        &secret,
        &scopes,
        opts.expires_in,
        Utc::now(),
    )
    .await?;
    if opts.store_secret {
        token.client_secret = Some(secret);
    }
    store.save(&settings.profile_name, &Credential::OAuth(token.clone()))?;
    Ok(token)
}

/// `zdk auth login --api-token`: store the legacy pair for the active profile.
pub fn login_api_token(
    settings: &Settings,
    store: &SharedStore,
    email: &str,
    token: SecretString,
    subdomain: Option<&str>,
) -> Result<()> {
    let email = email.trim();
    if email.is_empty() || !email.contains('@') {
        return Err(ZdkError::Usage(format!(
            "'{email}' is not an email address (--email is the agent's login email)"
        )));
    }
    if token.expose_secret().trim().is_empty() {
        return Err(ZdkError::Usage("the API token is empty".into()));
    }
    let (subdomain, _) = login_target(settings, subdomain)?;
    store.save(
        &settings.profile_name,
        &Credential::ApiToken {
            email: email.to_string(),
            token,
            subdomain,
        },
    )
}

/// Inputs for `zdk auth logout`.
#[derive(Debug, Clone, Copy)]
pub struct LogoutOptions {
    /// Revoke OAuth tokens server-side before deleting (best effort).
    pub revoke: bool,
    /// Every profile in the store, not just the active one.
    pub all: bool,
}

impl Default for LogoutOptions {
    fn default() -> Self {
        Self {
            revoke: true,
            all: false,
        }
    }
}

/// What `logout` did, for the CLI to render.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LogoutReport {
    pub removed: Vec<String>,
    /// Profiles whose token was revoked server-side.
    pub revoked: Vec<String>,
    /// `(profile, reason)` for revocations that failed — the credential was still removed.
    pub revoke_errors: Vec<(String, String)>,
    /// Profiles named but with nothing stored.
    pub not_found: Vec<String>,
}

/// `zdk auth logout [--all] [--no-revoke]`.
pub async fn logout(
    settings: &Settings,
    store: &SharedStore,
    opts: LogoutOptions,
) -> Result<LogoutReport> {
    let profiles = if opts.all {
        let mut all = store.list_profiles()?;
        if !all.iter().any(|p| p == &settings.profile_name)
            && store.load(&settings.profile_name)?.is_some()
        {
            all.push(settings.profile_name.clone());
        }
        all
    } else {
        vec![settings.profile_name.clone()]
    };

    let mut report = LogoutReport::default();
    let http = http_client(settings.timeout)?;
    for profile in profiles {
        let Some(cred) = store.load(&profile)? else {
            report.not_found.push(profile);
            continue;
        };
        if opts.revoke
            && let Credential::OAuth(token) = &cred
        {
            let base = if profile == settings.profile_name {
                base_url_for(settings, &token.subdomain)
            } else {
                Url::parse(&format!("https://{}.zendesk.com", token.subdomain)).map_err(Into::into)
            };
            let outcome = match (base, bearer_header(&token.access_token)) {
                (Ok(base), Ok(bearer)) => revoke::revoke_current_at(&http, &base, &bearer)
                    .await
                    .map(|_| ()),
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
            match outcome {
                Ok(()) => report.revoked.push(profile.clone()),
                Err(e) => report.revoke_errors.push((profile.clone(), e.to_string())),
            }
        }
        if store.delete(&profile)? {
            report.removed.push(profile);
        } else {
            report.not_found.push(profile);
        }
    }
    Ok(report)
}

/// `zdk auth status`: the provider's description plus the credential details a human wants.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthStatus {
    #[serde(flatten)]
    pub description: AuthDescription,
    /// Agent email (API-token auth).
    pub email: Option<String>,
    pub token_type: Option<String>,
    pub obtained_at: Option<DateTime<Utc>>,
    pub refresh_expires_at: Option<DateTime<Utc>>,
    /// Seconds until the access token expires (negative once expired); `None` = no expiry.
    pub expires_in_secs: Option<i64>,
    pub expired: bool,
    /// The next request would refresh / re-mint first.
    pub refresh_due: bool,
    /// The client secret needed for client-credentials re-mint is available (env or stored).
    pub can_renew: bool,
    /// Days until Zendesk stops creating API tokens (only while positive; API-token auth only).
    pub api_token_days_until_creation_cutoff: Option<i64>,
    /// The countdown line the CLI prints for API-token auth.
    pub deprecation_warning: Option<String>,
}

/// Build [`AuthStatus`] without touching the network. `Err(NotLoggedIn)` when nothing is configured.
pub fn status(settings: &Settings, env: &EnvOverrides, store: &SharedStore) -> Result<AuthStatus> {
    let provider = resolve_provider(settings, env, store.clone())?;
    let description = provider.description();
    let now = Utc::now();
    let mut s = AuthStatus {
        description,
        email: None,
        token_type: None,
        obtained_at: None,
        refresh_expires_at: None,
        expires_in_secs: None,
        expired: false,
        refresh_due: false,
        can_renew: false,
        api_token_days_until_creation_cutoff: None,
        deprecation_warning: None,
    };

    let from_env = s.description.store.as_deref() == Some("env");
    let credential = if from_env {
        env.api_token.as_ref().map(|t| Credential::ApiToken {
            email: settings.profile.email.clone().unwrap_or_default(),
            token: t.clone(),
            subdomain: settings.subdomain.clone().unwrap_or_default(),
        })
    } else {
        store.load(&settings.profile_name)?
    };

    match credential {
        Some(Credential::OAuth(t)) => {
            s.token_type = Some(t.token_type.clone());
            s.obtained_at = Some(t.obtained_at);
            s.refresh_expires_at = t.refresh_expires_at;
            s.expires_in_secs = t.expires_at.map(|e| (e - now).num_seconds());
            s.expired = t.is_expired(now);
            s.refresh_due = s.expired || t.refresh_due(settings.auth.refresh_at_percent, now);
            s.can_renew = match t.grant {
                GrantKind::ClientCredentials => {
                    client_credentials::secret_for(&t, env.client_secret.as_ref()).is_some()
                }
                _ => t.refresh_token.is_some(),
            };
        }
        Some(Credential::ApiToken { email, .. }) => {
            s.email = Some(email);
            let today = now.date_naive();
            let cutoff = api_token::days_until(api_token::DEADLINE_NO_NEW_TOKENS, today);
            s.api_token_days_until_creation_cutoff = (cutoff > 0).then_some(cutoff);
            s.deprecation_warning = Some(api_token::deprecation_warning(today));
        }
        None => {}
    }
    Ok(s)
}

/// `zdk auth refresh [--force]`: refresh (authorization code) or re-mint (client credentials)
/// now, persisting the result. A rotation that cannot be saved is a hard error here.
pub async fn refresh_now(
    settings: &Settings,
    env: &EnvOverrides,
    store: &SharedStore,
    force: bool,
) -> Result<TokenSet> {
    let profile = settings.profile_name.clone();
    let Some(cred) = store.load(&profile)? else {
        return Err(ZdkError::Auth(AuthFailure::NotLoggedIn { profile }));
    };
    let Credential::OAuth(current) = cred else {
        return Err(ZdkError::Usage(format!(
            "profile '{profile}' uses an API token; there is nothing to refresh. Run `zdk auth login` to switch to OAuth"
        )));
    };
    let now = Utc::now();
    let due = current.is_expired(now) || current.refresh_due(settings.auth.refresh_at_percent, now);
    if !force && !due {
        return Ok(current);
    }
    let base = base_url_for(settings, &current.subdomain)?;
    let http = http_client(settings.timeout)?;
    if current.grant == GrantKind::ClientCredentials {
        let secret = client_credentials::secret_for(&current, env.client_secret.as_ref())
            .ok_or_else(|| {
                ZdkError::Auth(AuthFailure::ClientSecretRequired {
                    profile: profile.clone(),
                })
            })?;
        let mut fresh = client_credentials::mint(
            &http,
            &base,
            &current.subdomain,
            &current.client_id,
            &secret,
            &current.scopes,
            client_credentials::original_lifetime_secs(&current),
            now,
        )
        .await?;
        fresh.client_secret.clone_from(&current.client_secret);
        store.save(&profile, &Credential::OAuth(fresh.clone()))?;
        return Ok(fresh);
    }
    let refresher = TokenRefresher::new(http, base, profile, store.clone());
    let fresh = refresher
        .refresh(&current, env.client_secret.as_ref())
        .await?;
    if let Some(msg) = refresher.take_warning() {
        return Err(ZdkError::CredentialStore(msg));
    }
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigFile, GlobalArgs, Paths};
    use crate::store::MemoryStore;

    fn settings(env: &EnvOverrides) -> Settings {
        let paths = Paths {
            config_file: "/t/config.toml".into(),
            config_dir: "/t".into(),
            state_dir: "/t/state".into(),
            cache_dir: "/t/cache".into(),
        };
        Settings::resolve(&GlobalArgs::default(), env, &ConfigFile::default(), &paths).unwrap()
    }

    fn oauth_token(grant: GrantKind) -> TokenSet {
        TokenSet {
            access_token: SecretString::from("acc".to_string()),
            token_type: "bearer".into(),
            scopes: vec!["tickets:read".into()],
            obtained_at: Utc::now(),
            expires_at: Some(Utc::now() + chrono::TimeDelta::minutes(30)),
            refresh_token: Some(SecretString::from("ref".to_string())),
            refresh_expires_at: None,
            grant,
            subdomain: "acme".into(),
            client_id: "zdk".into(),
            client_secret: None,
        }
    }

    #[tokio::test]
    async fn resolve_prefers_env_access_token_then_api_token_then_store() {
        let store: SharedStore = Arc::new(MemoryStore::new());
        store
            .save(
                "default",
                &Credential::OAuth(oauth_token(GrantKind::AuthorizationCode)),
            )
            .unwrap();

        let env = EnvOverrides::from_pairs([
            ("ZENDESK_ACCESS_TOKEN", "static"),
            ("ZENDESK_EMAIL", "me@acme.com"),
            ("ZENDESK_API_TOKEN", "api"),
            ("ZENDESK_SUBDOMAIN", "acme"),
        ]);
        let p = resolve_provider(&settings(&env), &env, store.clone()).unwrap();
        assert_eq!(p.description().grant, GrantKind::StaticToken);
        assert_eq!(
            p.authorization().await.unwrap().to_str().unwrap(),
            "Bearer static"
        );

        let env = EnvOverrides::from_pairs([
            ("ZENDESK_EMAIL", "me@acme.com"),
            ("ZENDESK_API_TOKEN", "api"),
            ("ZENDESK_SUBDOMAIN", "acme"),
        ]);
        let p = resolve_provider(&settings(&env), &env, store.clone()).unwrap();
        let d = p.description();
        assert_eq!(d.grant, GrantKind::ApiToken);
        assert_eq!(d.store.as_deref(), Some("env"));
        assert_eq!(d.subdomain.as_deref(), Some("acme"));

        let env = EnvOverrides::from_pairs([("ZENDESK_API_TOKEN", "api")]);
        let err = resolve_provider(&settings(&env), &env, store.clone()).unwrap_err();
        assert_eq!(err.exit_code(), 10);
        assert!(err.to_string().contains("ZENDESK_EMAIL"));

        let env = EnvOverrides::from_pairs([("ZENDESK_SUBDOMAIN", "acme")]);
        let p = resolve_provider(&settings(&env), &env, store.clone()).unwrap();
        let d = p.description();
        assert_eq!(d.grant, GrantKind::AuthorizationCode);
        assert_eq!(d.store.as_deref(), Some("memory"));
        assert_eq!(d.client_id.as_deref(), Some("zdk"));
        assert!(d.has_refresh_token);
        assert_eq!(p.granted_scopes(), Some(vec!["tickets:read".to_string()]));
        assert_eq!(
            p.authorization().await.unwrap().to_str().unwrap(),
            "Bearer acc"
        );
        assert!(p.invalidate().await.unwrap());
        assert!(p.rotation_error().is_none());
        assert!(!format!("{p:?}").contains("\"acc\""));

        // Base URL is derived from the stored credential when no subdomain is configured.
        let env = EnvOverrides::default();
        assert!(resolve_provider(&settings(&env), &env, store.clone()).is_ok());

        store.delete("default").unwrap();
        let err = resolve_provider(&settings(&env), &env, store).unwrap_err();
        assert_eq!(err.error_code(), "AUTH_NOT_LOGGED_IN");
    }

    #[tokio::test]
    async fn oauth_provider_without_refresh_token_fails_only_once_expired() {
        let store: SharedStore = Arc::new(MemoryStore::new());
        let mut t = oauth_token(GrantKind::AuthorizationCode);
        t.refresh_token = None;
        store
            .save("default", &Credential::OAuth(t.clone()))
            .unwrap();
        let env = EnvOverrides::from_pairs([("ZENDESK_SUBDOMAIN", "acme")]);
        let p = resolve_provider(&settings(&env), &env, store.clone()).unwrap();
        assert!(p.authorization().await.is_ok(), "valid token, nothing due");
        assert!(!p.invalidate().await.unwrap(), "nothing to refresh with");

        t.expires_at = Some(Utc::now() - chrono::TimeDelta::seconds(1));
        store.save("default", &Credential::OAuth(t)).unwrap();
        let p = resolve_provider(&settings(&env), &env, store).unwrap();
        let err = p.authorization().await.unwrap_err();
        assert_eq!(err.error_code(), "AUTH_EXPIRED");
    }

    #[tokio::test]
    async fn client_credentials_without_secret_is_client_secret_required() {
        let store: SharedStore = Arc::new(MemoryStore::new());
        let mut t = oauth_token(GrantKind::ClientCredentials);
        t.refresh_token = None;
        t.expires_at = Some(Utc::now() - chrono::TimeDelta::seconds(1));
        store.save("default", &Credential::OAuth(t)).unwrap();
        let env = EnvOverrides::from_pairs([("ZENDESK_SUBDOMAIN", "acme")]);
        let p = resolve_provider(&settings(&env), &env, store.clone()).unwrap();
        assert!(!p.invalidate().await.unwrap());
        let err = p.authorization().await.unwrap_err();
        assert_eq!(err.error_code(), "AUTH_CLIENT_SECRET_REQUIRED");
        assert_eq!(err.exit_code(), 3);

        let err = refresh_now(&settings(&env), &env, &store, true)
            .await
            .unwrap_err();
        assert_eq!(err.error_code(), "AUTH_CLIENT_SECRET_REQUIRED");
    }

    #[test]
    fn status_reports_credential_details() {
        let store: SharedStore = Arc::new(MemoryStore::new());
        let env = EnvOverrides::from_pairs([("ZENDESK_SUBDOMAIN", "acme")]);
        let err = status(&settings(&env), &env, &store).unwrap_err();
        assert_eq!(err.exit_code(), 3);

        store
            .save(
                "default",
                &Credential::OAuth(oauth_token(GrantKind::AuthorizationCode)),
            )
            .unwrap();
        let s = status(&settings(&env), &env, &store).unwrap();
        assert_eq!(s.description.grant, GrantKind::AuthorizationCode);
        assert!(s.expires_in_secs.unwrap() > 1700);
        assert!(!s.expired && !s.refresh_due && s.can_renew);
        assert!(s.deprecation_warning.is_none());
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["grant"], "authorization_code",
            "description is flattened"
        );
        assert_eq!(json["store"], "memory");

        login_api_token(
            &settings(&env),
            &store,
            "me@acme.com",
            SecretString::from("tok".to_string()),
            None,
        )
        .unwrap();
        let s = status(&settings(&env), &env, &store).unwrap();
        assert_eq!(s.description.grant, GrantKind::ApiToken);
        assert_eq!(s.email.as_deref(), Some("me@acme.com"));
        assert!(s.deprecation_warning.is_some());
        assert!(s.description.api_token_days_remaining.is_some());

        let err = login_api_token(
            &settings(&env),
            &store,
            "not-an-email",
            SecretString::from("tok".to_string()),
            None,
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn refresh_now_is_a_no_op_when_not_due_and_refuses_api_tokens() {
        let store: SharedStore = Arc::new(MemoryStore::new());
        let env = EnvOverrides::from_pairs([("ZENDESK_SUBDOMAIN", "acme")]);
        let s = settings(&env);
        assert_eq!(
            refresh_now(&s, &env, &store, false)
                .await
                .unwrap_err()
                .exit_code(),
            3
        );
        store
            .save(
                "default",
                &Credential::OAuth(oauth_token(GrantKind::AuthorizationCode)),
            )
            .unwrap();
        let t = refresh_now(&s, &env, &store, false).await.unwrap();
        assert_eq!(
            t.access_token.expose_secret(),
            "acc",
            "fresh token returned as-is"
        );
        login_api_token(
            &s,
            &store,
            "me@acme.com",
            SecretString::from("t".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(
            refresh_now(&s, &env, &store, true)
                .await
                .unwrap_err()
                .exit_code(),
            2
        );
    }

    #[tokio::test]
    async fn login_validation_errors_come_before_any_network() {
        let store: SharedStore = Arc::new(MemoryStore::new());
        let env = EnvOverrides::default();
        let s = settings(&env);
        let err = login_authorization_code(&s, &env, &store, LoginOptions::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no subdomain"), "{err}");

        let env = EnvOverrides::from_pairs([("ZENDESK_SUBDOMAIN", "acme")]);
        let s = settings(&env);
        let err = login_authorization_code(&s, &env, &store, LoginOptions::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no OAuth client id"), "{err}");

        let opts = LoginOptions {
            client_id: Some("zdk".into()),
            ..Default::default()
        };
        let err = login_authorization_code(&s, &env, &store, opts)
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), 2, "empty scopes: {err}");

        let opts = LoginOptions {
            client_id: Some("zdk".into()),
            scopes: vec!["tickets:read".into()],
            ..Default::default()
        };
        let err = login_client_credentials(&s, &env, &store, opts)
            .await
            .unwrap_err();
        assert_eq!(err.error_code(), "AUTH_CLIENT_SECRET_REQUIRED");
        assert!(store.list_profiles().unwrap().is_empty());
    }

    #[test]
    fn login_target_honours_base_url_override_only_for_the_configured_subdomain() {
        let env = EnvOverrides::from_pairs([
            ("ZENDESK_SUBDOMAIN", "test"),
            ("ZENDESK_BASE_URL", "http://127.0.0.1:1"),
        ]);
        let s = settings(&env);
        let (sub, base) = login_target(&s, None).unwrap();
        assert_eq!(sub, "test");
        assert_eq!(base.as_str(), "http://127.0.0.1:1/");
        let (sub, base) = login_target(&s, Some("test")).unwrap();
        assert_eq!(
            (sub.as_str(), base.as_str()),
            ("test", "http://127.0.0.1:1/")
        );
        let (sub, base) = login_target(&s, Some("other")).unwrap();
        assert_eq!(
            (sub.as_str(), base.as_str()),
            ("other", "https://other.zendesk.com/")
        );
    }

    #[tokio::test]
    async fn logout_removes_profiles_and_reports() {
        let store: SharedStore = Arc::new(MemoryStore::new());
        let env = EnvOverrides::from_pairs([("ZENDESK_SUBDOMAIN", "acme")]);
        let s = settings(&env);
        let r = logout(&s, &store, LogoutOptions::default()).await.unwrap();
        assert_eq!(r.not_found, vec!["default"]);
        assert!(r.removed.is_empty());

        login_api_token(
            &s,
            &store,
            "me@acme.com",
            SecretString::from("t".to_string()),
            None,
        )
        .unwrap();
        store
            .save(
                "other",
                &Credential::OAuth(oauth_token(GrantKind::AuthorizationCode)),
            )
            .unwrap();
        let r = logout(
            &s,
            &store,
            LogoutOptions {
                revoke: false,
                all: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(r.removed, vec!["default", "other"]);
        assert!(r.revoked.is_empty() && r.revoke_errors.is_empty());
        assert!(store.list_profiles().unwrap().is_empty());
    }
}
