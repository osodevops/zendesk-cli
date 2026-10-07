//! Refresh-token rotation (PRD §6.6): serialised, persisted before use, never retried with a
//! burned token.
//!
//! Sequence: POST `refresh_token` grant → build the new [`TokenSet`] (carrying scopes,
//! client id/secret and subdomain forward) → `store.save` → hand the fresh set back. When the
//! save fails the fresh tokens are still returned so the running command completes, and the
//! failure is recorded: the provider keeps working, [`TokenRefresher::rotation_error`] reports
//! it, and the CLI turns it into exit 10 at the end ("rotated but could not be saved").

use std::{path::PathBuf, sync::Mutex, time::Duration};

use chrono::Utc;
use secrecy::{ExposeSecret, SecretString};
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
    lock_path: Option<PathBuf>,
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
            lock_path: None,
        }
    }

    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    #[must_use]
    pub fn with_lock_path(mut self, path: Option<PathBuf>) -> Self {
        self.lock_path = path;
        self
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
        // Keep the OS lock through persistence. Closing the handle releases it, including
        // cancellation or process termination; never unlink a lock file with waiters.
        let _process_lock = match &self.lock_path {
            Some(path) => Some(acquire_lock(path).await?),
            None => None,
        };
        let now = Utc::now();
        let Some(Credential::OAuth(stored)) = self.store.load(&self.profile)? else {
            return Err(ZdkError::Auth(AuthFailure::NotLoggedIn {
                profile: self.profile.clone(),
            }));
        };
        if stored.client_id != current.client_id
            || stored.subdomain != current.subdomain
            || stored.grant != current.grant
        {
            return Err(ZdkError::Auth(AuthFailure::Revoked {
                profile: self.profile.clone(),
                detail: "the stored OAuth profile changed while this command was running".into(),
            }));
        }
        let changed = stored.access_token.expose_secret() != current.access_token.expose_secret()
            || stored
                .refresh_token
                .as_ref()
                .map(ExposeSecret::expose_secret)
                != current
                    .refresh_token
                    .as_ref()
                    .map(ExposeSecret::expose_secret);
        // A newer saved generation supersedes a stale process cache, including a forced
        // refresh following a 401. Do not roll back an unsaved in-process rotation.
        let current = if changed && stored.obtained_at >= current.obtained_at {
            if !stored.is_expired(now) {
                return Ok(stored);
            }
            &stored
        } else {
            current
        };
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

async fn acquire_lock(path: &std::path::Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    let map_error = |e: std::io::Error| {
        ZdkError::CredentialStore(format!("cannot lock OAuth refresh state: {e}"))
    };
    if let Some(parent) = path.parent() {
        crate::util::fs::ensure_dir(parent).map_err(map_error)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(map_error)?;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(()),
                Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(e) => return Err(map_error(e)),
            }
        }
    })
    .await
    .map_err(|_| {
        ZdkError::CredentialStore(
            "timed out waiting for another OAuth refresh; retry the command".into(),
        )
    })??;
    Ok(file)
}

pub(super) fn lock_path(
    settings: &crate::config::Settings,
    store: &SharedStore,
) -> Option<PathBuf> {
    use crate::store::StoreKind;
    use sha2::{Digest, Sha256};
    match store.kind() {
        // The encrypted file contains every profile: serialize its refresh writes together.
        StoreKind::File => Some(settings.paths.config_dir.join("oauth-refresh.lock")),
        StoreKind::Keyring => Some(settings.paths.state_dir.join("auth-locks").join(format!(
            "{:x}.lock",
            Sha256::digest(settings.profile_name.as_bytes())
        ))),
        StoreKind::Env | StoreKind::Memory => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn lock_wait_is_bounded_and_the_handle_releases_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refresh.lock");
        let held = acquire_lock(&path).await.unwrap();
        let error = acquire_lock(&path).await.unwrap_err();
        assert_eq!(error.error_code(), "CREDENTIAL_STORE");
        assert!(error.to_string().contains("timed out"));
        drop(held);
        let _next = acquire_lock(&path).await.unwrap();
        assert!(path.exists(), "keep the same lock inode for future waiters");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn lock_failure_is_a_store_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            acquire_lock(dir.path()).await.unwrap_err().error_code(),
            "CREDENTIAL_STORE"
        );
    }
}
