//! Cursor pagination (`page[size]` / `page[after]`, PRD §4.4): follow `links.next`, fall back
//! to `meta.after_cursor`, then to a `Link: …; rel="next"` header (Help Center); stop when
//! `meta.has_more` is false or a page comes back empty.

use serde_json::Value;
use url::Url;

use super::{Page, PageInfo, PageOptions, Paginator, link_header};
use crate::api::PageDialect;
use crate::http::RequestSpec;
use crate::{Result, ZdkError};

/// See the module docs.
#[derive(Debug, Default)]
pub struct CursorPaginator {
    warned_sort: bool,
}

impl CursorPaginator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Turn a `next` URL Zendesk handed back into a request under the same base: only the
    /// path and query survive, so a foreign host can never be followed.
    fn spec_for_next(prev: &RequestSpec, next: &str) -> Result<RequestSpec> {
        let url = match Url::parse(next) {
            Ok(u) => u,
            Err(_) => Url::parse("https://zendesk.invalid")
                .and_then(|b| b.join(next))
                .map_err(|e| {
                    ZdkError::Other(format!(
                        "Zendesk returned an unusable next link '{next}': {e}"
                    ))
                })?,
        };
        let mut spec = RequestSpec::from_url(prev.method, &url);
        spec.headers.clone_from(&prev.headers);
        spec.op = prev.op;
        spec.timeout = prev.timeout;
        spec.no_preflight = prev.no_preflight;
        spec.rate_rules.clone_from(&prev.rate_rules);
        Ok(spec)
    }
}

impl Paginator for CursorPaginator {
    fn dialect(&self) -> PageDialect {
        PageDialect::Cursor
    }

    fn first(&mut self, spec: &RequestSpec, opts: &PageOptions) -> Result<RequestSpec> {
        let mut next = spec
            .try_clone()
            .ok_or_else(|| ZdkError::Usage("cannot paginate a multipart request".into()))?;
        if next.query_value("page[size]").is_none() {
            next.set_query("page[size]", opts.page_size.clamp(1, super::MAX_PAGE_SIZE));
        }
        if let Some(after) = &opts.start_after {
            next.set_query("page[after]", after);
        }
        if !self.warned_sort
            && (next.query_value("sort_by").is_some() || next.query_value("sort_order").is_some())
        {
            self.warned_sort = true;
            crate::output::warn(
                "warning: sort_by/sort_order are ignored by cursor pagination on this endpoint; use `sort=` (e.g. sort=-updated_at) if it supports sorting",
            );
        }
        Ok(next)
    }

