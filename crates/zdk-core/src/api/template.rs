//! A tiny path-template matcher shared by the registry lookup and the rate-limit rules.
//!
//! Templates look like OpenAPI paths: `{name}` matches exactly one segment, `{name..}` matches
//! the rest of the path (one or more segments). Matching is on segments, so `/a/b` never
//! matches `/a/bc`. Callers normalise the request path with [`normalize`] first.

/// Strip the query string, a trailing `.json` and a trailing slash.
#[must_use]
pub fn normalize(path: &str) -> String {
    let no_query = path.split(['?', '#']).next().unwrap_or_default();
    let trimmed = no_query.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".json").unwrap_or(trimmed);
    if trimmed.is_empty() {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

/// Match `path` (already normalised) against `template`, returning the captured parameters in
/// template order, or `None`.
#[must_use]
pub fn match_template(template: &str, path: &str) -> Option<Vec<(String, String)>> {
    let tpl: Vec<&str> = template.trim_matches('/').split('/').collect();
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let mut params = Vec::new();
    let mut i = 0;
    for (ti, t) in tpl.iter().enumerate() {
        if let Some(name) = t.strip_prefix('{').and_then(|t| t.strip_suffix("..}")) {
            // Remainder: everything left, but at least one segment; only valid as the last part.
            if ti + 1 != tpl.len() || i >= segs.len() {
                return None;
            }
            params.push((name.to_string(), segs[i..].join("/")));
            return Some(params);
        }
        let seg = segs.get(i)?;
        if let Some(name) = t.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            if seg.is_empty() {
                return None;
            }
            params.push((name.to_string(), (*seg).to_string()));
        } else if t != seg {
            return None;
        }
        i += 1;
    }
    (i == segs.len()).then_some(params)
}

/// Number of literal (non-parameter) segments — higher means a more specific template, so
/// `/api/v2/users/me` beats `/api/v2/users/{user_id}`.
#[must_use]
pub fn specificity(template: &str) -> usize {
    template
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.starts_with('{'))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_query_json_suffix_and_slashes() {
        assert_eq!(normalize("/api/v2/tickets.json?page=2"), "/api/v2/tickets");
        assert_eq!(normalize("api/v2/tickets/"), "/api/v2/tickets");
        assert_eq!(normalize("/api/v2/tickets/12.json"), "/api/v2/tickets/12");
        assert_eq!(normalize(""), "/");
    }

    #[test]
    fn single_segment_params_and_literals() {
        assert_eq!(
            match_template("/api/v2/tickets/{ticket_id}", "/api/v2/tickets/42"),
            Some(vec![("ticket_id".into(), "42".into())])
        );
        assert_eq!(
            match_template("/api/v2/tickets", "/api/v2/tickets"),
            Some(vec![])
        );
        assert!(match_template("/api/v2/tickets/{ticket_id}", "/api/v2/tickets").is_none());
        assert!(match_template("/api/v2/tickets/{ticket_id}", "/api/v2/tickets/42/tags").is_none());
        assert!(match_template("/api/v2/tickets", "/api/v2/ticketsx").is_none());
        assert!(match_template("/api/v2/tickets/{ticket_id}", "/api/v2/tickets//").is_none());
    }

    #[test]
    fn remainder_params_take_the_rest() {
        assert_eq!(
            match_template(
                "/api/v2/incremental/{rest..}",
                "/api/v2/incremental/tickets/cursor"
            ),
            Some(vec![("rest".into(), "tickets/cursor".into())])
        );
        assert!(match_template("/api/v2/incremental/{rest..}", "/api/v2/incremental").is_none());
    }

    #[test]
    fn specificity_prefers_literals() {
        assert!(specificity("/api/v2/users/me") > specificity("/api/v2/users/{user_id}"));
    }
}
