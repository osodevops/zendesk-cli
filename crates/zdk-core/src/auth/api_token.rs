//! Legacy API-token auth (`Basic {email}/token:{token}`) with the end-of-life countdown.
//!
//! Zendesk stops issuing API tokens on 27 October 2026 and disables every token on
//! 30 April 2027 (PRD §6.3). Each process using an API token prints one stderr warning.

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{NaiveDate, Utc};
use http::HeaderValue;
use secrecy::{ExposeSecret, SecretString};

use super::{AuthDescription, AuthProvider, GrantKind};
use crate::{Result, ZdkError};

const fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    match NaiveDate::from_ymd_opt(y, m, d) {
        Some(v) => v,
        None => panic!("invalid deadline date"),
    }
}

/// After this date Zendesk no longer creates new API tokens.
pub const DEADLINE_NO_NEW_TOKENS: NaiveDate = date(2026, 10, 27);
/// After this date every API token stops working.
pub const DEADLINE_ALL_TOKENS_DEAD: NaiveDate = date(2027, 4, 30);

/// Whole days from `today` to `deadline` (negative once past).
#[must_use]
pub fn days_until(deadline: NaiveDate, today: NaiveDate) -> i64 {
    (deadline - today).num_days()
}

fn fmt_date(d: NaiveDate) -> String {
    d.format("%-d %b %Y").to_string()
}

/// The one-line stderr warning for `today`. Mentions the creation cut-off only while it is
/// still ahead; always states the final deadline with the days remaining (or elapsed).
#[must_use]
pub fn deprecation_warning(today: NaiveDate) -> String {
    let dead_in = days_until(DEADLINE_ALL_TOKENS_DEAD, today);
    let no_new_in = days_until(DEADLINE_NO_NEW_TOKENS, today);
    let switch = "Run `zdk auth login` to switch to OAuth.";
    if dead_in <= 0 {
        return format!(
            "warning: API token auth is deprecated: all API tokens stopped working on {} ({} days ago). {switch}",
            fmt_date(DEADLINE_ALL_TOKENS_DEAD),
            -dead_in
        );
    }
    if no_new_in > 0 {
        format!(
            "warning: API token auth is deprecated. New tokens cannot be created after {} ({no_new_in} days); all tokens stop working on {} ({dead_in} days). {switch}",
            fmt_date(DEADLINE_NO_NEW_TOKENS),
            fmt_date(DEADLINE_ALL_TOKENS_DEAD),
        )
    } else {
        format!(
            "warning: API token auth is deprecated. Zendesk no longer issues new API tokens; all tokens stop working on {} ({dead_in} days). {switch}",
            fmt_date(DEADLINE_ALL_TOKENS_DEAD),
        )
    }
}

/// `Authorization: Basic base64("{email}/token:{token}")`, marked sensitive.
pub fn basic_header(email: &str, token: &SecretString) -> Result<HeaderValue> {
    let raw = format!("{email}/token:{}", token.expose_secret());
    let mut value = HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(raw)))
        .map_err(|e| ZdkError::Config(format!("API token is not a valid header value: {e}")))?;
    value.set_sensitive(true);
    Ok(value)
}

/// [`AuthProvider`] for legacy API tokens.
pub struct ApiTokenProvider {
    profile: String,
    email: String,
    token: SecretString,
    subdomain: String,
    suppress_warning: bool,
    warned: AtomicBool,
    store: Option<String>,
}

impl std::fmt::Debug for ApiTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiTokenProvider")
            .field("profile", &self.profile)
            .field("email", &self.email)
            .field("token", &"[redacted]")
            .field("subdomain", &self.subdomain)
            .field("suppress_warning", &self.suppress_warning)
            .field("warned", &self.warned)
            .field("store", &self.store)
            .finish()
    }
}

impl ApiTokenProvider {
    /// `store` names where the credential came from (`env`, `keyring`, `file`) for status.
    #[must_use]
    pub fn new(
        profile: impl Into<String>,
        email: impl Into<String>,
        token: SecretString,
        subdomain: impl Into<String>,
        suppress_warning: bool,
        store: Option<String>,
    ) -> Self {
        Self {
            profile: profile.into(),
            email: email.into(),
            token,
            subdomain: subdomain.into(),
            suppress_warning,
            warned: AtomicBool::new(false),
            store,
        }
    }

    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }

    /// Emit the countdown once per process (unless suppressed). Returns the text when it fired.
    pub fn warn_once(&self) -> Option<String> {
        if self.suppress_warning || self.warned.swap(true, Ordering::SeqCst) {
            return None;
        }
        let text = deprecation_warning(Utc::now().date_naive());
        crate::output::warn(&text);
        Some(text)
    }
}

