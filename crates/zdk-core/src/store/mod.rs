//! Where credentials live: OS keyring, encrypted file, environment, or memory.
//!
//! Selection (`--credential-store`, `ZENDESK_CREDENTIAL_STORE`, `auth.credential_store`):
//! - `auto` (default): probe the OS keyring once, fall back to the encrypted file when it is
//!   unusable, and cache the decision in `state/store-decision.json` so every later command
//!   skips the probe (`zdk auth login` / `zdk doctor` re-probe with [`open_with`]).
//! - `keyring`, `file`, `env`, `none` force a backend.

use std::fmt;
use std::sync::Arc;

use crate::Result;
use crate::auth::token::Credential;
use crate::config::{EnvOverrides, Paths};

/// Backend identity, for `zdk auth status` / `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    Keyring,
    File,
    Env,
    Memory,
}

impl StoreKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::File => "file",
            Self::Env => "env",
            Self::Memory => "memory",
        }
    }
}

impl fmt::Display for StoreKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

pub mod env;
pub mod file;
pub mod keyring;
pub mod memory;

pub use env::EnvStore;
pub use file::FileStore;
pub use keyring::KeyringStore;
pub use memory::MemoryStore;

/// File name (under the state directory) that caches the `auto` decision.
pub const DECISION_FILE: &str = "store-decision.json";

/// What `auto` decided, as cached on disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoreDecision {
    pub backend: StoreKind,
    pub decided_at: chrono::DateTime<chrono::Utc>,
    /// Why the keyring was rejected, when it was (for `doctor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl StoreDecision {
    fn path(paths: &Paths) -> std::path::PathBuf {
        paths.state_dir.join(DECISION_FILE)
    }

    /// The cached decision, if a readable one exists.
    #[must_use]
    pub fn read(paths: &Paths) -> Option<Self> {
        let bytes = std::fs::read(Self::path(paths)).ok()?;
        serde_json::from_slice::<Self>(&bytes)
            .ok()
            .filter(|d| matches!(d.backend, StoreKind::Keyring | StoreKind::File))
    }

    /// Persist (best effort — a read-only state dir must not break the command).
    pub fn write(&self, paths: &Paths) {
        if let Ok(json) = serde_json::to_vec_pretty(self)
            && let Err(e) = crate::util::fs::atomic_write_0600(&Self::path(paths), &json)
        {
            tracing::debug!(error = %e, "could not cache the credential-store decision");
        }
    }

    /// Remove the cache so the next `auto` open probes again.
    pub fn clear(paths: &Paths) {
        let _ = std::fs::remove_file(Self::path(paths));
    }
}

/// Open the credential store selected by flags/env/config, honouring a cached `auto` decision.
pub fn open(selector: StoreSelector, paths: &Paths, env: &EnvOverrides) -> Result<SharedStore> {
    open_with(selector, paths, env, false)
}

/// [`open`] with control over the `auto` probe: `force_probe = true` ignores the cached
/// decision and asks the keyring again (`zdk auth login`, `zdk doctor`).
pub fn open_with(
    selector: StoreSelector,
    paths: &Paths,
    env: &EnvOverrides,
    force_probe: bool,
) -> Result<SharedStore> {
    open_with_probe(selector, paths, env, force_probe, &keyring::probe)
}

/// Same as [`open_with`] with the keyring probe injected (tests never touch a real keychain).
pub fn open_with_probe(
    selector: StoreSelector,
    paths: &Paths,
    env: &EnvOverrides,
    force_probe: bool,
    probe: &dyn Fn() -> Result<()>,
) -> Result<SharedStore> {
    match selector {
        StoreSelector::None => Ok(Arc::new(MemoryStore::new())),
        StoreSelector::Env => Ok(Arc::new(EnvStore::from_env(env))),
        StoreSelector::Keyring => Ok(Arc::new(KeyringStore::os())),
        StoreSelector::File => Ok(Arc::new(FileStore::new(paths, env))),
        StoreSelector::Auto => {
            if !force_probe && let Some(cached) = StoreDecision::read(paths) {
                tracing::debug!(backend = %cached.backend, "using cached credential-store decision");
                return Ok(backend_for(cached.backend, paths, env));
            }
            let (backend, reason) = match probe() {
                Ok(()) => (StoreKind::Keyring, None),
                Err(e) => {
                    tracing::info!(error = %e, "OS keyring unavailable; using the encrypted file store");
                    (StoreKind::File, Some(e.to_string()))
                }
            };
            StoreDecision {
                backend,
                decided_at: chrono::Utc::now(),
                reason,
            }
            .write(paths);
            Ok(backend_for(backend, paths, env))
        }
    }
}

