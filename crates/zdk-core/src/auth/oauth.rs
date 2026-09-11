//! `POST /oauth/tokens`: the three grants zdk uses, with `_at(base)` seams for wiremock.
//!
//! Zendesk facts: `client_secret` is optional on `authorization_code` (public PKCE clients),
//! `refresh_token` rotates (the previous access + refresh tokens are invalidated),
//! `client_credentials` never returns a refresh token. Error bodies look like
//! `{"error":"invalid_scope","error_description":"…"}`.

use chrono::{DateTime, TimeDelta, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

use super::GrantKind;
use super::token::TokenSet;
use crate::error::AuthFailure;
use crate::{Result, ZdkError};

/// Path of the token endpoint relative to the instance base URL.
pub const TOKEN_PATH: &str = "oauth/tokens";
/// Path of the authorization page relative to the instance base URL.
pub const AUTHORIZE_PATH: &str = "oauth/authorizations/new";

/// Resolve `rel` against `base`, keeping any path prefix on `base`.
pub fn endpoint(base: &Url, rel: &str) -> Result<Url> {
    let mut b = base.clone();
    if !b.path().ends_with('/') {
        let p = format!("{}/", b.path());
        b.set_path(&p);
    }
    Ok(b.join(rel)?)
}

/// The token endpoint's success body.
#[derive(Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: SecretString,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<SecretString>,
    #[serde(default)]
    pub refresh_token_expires_in: Option<u64>,
}

fn default_token_type() -> String {
    "bearer".into()
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &"[redacted]")
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .field("expires_in", &self.expires_in)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("refresh_token_expires_in", &self.refresh_token_expires_in)
            .finish()
    }
}

impl TokenResponse {
    /// Turn the wire response into what we persist, computing absolute expiries from `now`.
    #[must_use]
    pub fn into_token_set(
        self,
        grant: GrantKind,
        subdomain: &str,
        client_id: &str,
        client_secret: Option<SecretString>,
        now: DateTime<Utc>,
    ) -> TokenSet {
        let after = |secs: u64| {
            let secs = i64::try_from(secs).unwrap_or(i64::MAX);
            TimeDelta::try_seconds(secs).map(|d| now + d)
        };
        TokenSet {
            access_token: self.access_token,
            token_type: self.token_type,
            scopes: self
                .scope
                .as_deref()
                .map(|s| s.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
            obtained_at: now,
            expires_at: self.expires_in.and_then(after),
            refresh_token: self.refresh_token,
            refresh_expires_at: self.refresh_token_expires_in.and_then(after),
            grant,
            subdomain: subdomain.to_string(),
            client_id: client_id.to_string(),
            client_secret,
        }
    }
}

/// Inputs for the `authorization_code` grant.
#[derive(Clone)]
pub struct CodeExchange {
    pub client_id: String,
    /// Sent only when configured (confidential clients).
    pub client_secret: Option<SecretString>,
    pub code: String,
    pub redirect_uri: String,
    pub code_verifier: String,
    /// Space-joined scopes, when the client requires them on exchange.
    pub scope: Option<String>,
    /// Requested access-token lifetime in seconds (300–172 800).
    pub expires_in: Option<u64>,
}

impl std::fmt::Debug for CodeExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeExchange")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[redacted]"),
            )
            .field("code", &"[redacted]")
            .field("redirect_uri", &self.redirect_uri)
            .field("code_verifier", &"[redacted]")
            .field("scope", &self.scope)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// Which grant a request used — decides how `invalid_grant` is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    AuthorizationCode,
    RefreshToken,
    ClientCredentials,
}

/// Exchange an authorization code (+ PKCE verifier) for tokens.
pub async fn exchange_code_at(
    http: &reqwest::Client,
    base: &Url,
    req: &CodeExchange,
) -> Result<TokenResponse> {
    let mut form: Vec<(&str, String)> = vec![
        ("grant_type", "authorization_code".into()),
        ("client_id", req.client_id.clone()),
        ("code", req.code.clone()),
        ("redirect_uri", req.redirect_uri.clone()),
        ("code_verifier", req.code_verifier.clone()),
    ];
    if let Some(secret) = &req.client_secret {
        form.push(("client_secret", secret.expose_secret().to_string()));
    }
    if let Some(scope) = &req.scope {
        form.push(("scope", scope.clone()));
    }
    if let Some(e) = req.expires_in {
        form.push(("expires_in", e.to_string()));
    }
    post_token(
        http,
        base,
        &form,
        Grant::AuthorizationCode,
        req.scope.as_deref(),
        "",
    )
    .await
}

