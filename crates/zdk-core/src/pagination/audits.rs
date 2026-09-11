//! `GET /api/v2/ticket_audits` — a cursor variant with its own field names (PRD §9). Types
//! only until `zdk sync` (v0.4) walks it.

use serde::{Deserialize, Serialize};

use crate::ZdkError;

/// The pagination fields of a ticket-audits response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditsMeta {
    #[serde(default)]
    pub after_cursor: Option<String>,
    #[serde(default)]
    pub before_cursor: Option<String>,
    #[serde(default)]
    pub after_url: Option<String>,
    #[serde(default)]
    pub before_url: Option<String>,
}

/// The error the audits walker returns until it exists.
#[must_use]
pub fn unsupported() -> ZdkError {
    ZdkError::Usage(
        "walking /api/v2/ticket_audits needs the audits cursor dialect: `zdk sync audits` arrives in v0.4. \
         Until then call it directly with `zdk api GET /api/v2/ticket_audits --query cursor=…` one page at a time."
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_and_error() {
        let m: AuditsMeta =
            serde_json::from_str(r#"{"after_cursor":"a","before_url":null}"#).unwrap();
        assert_eq!(m.after_cursor.as_deref(), Some("a"));
        assert_eq!(unsupported().exit_code(), 2);
    }
}
