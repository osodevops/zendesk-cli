//! Per-profile persistence of the account limits learned from response headers, so the
//! governor starts every process with the right bucket instead of the 200/min floor
//! (PRD §10.2 "plan auto-detection … cache per profile").

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::util::fs::atomic_write_0600;

/// What is remembered between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateState {
    /// Last `X-Rate-Limit` seen on a Support request.
    #[serde(default)]
    pub support_limit: Option<u32>,
    /// Last `X-Rate-Limit` seen on a Help Center / Guide / community request.
    #[serde(default)]
    pub help_center_limit: Option<u32>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

/// `{state_dir}/rate_limit/{profile}.json`
#[must_use]
pub fn path(state_dir: &Path, profile: &str) -> PathBuf {
    state_dir
        .join("rate_limit")
        .join(format!("{}.json", safe_name(profile)))
}

/// Load the remembered limits; a missing or unreadable file is simply "nothing learned yet".
#[must_use]
pub fn load(state_dir: &Path, profile: &str) -> Option<RateState> {
    let text = std::fs::read_to_string(path(state_dir, profile)).ok()?;
    match serde_json::from_str(&text) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::debug!(error = %e, "ignoring unreadable rate-limit state");
            None
        }
    }
}

/// Persist atomically with owner-only permissions.
pub fn save(state_dir: &Path, profile: &str, state: &RateState) -> std::io::Result<()> {
    let mut state = state.clone();
    state.updated_at = Some(Utc::now());
    let text = serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?;
    atomic_write_0600(&path(state_dir, profile), text.as_bytes())
}

/// Profile names are user input; keep the file name to a safe subset.
fn safe_name(profile: &str) -> String {
    let cleaned: String = profile
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "default".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path(), "prod").is_none());
        let state = RateState {
            support_limit: Some(700),
            help_center_limit: None,
            updated_at: None,
        };
        save(dir.path(), "prod", &state).unwrap();
        let back = load(dir.path(), "prod").unwrap();
        assert_eq!(back.support_limit, Some(700));
        assert!(back.updated_at.is_some());
        assert!(path(dir.path(), "prod").ends_with("rate_limit/prod.json"));
        assert!(path(dir.path(), "we/ird").ends_with("we_ird.json"));
        std::fs::write(path(dir.path(), "prod"), "{not json").unwrap();
        assert!(load(dir.path(), "prod").is_none());
    }
}
