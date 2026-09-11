//! Client-credentials grant (PRD §6.2): confidential clients, no refresh token — a fresh
//! token is minted from the client secret whenever the current one is expired or due.

use chrono::{DateTime, Utc};
use secrecy::SecretString;
use url::Url;

use super::GrantKind;
use super::oauth;
use super::token::TokenSet;
use crate::Result;

/// Mint a token. The secret is not stored in the returned set (see [`TokenSet::client_secret`]);
/// the caller decides whether to keep it (`--store-secret`).
// Every argument is a distinct input of the grant; bundling them would only add a struct.
#[allow(clippy::too_many_arguments)]
pub async fn mint(
    http: &reqwest::Client,
    base: &Url,
    subdomain: &str,
    client_id: &str,
    client_secret: &SecretString,
    scopes: &[String],
    expires_in: Option<u64>,
    now: DateTime<Utc>,
) -> Result<TokenSet> {
    let response =
        oauth::client_credentials_at(http, base, client_id, client_secret, scopes, expires_in)
            .await?;
    let mut token = response.into_token_set(
        GrantKind::ClientCredentials,
        subdomain,
        client_id,
        None,
        now,
    );
    // Zendesk never issues a refresh token for this grant; never keep one by accident.
    token.refresh_token = None;
    token.refresh_expires_at = None;
    if token.scopes.is_empty() {
        token.scopes = scopes.to_vec();
    }
    Ok(token)
}

/// Re-mint policy: expired, or past the pre-emptive threshold (same rule as refresh).
#[must_use]
pub fn remint_due(token: &TokenSet, refresh_at_percent: u8, now: DateTime<Utc>) -> bool {
    token.is_expired(now) || token.refresh_due(refresh_at_percent, now)
}

/// The secret to re-mint with: the environment/CLI value beats the stored one.
#[must_use]
pub fn secret_for(token: &TokenSet, runtime_secret: Option<&SecretString>) -> Option<SecretString> {
    runtime_secret
        .cloned()
        .or_else(|| token.client_secret.clone())
}

/// The lifetime to request on re-mint so the new token matches the original one.
#[must_use]
pub fn original_lifetime_secs(token: &TokenSet) -> Option<u64> {
    let ttl = (token.expires_at? - token.obtained_at).num_seconds();
    u64::try_from(ttl).ok().filter(|s| *s > 0)
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use secrecy::ExposeSecret;

    use super::*;

    fn token(ttl: Option<i64>, secret: Option<&str>) -> TokenSet {
        let now = Utc::now();
        TokenSet {
            access_token: SecretString::from("a".to_string()),
            token_type: "bearer".into(),
            scopes: vec![],
            obtained_at: now,
            expires_at: ttl.map(|s| now + TimeDelta::seconds(s)),
            refresh_token: None,
            refresh_expires_at: None,
            grant: GrantKind::ClientCredentials,
            subdomain: "acme".into(),
            client_id: "zdk".into(),
            client_secret: secret.map(|s| SecretString::from(s.to_string())),
        }
    }

    #[test]
    fn remint_policy_and_secret_precedence() {
        let t = token(Some(1800), Some("stored"));
        let now = t.obtained_at;
        assert!(!remint_due(&t, 80, now + TimeDelta::seconds(600)));
        assert!(remint_due(&t, 80, now + TimeDelta::seconds(1500)));
        assert!(remint_due(&t, 80, now + TimeDelta::seconds(1800)));
        assert!(!remint_due(
            &token(None, None),
            80,
            now + TimeDelta::days(1)
        ));

        let env = SecretString::from("env".to_string());
        assert_eq!(secret_for(&t, Some(&env)).unwrap().expose_secret(), "env");
        assert_eq!(secret_for(&t, None).unwrap().expose_secret(), "stored");
        assert!(secret_for(&token(None, None), None).is_none());
        assert_eq!(original_lifetime_secs(&t), Some(1800));
        assert_eq!(original_lifetime_secs(&token(None, None)), None);
    }
}
