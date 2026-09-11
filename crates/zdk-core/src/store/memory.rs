//! In-memory credential store: used by tests and by `--credential-store none`.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{CredentialStore, StoreKind};
use crate::auth::token::Credential;
use crate::{Result, ZdkError};

/// Credentials that live only for the lifetime of the process.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<BTreeMap<String, Credential>>,
    fail_saves: AtomicBool,
}

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent `save` fail with a `CredentialStore` error (tests simulate a
    /// refresh-token rotation that could not be persisted).
    pub fn set_fail_saves(&self, fail: bool) {
        self.fail_saves.store(fail, Ordering::SeqCst);
    }
}

impl CredentialStore for MemoryStore {
    fn kind(&self) -> StoreKind {
        StoreKind::Memory
    }

    fn load(&self, profile: &str) -> Result<Option<Credential>> {
        Ok(self
            .entries
            .lock()
            .expect("memory store poisoned")
            .get(profile)
            .cloned())
    }

    fn save(&self, profile: &str, cred: &Credential) -> Result<()> {
        if self.fail_saves.load(Ordering::SeqCst) {
            return Err(ZdkError::CredentialStore(
                "memory store: simulated save failure".into(),
            ));
        }
        self.entries
            .lock()
            .expect("memory store poisoned")
            .insert(profile.to_string(), cred.clone());
        Ok(())
    }

    fn delete(&self, profile: &str) -> Result<bool> {
        Ok(self
            .entries
            .lock()
            .expect("memory store poisoned")
            .remove(profile)
            .is_some())
    }

    fn list_profiles(&self) -> Result<Vec<String>> {
        Ok(self
            .entries
            .lock()
            .expect("memory store poisoned")
            .keys()
            .cloned()
            .collect())
    }
}
