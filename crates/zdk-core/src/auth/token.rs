//! Credential shapes persisted by the credential store.

use chrono::{DateTime, Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use super::GrantKind;

fn serialize_secret<S: serde::Serializer>(s: &SecretString, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(s.expose_secret())
}

// serde's `serialize_with` requires the `&Option<T>` signature.
#[allow(clippy::ref_option)]
fn serialize_opt_secret<S: serde::Serializer>(
    s: &Option<SecretString>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    match s {
        Some(v) => ser.serialize_some(v.expose_secret()),
        None => ser.serialize_none(),
    }
}

/// An OAuth token set as returned by `POST /oauth/tokens`, plus what we need to refresh it.
#[derive(Clone, Serialize, Deserialize)]
pub struct TokenSet {
    #[serde(serialize_with = "serialize_secret")]
    pub access_token: SecretString,
    pub token_type: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub obtained_at: DateTime<Utc>,
    /// `None` for OAuth clients created before 30 April 2026 (no expiry).
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, serialize_with = "serialize_opt_secret")]
    pub refresh_token: Option<SecretString>,
    pub refresh_expires_at: Option<DateTime<Utc>>,
    pub grant: GrantKind,
    pub subdomain: String,
    pub client_id: String,
    /// Only stored when the user opted in with `--store-secret` (client credentials re-mint).
    #[serde(default, serialize_with = "serialize_opt_secret")]
    pub client_secret: Option<SecretString>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"[redacted]")
            .field("token_type", &self.token_type)
            .field("scopes", &self.scopes)
            .field("obtained_at", &self.obtained_at)
            .field("expires_at", &self.expires_at)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("grant", &self.grant)
            .field("subdomain", &self.subdomain)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl TokenSet {
    /// True once `at_percent` % of the access token's lifetime has elapsed.
    #[must_use]
    pub fn refresh_due(&self, at_percent: u8, now: DateTime<Utc>) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        let ttl = expires_at - self.obtained_at;
        if ttl <= Duration::zero() {
            return true;
        }
        let threshold = self.obtained_at + ttl * i32::from(at_percent.min(100)) / 100;
        now >= threshold
    }

    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|e| now >= e)
    }

    #[must_use]
    pub fn refresh_token_expired(&self, now: DateTime<Utc>) -> bool {
        self.refresh_expires_at.is_some_and(|e| now >= e)
    }
}

/// What the credential store holds for one profile.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    OAuth(TokenSet),
    ApiToken {
        email: String,
        #[serde(serialize_with = "serialize_secret")]
        token: SecretString,
        subdomain: String,
    },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OAuth(t) => f.debug_tuple("OAuth").field(t).finish(),
            Self::ApiToken {
                email, subdomain, ..
            } => f
                .debug_struct("ApiToken")
                .field("email", email)
                .field("token", &"[redacted]")
                .field("subdomain", subdomain)
                .finish(),
        }
    }
}

impl Credential {
    #[must_use]
    pub fn subdomain(&self) -> &str {
        match self {
            Self::OAuth(t) => &t.subdomain,
            Self::ApiToken { subdomain, .. } => subdomain,
        }
    }

    #[must_use]
    pub fn grant(&self) -> GrantKind {
        match self {
            Self::OAuth(t) => t.grant,
            Self::ApiToken { .. } => GrantKind::ApiToken,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(obtained: DateTime<Utc>, ttl_secs: i64) -> TokenSet {
        TokenSet {
            access_token: SecretString::from("a".to_string()),
            token_type: "bearer".into(),
            scopes: vec!["tickets:read".into()],
            obtained_at: obtained,
            expires_at: Some(obtained + Duration::seconds(ttl_secs)),
            refresh_token: None,
            refresh_expires_at: None,
            grant: GrantKind::AuthorizationCode,
            subdomain: "acme".into(),
            client_id: "zdk".into(),
            client_secret: None,
        }
    }

    #[test]
    fn refresh_is_due_at_80_percent_of_ttl() {
        let t0 = Utc::now();
        let t = token(t0, 1800);
        assert!(!t.refresh_due(80, t0 + Duration::seconds(1439)));
        assert!(t.refresh_due(80, t0 + Duration::seconds(1441)));
        assert!(!t.is_expired(t0 + Duration::seconds(1799)));
        assert!(t.is_expired(t0 + Duration::seconds(1800)));
    }

    #[test]
    fn tokens_without_expiry_never_need_refresh() {
        let mut t = token(Utc::now(), 1800);
        t.expires_at = None;
        assert!(!t.refresh_due(80, Utc::now() + Duration::days(365)));
        assert!(!t.is_expired(Utc::now() + Duration::days(365)));
    }

    #[test]
    fn debug_output_never_contains_secrets() {
        let t = token(Utc::now(), 60);
        let dbg = format!("{t:?}");
        assert!(!dbg.contains("\"a\""));
        assert!(dbg.contains("[redacted]"));
        let c = Credential::ApiToken {
            email: "e@x".into(),
            token: SecretString::from("s3cret".to_string()),
            subdomain: "acme".into(),
        };
        assert!(!format!("{c:?}").contains("s3cret"));
    }

    #[test]
    fn credential_round_trips_through_json() {
        let t = token(Utc::now(), 60);
        let json = serde_json::to_string(&Credential::OAuth(t)).unwrap();
        let back: Credential = serde_json::from_str(&json).unwrap();
        assert_eq!(back.subdomain(), "acme");
        assert_eq!(back.grant(), GrantKind::AuthorizationCode);
    }
}
