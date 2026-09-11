//! Incremental exports (`/api/v2/incremental/*`, PRD §4.5): the types are here so the
//! registry and `zdk api describe` can talk about them; the walker itself ships with
//! `zdk sync` in v0.4.

use serde::{Deserialize, Serialize};

use crate::ZdkError;

/// Records per incremental page.
pub const PAGE_SIZE: u32 = 1000;
/// Time-based exports exclude the most recent minute.
pub const EXCLUSION_WINDOW_SECS: u64 = 60;
/// Global limit for the whole `/incremental/` family (30 with High Volume).
pub const REQUESTS_PER_MINUTE: u32 = 10;

/// The pagination fields of an incremental-export response (both time- and cursor-based).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncrementalMeta {
    /// Time-based: feed as the next `start_time`.
    #[serde(default)]
    pub end_time: Option<i64>,
    /// Cursor-based: the high-water mark.
    #[serde(default)]
    pub after_cursor: Option<String>,
    #[serde(default)]
    pub after_url: Option<String>,
    #[serde(default)]
    pub before_cursor: Option<String>,
    /// `true` once the export has caught up.
    #[serde(default)]
    pub end_of_stream: Option<bool>,
    /// Unreliable by Zendesk's own documentation; never a loop terminator.
    #[serde(default)]
    pub count: Option<u64>,
}

/// The error every incremental entry point returns until `zdk sync` exists.
#[must_use]
pub fn unsupported() -> ZdkError {
    ZdkError::Usage(
        "incremental exports need cursor state, dedupe and the 10/min budget: `zdk sync` arrives in v0.4. \
         Until then call the endpoint directly with `zdk api GET /api/v2/incremental/... --query cursor=…` one page at a time."
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_deserialises_both_modes() {
        let t: IncrementalMeta = serde_json::from_str(
            r#"{"end_time": 1700000000, "end_of_stream": false, "count": 1000}"#,
        )
        .unwrap();
        assert_eq!(t.end_time, Some(1_700_000_000));
        assert_eq!(t.end_of_stream, Some(false));
        let c: IncrementalMeta = serde_json::from_str(
            r#"{"after_cursor": "abc", "after_url": "https://x/y", "end_of_stream": true}"#,
        )
        .unwrap();
        assert_eq!(c.after_cursor.as_deref(), Some("abc"));
        assert_eq!(unsupported().exit_code(), 2);
        assert!(unsupported().to_string().contains("v0.4"));
    }
}
