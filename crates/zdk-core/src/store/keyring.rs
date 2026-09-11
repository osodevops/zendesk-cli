//! OS keyring store: macOS Keychain, Windows Credential Manager, Linux Secret Service
//! (via `keyring` 4.2 with the `v1` feature, which auto-selects the platform store).
//!
//! Layout: service `zendesk-cli`, one entry per profile (user = profile name) holding the JSON
//! [`Credential`], plus an index entry (user `profile-index`) holding a JSON array of profile
//! names, because no platform store can enumerate entries portably.
//!
//! The `keyring::Entry` calls sit behind the [`SecretStore`] trait so every test runs against
//! [`MemorySecretStore`] and never touches a real keychain.

use std::fmt;
use std::sync::Mutex;

use super::{CredentialStore, StoreKind};
use crate::auth::token::Credential;
use crate::{Result, ZdkError};

/// Keychain service name for every entry written by `zdk`.
pub const SERVICE_NAME: &str = "zendesk-cli";
/// Entry that holds the JSON array of profile names.
pub const INDEX_USER: &str = "profile-index";
/// Entry read by the `auto` probe; never written.
pub const PROBE_USER: &str = "probe";

/// Windows Credential Manager caps a blob at 2560 bytes; we keep well under it
/// (a unit test asserts the largest realistic credential serialises below this).
pub const MAX_CREDENTIAL_BYTES: usize = 2048;

/// The minimal surface the store logic needs from a secret backend.
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// `Ok(None)` when no entry exists under `user`.
    fn get_password(&self, user: &str) -> Result<Option<String>>;
    /// Update in place (never delete + recreate: that discards the macOS ACL).
    fn set_password(&self, user: &str, value: &str) -> Result<()>;
    /// Whether an entry existed before the call.
    fn delete(&self, user: &str) -> Result<bool>;
}

/// The real platform keyring.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsKeyring;

impl OsKeyring {
    fn entry(user: &str) -> Result<::keyring::Entry> {
        ::keyring::Entry::new(SERVICE_NAME, user).map_err(|e| map_error(&e, "open"))
    }
}

/// Translate a keyring failure into a `CredentialStore` error with actionable text.
fn map_error(e: &::keyring::Error, what: &str) -> ZdkError {
    use ::keyring::Error;
    let hint = "use --credential-store file (or ZENDESK_CREDENTIAL_STORE=file) to keep \
                credentials in an encrypted file instead";
    match e {
        Error::NoStorageAccess(inner) => ZdkError::CredentialStore(format!(
            "cannot {what} the OS keyring entry: storage is not accessible ({inner}); {hint}"
        )),
        Error::PlatformFailure(inner) => ZdkError::CredentialStore(format!(
            "cannot {what} the OS keyring entry: the platform store failed ({inner}); {hint}"
        )),
        Error::NoDefaultStore => ZdkError::CredentialStore(format!(
            "cannot {what} the OS keyring entry: no keyring is available on this system \
             (no Secret Service / Keychain / Credential Manager); {hint}"
        )),
        Error::TooLong(attr, limit) => ZdkError::CredentialStore(format!(
            "cannot {what} the OS keyring entry: '{attr}' exceeds the platform limit of {limit}"
        )),
        other => ZdkError::CredentialStore(format!(
            "cannot {what} the OS keyring entry: {other}; {hint}"
        )),
    }
}

impl SecretStore for OsKeyring {
    fn get_password(&self, user: &str) -> Result<Option<String>> {
        match Self::entry(user)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(::keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(&e, "read")),
        }
    }

    fn set_password(&self, user: &str, value: &str) -> Result<()> {
        Self::entry(user)?
            .set_password(value)
            .map_err(|e| map_error(&e, "write"))
    }

    fn delete(&self, user: &str) -> Result<bool> {
        match Self::entry(user)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(::keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(map_error(&e, "delete")),
        }
    }
}

/// Is the platform keyring usable right now? Used by the `auto` selector.
///
/// `keyring::Entry::store_status()` reports whether the platform store initialised (it fails
/// on Linux without a Secret Service / session bus); a `get_password` on a sentinel entry then
/// exercises the store itself (a locked Keychain or an unreachable daemon surfaces here as
/// `NoStorageAccess` / `PlatformFailure`). Both map to `ZdkError::CredentialStore`.
pub fn probe() -> Result<()> {
    if let Err(e) = ::keyring::Entry::store_status() {
        return Err(map_error(e, "initialise"));
    }
    probe_with(&OsKeyring)
}