    fn inspect(&self, body: &Value, headers: &http::HeaderMap) -> PageInfo {
        let meta_has_more = body
            .get("meta")
            .and_then(|m| m.get("has_more"))
            .and_then(Value::as_bool);
        let links_next = body
            .get("links")
            .and_then(|l| l.get("next"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let header_next = link_header::next_url(headers);
        let cursor = body
            .get("meta")
            .and_then(|m| m.get("after_cursor"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let has_more = match meta_has_more {
            Some(b) => b,
            None => links_next.is_some() || header_next.is_some(),
        };
        PageInfo {
            has_more,
            count: body.get("count").and_then(Value::as_u64),
            cursor,
            page: None,
        }
    }

    fn next(
        &mut self,
        page: &Page,
        prev: &RequestSpec,
        _opts: &PageOptions,
    ) -> Result<Option<RequestSpec>> {
        if !page.has_more || page.items.is_empty() {
            return Ok(None);
        }
        let links_next = page
            .body
            .get("links")
            .and_then(|l| l.get("next"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(next) = links_next {
            return Self::spec_for_next(prev, next).map(Some);
        }
        let after = page
            .body
            .get("meta")
            .and_then(|m| m.get("after_cursor"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(cursor) = after {
            let mut next = prev
                .try_clone()
                .ok_or_else(|| ZdkError::Usage("cannot paginate a multipart request".into()))?;
            next.set_query("page[after]", cursor);
            next.remove_query("page[before]");
            return Ok(Some(next));
        }
        if let Some(next) = link_header::next_url(&page.headers) {
            return Self::spec_for_next(prev, &next).map(Some);
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(body: Value, headers: http::HeaderMap, items: usize) -> Page {
        let p = CursorPaginator::new();
        let info = p.inspect(&body, &headers);
        Page {
            number: 1,
            items: vec![json!({}); items],
            body,
            status: 200,
            headers,
            rate: crate::http::RateHeaders::default(),
            request_id: None,
            has_more: info.has_more,
            count: info.count,
            fetched_total: items as u64,
        }
    }

    #[test]
    fn first_adds_page_size_and_start_after() {
        let mut p = CursorPaginator::new();
        let spec = RequestSpec::get("/api/v2/tickets").query("sort", "-updated_at");
        let opts = PageOptions {
            page_size: 25,
            start_after: Some("c9".into()),
            ..PageOptions::default()
        };
        let first = p.first(&spec, &opts).unwrap();
        assert_eq!(first.query_value("page[size]"), Some("25"));
        assert_eq!(first.query_value("page[after]"), Some("c9"));
        assert_eq!(first.query_value("sort"), Some("-updated_at"));
        let explicit = RequestSpec::get("/api/v2/tickets").query("page[size]", "7");
        assert_eq!(
            p.first(&explicit, &PageOptions::default())
                .unwrap()
                .query_value("page[size]"),
            Some("7")
        );
    }

    #[test]
    fn links_next_is_made_relative_and_wins_over_cursor() {
        let mut p = CursorPaginator::new();
        let prev = RequestSpec::get("/api/v2/tickets")
            .query("page[size]", "2")
            .header("X-Custom", "1");
        let body = json!({
            "tickets": [{}, {}],
            "meta": {"has_more": true, "after_cursor": "c2"},
            "links": {"next": "https://acme.zendesk.com/api/v2/tickets.json?page%5Bsize%5D=2&page%5Bafter%5D=c2"}
        });
        let pg = page(body, http::HeaderMap::new(), 2);
        let next = p
            .next(&pg, &prev, &PageOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(next.path, "/api/v2/tickets.json");
        assert_eq!(next.query_value("page[after]"), Some("c2"));
        assert_eq!(next.query_value("page[size]"), Some("2"));
        assert_eq!(next.headers, prev.headers);
        assert!(!next.is_absolute());
    }

    #[test]
    fn after_cursor_and_link_header_fallbacks_and_stop_conditions() {
        let mut p = CursorPaginator::new();
        let prev = RequestSpec::get("/api/v2/tickets")
            .query("page[size]", "2")
            .query("page[before]", "x");
        let body = json!({"tickets": [{}], "meta": {"has_more": true, "after_cursor": "c3"}, "links": {"next": null}});
        let next = p
            .next(
                &page(body, http::HeaderMap::new(), 1),
                &prev,
                &PageOptions::default(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(next.query_value("page[after]"), Some("c3"));
        assert!(next.query_value("page[before]").is_none());

        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::LINK, "<https://acme.zendesk.com/api/v2/help_center/articles.json?page=2&per_page=30>; rel=\"next\"".parse().unwrap());
        let body = json!({"articles": [{}]});
        let pg = page(body, headers, 1);
        assert!(pg.has_more, "Link header implies more");
        let next = p
            .next(&pg, &prev, &PageOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(next.path, "/api/v2/help_center/articles.json");
        assert_eq!(next.query_value("page"), Some("2"));

        let done = page(
            json!({"tickets": [], "meta": {"has_more": false}}),
            http::HeaderMap::new(),
            0,
        );
        assert!(!done.has_more);
        assert!(
            p.next(&done, &prev, &PageOptions::default())
                .unwrap()
                .is_none()
        );
        let empty_more = page(
            json!({"tickets": [], "meta": {"has_more": true, "after_cursor": "z"}}),
            http::HeaderMap::new(),
            0,
        );
        assert!(
            p.next(&empty_more, &prev, &PageOptions::default())
                .unwrap()
                .is_none(),
            "empty page ends the walk"
        );
    }
}
