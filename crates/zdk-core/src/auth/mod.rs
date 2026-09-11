//! Authentication: OAuth 2.0 (authorization code + PKCE, client credentials) and legacy API tokens.
//!
//! The HTTP layer only ever talks to an [`AuthProvider`]; concrete providers live in this module's
//! submodules and are selected by `resolve_provider` from the active profile and environment.

pub mod token;

use async_trait::async_trait;
use http::HeaderValue;

use crate::Result;

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
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
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
}

/// A fixed bearer token (`ZENDESK_ACCESS_TOKEN`); never refreshed.
#[derive(Clone)]
pub struct StaticBearer {
    token: secrecy::SecretString,
    profile: String,
}

impl StaticBearer {
    #[must_use]
    pub fn new(token: impl Into<String>, profile: impl Into<String>) -> Self {
        Self {
            token: secrecy::SecretString::from(token.into()),
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
        use secrecy::ExposeSecret;
        let mut value = HeaderValue::from_str(&format!("Bearer {}", self.token.expose_secret()))
            .map_err(|e| {
                crate::ZdkError::Config(format!("access token is not a valid header value: {e}"))
            })?;
        value.set_sensitive(true);
        Ok(value)
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

/// Pick the provider for the active profile: `ZENDESK_ACCESS_TOKEN` → `ZENDESK_EMAIL`+`ZENDESK_API_TOKEN`
/// → the credential store → not logged in. (Phase P3 implements the store-backed OAuth and API-token
/// providers; until then only the static token path exists.)
pub fn resolve_provider(
    settings: &crate::config::Settings,
    env: &crate::config::EnvOverrides,
    store: crate::store::SharedStore,
) -> Result<std::sync::Arc<dyn AuthProvider>> {
    drop(store);
    if let Some(token) = env.access_token.as_ref() {
        use secrecy::ExposeSecret;
        return Ok(std::sync::Arc::new(StaticBearer::new(
            token.expose_secret(),
            settings.profile_name.clone(),
        )));
    }
    Err(crate::ZdkError::Auth(
        crate::error::AuthFailure::NotLoggedIn {
            profile: settings.profile_name.clone(),
        },
    ))
}