/// [`probe`] against an injected backend.
pub fn probe_with(backend: &dyn SecretStore) -> Result<()> {
    backend.get_password(PROBE_USER).map(|_| ())
}

/// The keyring-backed [`CredentialStore`].
#[derive(Debug)]
pub struct KeyringStore {
    backend: Box<dyn SecretStore>,
}

impl KeyringStore {
    /// Use the platform keyring.
    #[must_use]
    pub fn os() -> Self {
        Self::with_backend(Box::new(OsKeyring))
    }

    /// Use any backend (tests use [`MemorySecretStore`]).
    #[must_use]
    pub fn with_backend(backend: Box<dyn SecretStore>) -> Self {
        Self { backend }
    }

    fn check_profile(profile: &str) -> Result<()> {
        if profile.is_empty() || profile == INDEX_USER || profile == PROBE_USER {
            return Err(ZdkError::Config(format!(
                "'{profile}' is not a usable profile name for the keyring store"
            )));
        }
        Ok(())
    }

    fn read_index(&self) -> Result<Vec<String>> {
        Ok(self
            .backend
            .get_password(INDEX_USER)?
            .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
            .unwrap_or_default())
    }

    fn write_index(&self, profiles: &[String]) -> Result<()> {
        let json = serde_json::to_string(profiles)?;
        self.backend.set_password(INDEX_USER, &json)
    }
}

impl CredentialStore for KeyringStore {
    fn kind(&self) -> StoreKind {
        StoreKind::Keyring
    }

    fn load(&self, profile: &str) -> Result<Option<Credential>> {
        Self::check_profile(profile)?;
        let Some(json) = self.backend.get_password(profile)? else {
            return Ok(None);
        };
        serde_json::from_str(&json).map(Some).map_err(|e| {
            ZdkError::CredentialStore(format!(
                "stored credential for profile '{profile}' is unreadable ({e}); run `zdk auth login` again"
            ))
        })
    }

    fn save(&self, profile: &str, cred: &Credential) -> Result<()> {
        Self::check_profile(profile)?;
        let json = serde_json::to_string(cred)?;
        if json.len() > MAX_CREDENTIAL_BYTES {
            return Err(ZdkError::CredentialStore(format!(
                "credential for profile '{profile}' is {} bytes, above the {MAX_CREDENTIAL_BYTES}-byte keyring limit",
                json.len()
            )));
        }
        self.backend.set_password(profile, &json)?;
        let mut index = self.read_index()?;
        if !index.iter().any(|p| p == profile) {
            index.push(profile.to_string());
            self.write_index(&index)?;
        }
        Ok(())
    }

    fn delete(&self, profile: &str) -> Result<bool> {
        Self::check_profile(profile)?;
        let existed = self.backend.delete(profile)?;
        let mut index = self.read_index()?;
        let before = index.len();
        index.retain(|p| p != profile);
        if index.len() != before {
            self.write_index(&index)?;
        }
        Ok(existed)
    }

    fn list_profiles(&self) -> Result<Vec<String>> {
        self.read_index()
    }
}

/// In-memory [`SecretStore`] for tests: a map plus an optional injected failure.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    entries: Mutex<std::collections::BTreeMap<String, String>>,
    fail_with: Mutex<Option<String>>,
}

impl MemorySecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make every call fail with a `CredentialStore` error carrying `message` (`None` clears).
    pub fn set_failure(&self, message: Option<&str>) {
        *self.fail_with.lock().expect("fake keyring poisoned") = message.map(str::to_string);
    }

    /// Entry names currently held (sorted).
    #[must_use]
    pub fn users(&self) -> Vec<String> {
        self.entries
            .lock()
            .expect("fake keyring poisoned")
            .keys()
            .cloned()
            .collect()
    }

    fn check(&self) -> Result<()> {
        match self
            .fail_with
            .lock()
            .expect("fake keyring poisoned")
            .as_ref()
        {
            Some(msg) => Err(ZdkError::CredentialStore(msg.clone())),
            None => Ok(()),
        }
    }
}

impl SecretStore for MemorySecretStore {
    fn get_password(&self, user: &str) -> Result<Option<String>> {
        self.check()?;
        Ok(self
            .entries
            .lock()
            .expect("fake keyring poisoned")
            .get(user)
            .cloned())
    }