fn backend_for(kind: StoreKind, paths: &Paths, env: &EnvOverrides) -> SharedStore {
    match kind {
        StoreKind::Keyring => Arc::new(KeyringStore::os()),
        StoreKind::File => Arc::new(FileStore::new(paths, env)),
        StoreKind::Env => Arc::new(EnvStore::from_env(env)),
        StoreKind::Memory => Arc::new(MemoryStore::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ZdkError;

    fn paths(root: &std::path::Path) -> Paths {
        Paths {
            config_file: root.join("config.toml"),
            config_dir: root.to_path_buf(),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
        }
    }

    #[test]
    fn explicit_selectors_map_to_backends() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let env = EnvOverrides::default();
        let never = || panic!("probe must not run for explicit selectors");
        assert_eq!(
            open_with_probe(StoreSelector::None, &p, &env, false, &never)
                .unwrap()
                .kind(),
            StoreKind::Memory
        );
        assert_eq!(
            open_with_probe(StoreSelector::Env, &p, &env, false, &never)
                .unwrap()
                .kind(),
            StoreKind::Env
        );
        assert_eq!(
            open_with_probe(StoreSelector::File, &p, &env, false, &never)
                .unwrap()
                .kind(),
            StoreKind::File
        );
        assert_eq!(
            open_with_probe(StoreSelector::Keyring, &p, &env, false, &never)
                .unwrap()
                .kind(),
            StoreKind::Keyring
        );
    }

    #[test]
    fn auto_falls_back_to_file_and_caches_the_decision() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let env = EnvOverrides::default();
        let calls = std::cell::Cell::new(0);
        let failing = || {
            calls.set(calls.get() + 1);
            Err(ZdkError::CredentialStore("no session bus".into()))
        };

        let store = open_with_probe(StoreSelector::Auto, &p, &env, false, &failing).unwrap();
        assert_eq!(store.kind(), StoreKind::File);
        assert_eq!(calls.get(), 1);
        let cached = StoreDecision::read(&p).expect("decision cached");
        assert_eq!(cached.backend, StoreKind::File);
        assert_eq!(
            cached.reason.as_deref(),
            Some("credential store error: no session bus")
        );

        // Cached: no probe on the next open, even if the keyring would now succeed.
        let ok = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let store = open_with_probe(StoreSelector::Auto, &p, &env, false, &ok).unwrap();
        assert_eq!(store.kind(), StoreKind::File);
        assert_eq!(calls.get(), 1);

        // force_probe re-asks and rewrites the cache.
        let store = open_with_probe(StoreSelector::Auto, &p, &env, true, &ok).unwrap();
        assert_eq!(store.kind(), StoreKind::Keyring);
        assert_eq!(calls.get(), 2);
        assert_eq!(StoreDecision::read(&p).unwrap().backend, StoreKind::Keyring);

        StoreDecision::clear(&p);
        assert!(StoreDecision::read(&p).is_none());
    }

    #[test]
    fn corrupt_decision_cache_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        std::fs::create_dir_all(&p.state_dir).unwrap();
        std::fs::write(p.state_dir.join(DECISION_FILE), b"{not json").unwrap();
        assert!(StoreDecision::read(&p).is_none());
        std::fs::write(
            p.state_dir.join(DECISION_FILE),
            br#"{"backend":"memory","decided_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert!(
            StoreDecision::read(&p).is_none(),
            "only keyring/file are valid decisions"
        );
    }

    #[test]
    fn selector_parse_round_trips() {
        for s in [
            StoreSelector::Auto,
            StoreSelector::Keyring,
            StoreSelector::File,
            StoreSelector::Env,
            StoreSelector::None,
        ] {
            assert_eq!(StoreSelector::parse(s.as_str()), Some(s));
        }
        assert_eq!(
            StoreSelector::parse("keychain"),
            Some(StoreSelector::Keyring)
        );
        assert_eq!(StoreSelector::parse("memory"), Some(StoreSelector::None));
        assert!(StoreSelector::parse("cloud").is_none());
        assert_eq!(StoreKind::Keyring.to_string(), "keyring");
    }
}