/// Redeem a refresh token. `invalid_grant` here means the refresh token is dead
/// (revoked, rotated elsewhere, or expired) → [`AuthFailure::Revoked`] for `profile`.
pub async fn refresh_at(
    http: &reqwest::Client,
    base: &Url,
    client_id: &str,
    client_secret: Option<&SecretString>,
    refresh_token: &SecretString,
    profile: &str,
) -> Result<TokenResponse> {
    let mut form: Vec<(&str, String)> = vec![
        ("grant_type", "refresh_token".into()),
        ("client_id", client_id.to_string()),
        ("refresh_token", refresh_token.expose_secret().to_string()),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret.expose_secret().to_string()));
    }
    post_token(http, base, &form, Grant::RefreshToken, None, profile).await
}

/// Mint a token with the `client_credentials` grant (confidential clients; no refresh token).
pub async fn client_credentials_at(
    http: &reqwest::Client,
    base: &Url,
    client_id: &str,
    client_secret: &SecretString,
    scopes: &[String],
    expires_in: Option<u64>,
) -> Result<TokenResponse> {
    let scope = scopes.join(" ");
    let mut form: Vec<(&str, String)> = vec![
        ("grant_type", "client_credentials".into()),
        ("client_id", client_id.to_string()),
        ("client_secret", client_secret.expose_secret().to_string()),
        ("scope", scope.clone()),
    ];
    if let Some(e) = expires_in {
        form.push(("expires_in", e.to_string()));
    }
    post_token(
        http,
        base,
        &form,
        Grant::ClientCredentials,
        Some(&scope),
        "",
    )
    .await
}

