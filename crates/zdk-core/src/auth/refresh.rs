//! Refresh-token rotation (PRD §6.6): serialised, persisted before use, never retried with a
//! burned token.
//!
//! Sequence: POST `refresh_token` grant → build the new [`TokenSet`] (carrying scopes,
//! client id/secret and subdomain forward) → `store.save` → hand the fresh set back. When the
//! save fails the fresh tokens are still returned so the running command completes, and the
//! failure is recorded: the provider keeps working, [`TokenRefresher::rotation_error`] reports
//! it, and the CLI turns it into exit 10 at the end ("rotated but could not be saved").

use std::sync::Mutex;

use chrono::Utc;
use secrecy::SecretString;
use url::Url;

use super::oauth;
use super::token::{Credential, TokenSet};
use crate::error::AuthFailure;
use crate::store::SharedStore;
use crate::{Result, ZdkError};

/// Performs and persists refreshes for one profile.
pub struct TokenRefresher {
    http: reqwest::Client,
    base: Url,
    profile: String,
    store: SharedStore,
    gate: tokio::sync::Mutex<()>,
    pending_error: Mutex<Option<String>>,
}

impl std::fmt::Debug for TokenRefresher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRefresher")
            .field("base", &self.base.as_str())
            .field("profile", &self.profile)
            .field("store", &self.store.kind())
            .field("pending_error", &self.rotation_error())
            .finish_non_exhaustive()
    }
}

impl TokenRefresher {
    #[must_use]
    pub fn new(
        http: reqwest::Client,
        base: Url,
        profile: impl Into<String>,
        store: SharedStore,
    ) -> Self {
        Self {
            http,
            base,
            profile: profile.into(),
            store,
            gate: tokio::sync::Mutex::new(()),
            pending_error: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Redeem `current.refresh_token`, persist the rotated set, return it. `runtime_secret`
    /// (`ZENDESK_CLIENT_SECRET` / `--client-secret`) is sent for confidential clients when no
    /// secret was stored; it is used for the POST only and never persisted.
    pub async fn refresh(
        &self,
        current: &TokenSet,
        runtime_secret: Option<&SecretString>,
    ) -> Result<TokenSet> {
        let _serialised = self.gate.lock().await;
        let now = Utc::now();
        let Some(refresh_token) = current.refresh_token.as_ref() else {
            return Err(ZdkError::Auth(AuthFailure::Expired {
                profile: self.profile.clone(),
                detail: "no refresh token was issued for this token".into(),
            }));
        };
        if current.refresh_token_expired(now) {
            return Err(ZdkError::Auth(AuthFailure::Revoked {
                profile: self.profile.clone(),
                detail: format!(
                    "the refresh token expired at {}",
                    current
                        .refresh_expires_at
                        .map(|d| d.to_rfc3339())
                        .unwrap_or_default()
                ),
            }));
        }

        let response = oauth::refresh_at(
            &self.http,
            &self.base,
            &current.client_id,
            current.client_secret.as_ref().or(runtime_secret),
            refresh_token,
            &self.profile,
        )
        .await?;

        let mut fresh = response.into_token_set(
            current.grant,
            &current.subdomain,
            &current.client_id,
            current.client_secret.clone(),
            now,
        );
        if fresh.scopes.is_empty() {
            fresh.scopes.clone_from(&current.scopes);
        }
        // Zendesk rotates: the old refresh token is dead now. If the response carried none,
        // there is nothing to fall back to — leave it `None` rather than reuse the burned one.

        match self
            .store
            .save(&self.profile, &Credential::OAuth(fresh.clone()))
        {
            Ok(()) => {
                self.set_pending(None);
                tracing::debug!(target: "zdk::auth", profile = %self.profile, "refresh token rotated and saved");
            }
            Err(e) => {
                let msg = format!(
                    "the access token for profile '{}' was refreshed, but the rotated refresh token \
                     could not be saved to the {} credential store ({e}). The old refresh token is \
                     now invalid: run `zdk auth login` before the next command.",
                    self.profile,
                    self.store.kind()
                );
                tracing::error!(target: "zdk::auth", "{msg}");
                self.set_pending(Some(msg));
            }
        }
        Ok(fresh)
    }

    fn set_pending(&self, msg: Option<String>) {
        *self.pending_error.lock().expect("refresher poisoned") = msg;
    }

    /// The persist failure from the last rotation, if any (for exit 10 at command end).
    #[must_use]
    pub fn rotation_error(&self) -> Option<String> {
        self.pending_error
            .lock()
            .expect("refresher poisoned")
            .clone()
    }

    /// Like [`rotation_error`](Self::rotation_error) but clears it.
    #[must_use]
    pub fn take_warning(&self) -> Option<String> {
        self.pending_error
            .lock()
            .expect("refresher poisoned")
            .take()
    }
}
