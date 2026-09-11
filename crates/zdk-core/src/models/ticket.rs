//! The ticket shape (Support API). Every field is optional and unknown keys survive a
//! round trip through `extra`, so commands can pass records through untouched.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A ticket's custom field value (`custom_fields[]`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomField {
    pub id: u64,
    #[serde(default)]
    pub value: Value,
}

/// `via` — the channel a ticket or comment arrived through.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Via {
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub source: Option<Value>,
}

/// `satisfaction_rating` on a ticket.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SatisfactionRating {
    #[serde(default)]
    pub score: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub id: Option<u64>,
}

/// A Zendesk ticket.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ticket {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub raw_subject: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(rename = "type", default)]
    pub ticket_type: Option<String>,
    #[serde(default)]
    pub requester_id: Option<u64>,
    #[serde(default)]
    pub submitter_id: Option<u64>,
    #[serde(default)]
    pub assignee_id: Option<u64>,
    #[serde(default)]
    pub organization_id: Option<u64>,
    #[serde(default)]
    pub group_id: Option<u64>,
    #[serde(default)]
    pub brand_id: Option<u64>,
    #[serde(default)]
    pub ticket_form_id: Option<u64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub custom_fields: Vec<CustomField>,
    #[serde(default)]
    pub via: Option<Via>,
    #[serde(default)]
    pub satisfaction_rating: Option<SatisfactionRating>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub due_at: Option<String>,
    /// Everything Zendesk sent that this struct does not model.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `{"ticket": …}` envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TicketEnvelope {
    pub ticket: Ticket,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_round_trips_with_unknown_keys_preserved() {
        let text = include_str!("../../../../tests/fixtures/tickets/ticket.json");
        let env: TicketEnvelope = serde_json::from_str(text).unwrap();
        let t = &env.ticket;
        assert_eq!(t.id, Some(1));
        assert_eq!(t.ticket_type.as_deref(), Some("incident"));
        assert_eq!(t.custom_fields[0].id, 360_000_001);
        assert_eq!(t.custom_fields[0].value, "production");
        assert_eq!(t.via.as_ref().unwrap().channel.as_deref(), Some("email"));
        assert_eq!(
            t.satisfaction_rating.as_ref().unwrap().score.as_deref(),
            Some("unoffered")
        );
        assert_eq!(t.extra["allow_attachments"], true);
        let back = serde_json::to_value(&env).unwrap();
        let original: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            back["ticket"]["custom_status_id"],
            original["ticket"]["custom_status_id"]
        );
        assert_eq!(back["ticket"]["type"], "incident");
    }

    #[test]
    fn empty_object_is_a_valid_ticket() {
        let t: Ticket = serde_json::from_str("{}").unwrap();
        assert!(t.id.is_none() && t.tags.is_empty());
    }
}
