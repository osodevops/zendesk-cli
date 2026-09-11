//! Users (agents, admins and end users).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A Zendesk user.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct User {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub verified: Option<bool>,
    #[serde(default)]
    pub suspended: Option<bool>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub organization_id: Option<u64>,
    #[serde(default)]
    pub default_group_id: Option<u64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub user_fields: Option<Map<String, Value>>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `{"user": …}` envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserEnvelope {
    pub user: User,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_parses_with_user_fields_and_extra() {
        let env: UserEnvelope =
            serde_json::from_str(include_str!("../../../../tests/fixtures/users/user.json"))
                .unwrap();
        let u = env.user;
        assert_eq!(u.id, Some(42));
        assert_eq!(u.email.as_deref(), Some("ada@example.com"));
        assert_eq!(u.user_fields.as_ref().unwrap()["tier"], "gold");
        assert_eq!(u.extra["two_factor_auth_enabled"], true);
        assert_eq!(u.tags, ["platform"]);
    }
}
