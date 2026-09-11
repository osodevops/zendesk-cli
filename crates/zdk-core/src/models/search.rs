//! Search results (`GET /api/v2/search` and `/search/export`): each record is a ticket, user,
//! organization or group tagged with `result_type`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One search hit. The typed fields are the ones every result type shares plus the
/// table-preset columns; everything else is in `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub result_type: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl SearchResult {
    /// `subject` (tickets), `name` (users/organizations/groups) or `title`, whichever exists.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.subject
            .as_deref()
            .or(self.name.as_deref())
            .or(self.title.as_deref())
    }
}

/// The offset-paginated `/api/v2/search` envelope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchPage {
    #[serde(default)]
    pub results: Vec<SearchResult>,
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub next_page: Option<String>,
    #[serde(default)]
    pub previous_page: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_results_expose_a_title_per_type() {
        let page: SearchPage = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/search/mixed_results.json"
        ))
        .unwrap();
        assert_eq!(page.count, Some(3));
        assert!(page.next_page.is_none());
        let types: Vec<&str> = page
            .results
            .iter()
            .filter_map(|r| r.result_type.as_deref())
            .collect();
        assert_eq!(types, ["ticket", "user", "organization"]);
        assert_eq!(page.results[0].title(), Some("API latency on eu-west-1"));
        assert_eq!(page.results[1].title(), Some("Ada Lovelace"));
        assert_eq!(page.results[2].extra["domain_names"][0], "acme.com");
        assert_eq!(SearchResult::default().title(), None);
    }
}
