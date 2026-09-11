//! Ticket comments (Support API `ticket_comments`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::ticket::Via;

/// One ticket comment.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(rename = "type", default)]
    pub comment_type: Option<String>,
    #[serde(default)]
    pub author_id: Option<u64>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub html_body: Option<String>,
    #[serde(default)]
    pub plain_body: Option<String>,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub attachments: Vec<Value>,
    #[serde(default)]
    pub audit_id: Option<u64>,
    #[serde(default)]
    pub via: Option<Via>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Comment {
    /// The best plain-text body for a transcript.
    #[must_use]
    pub fn text(&self) -> &str {
        self.plain_body
            .as_deref()
            .or(self.body.as_deref())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_parses_and_keeps_metadata() {
        let text = include_str!("../../../../tests/fixtures/tickets/comments.json");
        let v: Value = serde_json::from_str(text).unwrap();
        let comments: Vec<Comment> = serde_json::from_value(v["comments"].clone()).unwrap();
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[1].public, Some(false));
        assert_eq!(
            comments[0].text(),
            comments[0].plain_body.as_deref().unwrap()
        );
        assert!(comments[0].extra["metadata"]["system"]["client"].is_string());
        let bare = Comment {
            body: Some("b".into()),
            ..Default::default()
        };
        assert_eq!(bare.text(), "b");
        assert_eq!(Comment::default().text(), "");
    }
}