async fn post_token(
    http: &reqwest::Client,
    base: &Url,
    form: &[(&str, String)],
    grant: Grant,
    requested_scope: Option<&str>,
    profile: &str,
) -> Result<TokenResponse> {
    let url = endpoint(base, TOKEN_PATH)?;
    tracing::debug!(target: "zdk::auth", %url, grant = ?grant, "token request");
    // Encoded by hand: reqwest's `form` feature is not enabled in this workspace. The
    // serializer is dropped before the await so the future stays `Send`.
    let body = {
        let mut encoded = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in form {
            encoded.append_pair(k, v);
        }
        encoded.finish()
    };
    let response = http
        .post(url)
        .header(http::header::ACCEPT, "application/json")
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await?;
    let status = response.status().as_u16();
    let body = response.bytes().await?;
    if (200..300).contains(&status) {
        return serde_json::from_slice::<TokenResponse>(&body).map_err(|e| {
            ZdkError::Auth(AuthFailure::TokenEndpoint {
                status,
                error: "invalid_response".into(),
                description: format!("could not parse the token response: {e}"),
            })
        });
    }
    Err(classify_error(
        status,
        &body,
        grant,
        requested_scope,
        profile,
    ))
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Map a non-2xx token-endpoint response to the right [`AuthFailure`].
fn classify_error(
    status: u16,
    body: &[u8],
    grant: Grant,
    requested_scope: Option<&str>,
    profile: &str,
) -> ZdkError {
    let Ok(parsed) = serde_json::from_slice::<ErrorBody>(body) else {
        let text = String::from_utf8_lossy(body);
        let text = text.trim();
        let truncated: String = text.chars().take(300).collect();
        return ZdkError::Auth(AuthFailure::TokenEndpoint {
            status,
            error: format!("http_{status}"),
            description: if truncated.is_empty() {
                "empty response body".into()
            } else {
                truncated
            },
        });
    };
    let description = parsed.error_description.unwrap_or_default();
    match (parsed.error.as_str(), grant) {
        ("invalid_scope", _) => ZdkError::Auth(AuthFailure::InvalidScope {
            scope: requested_scope
                .filter(|s| !s.is_empty())
                .unwrap_or("(none)")
                .to_string(),
            detail: if description.is_empty() {
                "the token endpoint returned invalid_scope".into()
            } else {
                description
            },
        }),
        ("invalid_grant", Grant::AuthorizationCode) => ZdkError::Auth(AuthFailure::CodeExpired),
        ("invalid_grant", Grant::RefreshToken) => ZdkError::Auth(AuthFailure::Revoked {
            profile: profile.to_string(),
            detail: if description.is_empty() {
                "invalid_grant".into()
            } else {
                description
            },
        }),
        (error, _) => ZdkError::Auth(AuthFailure::TokenEndpoint {
            status,
            error: error.to_string(),
            description,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_keeps_base_path_prefix() {
        let base = Url::parse("https://acme.zendesk.com").unwrap();
        assert_eq!(
            endpoint(&base, TOKEN_PATH).unwrap().as_str(),
            "https://acme.zendesk.com/oauth/tokens"
        );
        let base = Url::parse("http://127.0.0.1:9/prefix").unwrap();
        assert_eq!(
            endpoint(&base, AUTHORIZE_PATH).unwrap().as_str(),
            "http://127.0.0.1:9/prefix/oauth/authorizations/new"
        );
    }

    #[test]
    fn into_token_set_computes_expiries_and_splits_scopes() {
        let now = Utc::now();
        let r: TokenResponse = serde_json::from_str(
            r#"{"access_token":"a","token_type":"bearer","scope":"tickets:read users:read","expires_in":1800,"refresh_token":"r","refresh_token_expires_in":2592000}"#,
        )
        .unwrap();
        let t = r.into_token_set(GrantKind::AuthorizationCode, "acme", "zdk", None, now);
        assert_eq!(t.scopes, vec!["tickets:read", "users:read"]);
        assert_eq!(t.obtained_at, now);
        assert_eq!(t.expires_at, Some(now + TimeDelta::seconds(1800)));
        assert_eq!(t.refresh_expires_at, Some(now + TimeDelta::days(30)));
        assert!(t.refresh_token.is_some());
        assert_eq!(t.subdomain, "acme");
        assert_eq!(t.client_id, "zdk");

        // Older clients: no expiry, no refresh expiry; token_type defaults.
        let r: TokenResponse = serde_json::from_str(r#"{"access_token":"a"}"#).unwrap();
        let t = r.into_token_set(GrantKind::ClientCredentials, "acme", "zdk", None, now);
        assert_eq!(t.token_type, "bearer");
        assert!(t.expires_at.is_none());
        assert!(t.refresh_token.is_none());
        assert!(t.scopes.is_empty());
        assert!(!format!("{t:?}").contains("\"a\""));
    }

    #[test]
    fn error_classification() {
        let body = br#"{"error":"invalid_scope","error_description":"The requested scope is invalid, unknown, or malformed."}"#;
        match classify_error(
            400,
            body,
            Grant::ClientCredentials,
            Some("tickets:read foo:read"),
            "",
        ) {
            ZdkError::Auth(AuthFailure::InvalidScope { scope, detail }) => {
                assert_eq!(scope, "tickets:read foo:read");
                assert!(detail.contains("malformed"));
            }
            other => panic!("{other:?}"),
        }
        let body = br#"{"error":"invalid_grant","error_description":"expired"}"#;
        assert!(matches!(
            classify_error(400, body, Grant::AuthorizationCode, None, ""),
            ZdkError::Auth(AuthFailure::CodeExpired)
        ));
        match classify_error(400, body, Grant::RefreshToken, None, "work") {
            ZdkError::Auth(AuthFailure::Revoked { profile, detail }) => {
                assert_eq!(profile, "work");
                assert_eq!(detail, "expired");
            }
            other => panic!("{other:?}"),
        }
        let body = br#"{"error":"invalid_client","error_description":"bad secret"}"#;
        match classify_error(401, body, Grant::ClientCredentials, None, "") {
            ZdkError::Auth(AuthFailure::TokenEndpoint {
                status,
                error,
                description,
            }) => {
                assert_eq!(status, 401);
                assert_eq!(error, "invalid_client");
                assert_eq!(description, "bad secret");
            }
            other => panic!("{other:?}"),
        }
        match classify_error(
            502,
            b"<html>Bad Gateway</html>",
            Grant::RefreshToken,
            None,
            "",
        ) {
            ZdkError::Auth(AuthFailure::TokenEndpoint { status, error, .. }) => {
                assert_eq!(status, 502);
                assert_eq!(error, "http_502");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            classify_error(400, body, Grant::AuthorizationCode, None, "").exit_code(),
            3
        );
    }

    #[test]
    fn debug_impls_redact() {
        let c = CodeExchange {
            client_id: "zdk".into(),
            client_secret: Some(SecretString::from("s3cret".to_string())),
            code: "c0de".into(),
            redirect_uri: "http://127.0.0.1:1/callback".into(),
            code_verifier: "v3rifier".into(),
            scope: None,
            expires_in: None,
        };
        let d = format!("{c:?}");
        assert!(!d.contains("s3cret") && !d.contains("c0de") && !d.contains("v3rifier"));
    }
}