#[async_trait]
impl AuthProvider for ApiTokenProvider {
    async fn authorization(&self) -> Result<HeaderValue> {
        self.warn_once();
        basic_header(&self.email, &self.token)
    }

    async fn invalidate(&self) -> Result<bool> {
        Ok(false)
    }

    fn granted_scopes(&self) -> Option<Vec<String>> {
        None
    }

    fn description(&self) -> AuthDescription {
        AuthDescription {
            grant: GrantKind::ApiToken,
            profile: self.profile.clone(),
            subdomain: Some(self.subdomain.clone()).filter(|s| !s.is_empty()),
            client_id: None,
            scopes: vec![],
            expires_at: None,
            has_refresh_token: false,
            store: self.store.clone(),
            api_token_days_remaining: Some(days_until(
                DEADLINE_ALL_TOKENS_DEAD,
                Utc::now().date_naive(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_header_matches_zendesk_format() {
        let h = basic_header("me@acme.com", &SecretString::from("s3cr3t".to_string())).unwrap();
        // base64("me@acme.com/token:s3cr3t")
        assert_eq!(
            h.to_str().unwrap(),
            "Basic bWVAYWNtZS5jb20vdG9rZW46czNjcjN0"
        );
        assert!(h.is_sensitive());
    }

    #[test]
    fn deadline_maths_at_fixed_dates() {
        let today = date(2026, 9, 11);
        assert_eq!(days_until(DEADLINE_NO_NEW_TOKENS, today), 46);
        assert_eq!(days_until(DEADLINE_ALL_TOKENS_DEAD, today), 231);
        let w = deprecation_warning(today);
        assert!(
            w.starts_with("warning: API token auth is deprecated"),
            "{w}"
        );
        assert!(w.contains("27 Oct 2026 (46 days)"), "{w}");
        assert!(w.contains("30 Apr 2027 (231 days)"), "{w}");
        assert!(w.contains("zdk auth login"), "{w}");

        // After the creation cut-off: only the final deadline is counted down.
        let w = deprecation_warning(date(2026, 11, 1));
        assert!(!w.contains("27 Oct 2026"), "{w}");
        assert!(w.contains("no longer issues"), "{w}");
        assert!(w.contains("30 Apr 2027 (180 days)"), "{w}");

        // On the cut-off day itself it is no longer "in the future".
        assert!(!deprecation_warning(DEADLINE_NO_NEW_TOKENS).contains("27 Oct 2026"));

        // Past the final deadline.
        let w = deprecation_warning(date(2027, 5, 10));
        assert!(
            w.contains("stopped working on 30 Apr 2027 (10 days ago)"),
            "{w}"
        );
        assert_eq!(days_until(DEADLINE_ALL_TOKENS_DEAD, date(2027, 5, 10)), -10);
    }

    #[tokio::test]
    async fn provider_warns_once_and_describes_itself() {
        crate::output::set_quiet(true); // keep the test log clean; warn() still records
        let p = ApiTokenProvider::new(
            "default",
            "me@acme.com",
            SecretString::from("tok".to_string()),
            "acme",
            false,
            Some("keyring".into()),
        );
        assert!(p.warn_once().is_some());
        assert!(p.warn_once().is_none(), "second call is silent");
        let h = p.authorization().await.unwrap();
        assert!(h.to_str().unwrap().starts_with("Basic "));
        assert!(!p.invalidate().await.unwrap());
        assert!(p.granted_scopes().is_none());
        let d = p.description();
        assert_eq!(d.grant, GrantKind::ApiToken);
        assert_eq!(d.subdomain.as_deref(), Some("acme"));
        assert_eq!(d.store.as_deref(), Some("keyring"));
        assert!(d.api_token_days_remaining.is_some());
        assert!(!format!("{p:?}").contains("tok\""));

        let quiet = ApiTokenProvider::new(
            "default",
            "me@acme.com",
            SecretString::from("tok".to_string()),
            "acme",
            true,
            None,
        );
        assert!(quiet.warn_once().is_none(), "suppressed");
        crate::output::set_quiet(false);
    }
}
