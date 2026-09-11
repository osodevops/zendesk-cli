//! In-memory credential store: used by tests and by `--credential-store none`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::{CredentialStore, StoreKind};
use crate::Result;
use crate::auth::token::Credential;

/// Credentials that live only for the lifetime of the process.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<BTreeMap<String, Credential>>,
}

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