    fn set_password(&self, user: &str, value: &str) -> Result<()> {
        self.check()?;
        self.entries
            .lock()
            .expect("fake keyring poisoned")
            .insert(user.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, user: &str) -> Result<bool> {
        self.check()?;
        Ok(self
            .entries
            .lock()
            .expect("fake keyring poisoned")
            .remove(user)
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use secrecy::SecretString;

    use super::*;
    use crate::auth::GrantKind;
    use crate::auth::token::TokenSet;

    fn store() -> KeyringStore {
        KeyringStore::with_backend(Box::new(MemorySecretStore::new()))
    }

    fn oauth(sub: &str) -> Credential {
        Credential::OAuth(TokenSet {
            access_token: SecretString::from("a".repeat(40)),
            token_type: "bearer".into(),
            scopes: vec!["tickets:read".into()],
            obtained_at: Utc::now(),
            expires_at: Some(Utc::now() + Duration::minutes(30)),
            refresh_token: Some(SecretString::from("r".repeat(40))),
            refresh_expires_at: Some(Utc::now() + Duration::days(30)),
            grant: GrantKind::AuthorizationCode,
            subdomain: sub.into(),
            client_id: "zdk".into(),
            client_secret: None,
        })
    }

    #[test]
    fn round_trip_maintains_index_and_updates_in_place() {
        let backend = Box::new(MemorySecretStore::new());
        let s = KeyringStore::with_backend(backend);
        assert!(s.load("work").unwrap().is_none());
        assert!(s.list_profiles().unwrap().is_empty());

        s.save("work", &oauth("acme")).unwrap();
        s.save("work", &oauth("acme2")).unwrap();
        s.save("home", &oauth("home")).unwrap();
        assert_eq!(s.list_profiles().unwrap(), vec!["work", "home"]);
        assert_eq!(s.load("work").unwrap().unwrap().subdomain(), "acme2");

        assert!(s.delete("work").unwrap());
        assert!(!s.delete("work").unwrap());
        assert_eq!(s.list_profiles().unwrap(), vec!["home"]);
        assert!(s.load("work").unwrap().is_none());
    }

    #[test]
    fn reserved_names_are_rejected() {
        let s = store();
        assert_eq!(s.save(INDEX_USER, &oauth("x")).unwrap_err().exit_code(), 10);
        assert_eq!(s.load("").unwrap_err().exit_code(), 10);
    }

    #[test]
    fn unreadable_entry_is_a_store_error() {
        let backend = MemorySecretStore::new();
        backend.set_password("work", "not json").unwrap();
        let s = KeyringStore::with_backend(Box::new(backend));
        let err = s.load("work").unwrap_err();
        assert!(matches!(err, ZdkError::CredentialStore(_)));
        assert!(err.to_string().contains("zdk auth login"));
    }

    #[test]
    fn probe_maps_backend_failures() {
        let backend = MemorySecretStore::new();
        assert!(probe_with(&backend).is_ok());
        backend.set_failure(Some("locked"));
        let err = probe_with(&backend).unwrap_err();
        assert!(matches!(err, ZdkError::CredentialStore(_)));
    }

    #[test]
    fn largest_realistic_credential_fits_in_a_windows_blob() {
        // Every catalogue scope, long-but-realistic tokens, a stored client secret.
        let scopes: Vec<String> = crate::auth::scopes::CATALOGUE
            .iter()
            .map(|s| s.name.to_string())
            .collect();
        let max = Credential::OAuth(TokenSet {
            access_token: SecretString::from("a".repeat(128)),
            token_type: "bearer".into(),
            scopes,
            obtained_at: Utc::now(),
            expires_at: Some(Utc::now() + Duration::seconds(172_800)),
            refresh_token: Some(SecretString::from("r".repeat(128))),
            refresh_expires_at: Some(Utc::now() + Duration::days(90)),
            grant: GrantKind::ClientCredentials,
            subdomain: "s".repeat(63),
            client_id: "c".repeat(64),
            client_secret: Some(SecretString::from("x".repeat(64))),
        });
        let len = serde_json::to_vec(&max).unwrap().len();
        assert!(
            len < MAX_CREDENTIAL_BYTES,
            "{len} bytes ≥ {MAX_CREDENTIAL_BYTES}"
        );
        store().save("big", &max).unwrap();
    }

    #[test]
    fn keyring_error_mapping_is_actionable() {
        let err = map_error(&::keyring::Error::NoDefaultStore, "read");
        assert_eq!(err.exit_code(), 10);
        assert!(err.to_string().contains("--credential-store file"));
        let err = map_error(&::keyring::Error::NoEntry, "read");
        assert!(matches!(err, ZdkError::CredentialStore(_)));
    }
}
