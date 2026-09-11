//! Read-only credential store backed by the environment (`--credential-store env`).
//!
//! Only the legacy API-token pair (`ZENDESK_EMAIL` + `ZENDESK_API_TOKEN`) is expressible as a
//! stored credential; `ZENDESK_ACCESS_TOKEN` bypasses the store entirely and is handled by
//! `auth::resolve_provider`. Every write is refused with a `Config` error (exit 10).

use secrecy::SecretString;

use super::{CredentialStore, StoreKind};
use crate::auth::token::Credential;
use crate::config::EnvOverrides;
use crate::{Result, ZdkError};

/// Credentials taken from the environment; never written.
#[derive(Clone)]
pub struct EnvStore {
    email: Option<String>,
    api_token: Option<SecretString>,
    subdomain: Option<String>,
}

impl std::fmt::Debug for EnvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvStore")
            .field("email", &self.email)
            .field("api_token", &self.api_token.as_ref().map(|_| "[redacted]"))
            .field("subdomain", &self.subdomain)
            .finish()
    }
}

impl EnvStore {
    /// Snapshot the relevant variables from the already-read environment.
    #[must_use]
    pub fn from_env(env: &EnvOverrides) -> Self {
        Self {
            email: env.email.clone(),
            api_token: env.api_token.clone(),
            subdomain: env.subdomain.clone(),
        }
    }

    fn read_only() -> ZdkError {
        ZdkError::Config(
            "credential_store = env is read-only: set ZENDESK_EMAIL/ZENDESK_API_TOKEN (or \
             ZENDESK_ACCESS_TOKEN) in the environment, or choose --credential-store keyring|file"
                .into(),
        )
    }
}

impl CredentialStore for EnvStore {
    fn kind(&self) -> StoreKind {
        StoreKind::Env
    }

    fn load(&self, _profile: &str) -> Result<Option<Credential>> {
        match (&self.email, &self.api_token) {
            (Some(email), Some(token)) => Ok(Some(Credential::ApiToken {
                email: email.clone(),
                token: token.clone(),
                subdomain: self.subdomain.clone().unwrap_or_default(),
            })),
            _ => Ok(None),
        }
    }

    fn save(&self, _profile: &str, _cred: &Credential) -> Result<()> {
        Err(Self::read_only())
    }

    fn delete(&self, _profile: &str) -> Result<bool> {
        Err(Self::read_only())
    }

    fn list_profiles(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_api_token_pair_and_refuses_writes() {
        let env = EnvOverrides::from_pairs([
            ("ZENDESK_EMAIL", "me@acme.com"),
            ("ZENDESK_API_TOKEN", "tok"),
            ("ZENDESK_SUBDOMAIN", "acme"),
        ]);
        let store = EnvStore::from_env(&env);
        assert_eq!(store.kind(), StoreKind::Env);
        let cred = store.load("anything").unwrap().expect("credential");
        assert!(
            matches!(&cred, Credential::ApiToken { email, subdomain, .. }
            if email == "me@acme.com" && subdomain == "acme")
        );
        let err = store.save("p", &cred).unwrap_err();
        assert_eq!(err.exit_code(), 10);
        assert!(err.to_string().contains("read-only"));
        assert_eq!(store.delete("p").unwrap_err().exit_code(), 10);
        assert!(store.list_profiles().unwrap().is_empty());
        assert!(!format!("{store:?}").contains("tok\""));
    }

    #[test]
    fn missing_pair_loads_nothing() {
        let env = EnvOverrides::from_pairs([("ZENDESK_API_TOKEN", "tok")]);
        assert!(EnvStore::from_env(&env).load("p").unwrap().is_none());
        assert!(
            EnvStore::from_env(&EnvOverrides::default())
                .load("p")
                .unwrap()
                .is_none()
        );
    }
}
