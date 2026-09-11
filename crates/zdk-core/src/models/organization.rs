//! Organizations.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A Zendesk organization.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Organization {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub domain_names: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub group_id: Option<u64>,
    #[serde(default)]
    pub shared_tickets: Option<bool>,
    #[serde(default)]
    pub shared_comments: Option<bool>,
    #[serde(default)]
    pub details: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub organization_fields: Option<Map<String, Value>>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `{"organization": …}` envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrganizationEnvelope {
    pub organization: Organization,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_parses() {
        let env: OrganizationEnvelope = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/organizations/organization.json"
        ))
        .unwrap();
        let o = env.organization;
        assert_eq!(o.id, Some(1001));
        assert_eq!(o.domain_names, ["acme.com", "acme.co.uk"]);
        assert_eq!(o.shared_tickets, Some(true));
        assert_eq!(o.organization_fields.as_ref().unwrap()["region"], "EMEA");
        assert!(o.extra.is_empty(), "{:?}", o.extra);
    }
}
