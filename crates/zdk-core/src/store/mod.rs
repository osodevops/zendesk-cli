//! Where credentials live: OS keyring, encrypted file, environment, or memory.

use std::fmt;
use std::sync::Arc;

use crate::Result;
use crate::auth::token::Credential;

/// Backend identity, for `zdk auth status` / `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    Keyring,
    File,
    Env,
    Memory,
}

impl fmt::Display for StoreKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Keyring => "keyring",
            Self::File => "file",
            Self::Env => "env",
            Self::Memory => "memory",
        })
    }
}

/// User-facing selector (`--credential-store`, `ZENDESK_CREDENTIAL_STORE`, `auth.credential_store`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreSelector {
    #[default]
    Auto,
    Keyring,
    File,
    Env,
    None,
}

impl StoreSelector {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "keyring" | "keychain" => Some(Self::Keyring),
            "file" => Some(Self::File),
            "env" => Some(Self::Env),
            "none" | "memory" => Some(Self::None),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Keyring => "keyring",
            Self::File => "file",
            Self::Env => "env",
            Self::None => "none",
        }
    }
}

/// Persistence for per-profile credentials.
pub trait CredentialStore: Send + Sync + fmt::Debug {
    fn kind(&self) -> StoreKind;
    fn load(&self, profile: &str) -> Result<Option<Credential>>;
    fn save(&self, profile: &str, cred: &Credential) -> Result<()>;
    /// Returns `false` if nothing was stored for the profile.
    fn delete(&self, profile: &str) -> Result<bool>;
    fn list_profiles(&self) -> Result<Vec<String>>;
}

/// Shared handle used by the auth layer.
pub type SharedStore = Arc<dyn CredentialStore>;

pub mod memory;

pub use memory::MemoryStore;

/// Open the credential store selected by flags/env/config (phase P3 implements the real
/// keyring → encrypted-file → env backends and the `auto` probe; until then every selector
/// yields an in-memory store).
pub fn open(
    selector: StoreSelector,
    paths: &crate::config::Paths,
    env: &crate::config::EnvOverrides,
) -> Result<SharedStore> {
    let _ = (selector, paths, env);
    Ok(Arc::new(MemoryStore::new()))
}
