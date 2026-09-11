//! Offset pagination (`page` / `per_page`): stop on `next_page: null`, never let the user find
//! the 400 at page 101 (PRD §4.4), and know the search API's quirks (PRD §4.6).

use serde_json::Value;

use super::{Page, PageInfo, PageOptions, Paginator};
use crate::api::{PageDialect, template};
use crate::http::RequestSpec;
use crate::{Result, ZdkError};

/// Zendesk rejects offset requests beyond this many pages …
pub const MAX_OFFSET_PAGES: u32 = 100;
/// … or this many records.
pub const MAX_OFFSET_RECORDS: u64 = 10_000;
/// `GET /api/v2/search` never returns more than this many results.
pub const SEARCH_CAP: u64 = 1000;
/// `GET /api/v2/tickets?page=N` with `N > 500` draws on a 50/min sub-budget.
pub const TICKETS_INDEX_DEEP_PAGE: u32 = 500;

/// What to use instead of a deep offset walk on `path`.
#[must_use]
pub fn alternatives_for(path: &str) -> Vec<String> {
    let p = template::normalize(path);
    if p.starts_with("/api/v2/search") {
        vec![
            "zdk search export <query> --filter-type ticket|user|organization  (cursor-based, no 1,000-result cap)".into(),
            "narrow the query with created>… / updated>… so it matches fewer than 1,000 records".into(),
        ]
    } else if p == "/api/v2/tickets" || p.starts_with("/api/v2/tickets/") {
        vec![
            "zdk tickets list --all  (cursor pagination, no depth limit)".into(),
            "zdk api GET /api/v2/tickets --paginate  (cursor dialect is inferred from the registry)".into(),
            "zdk sync tickets  (incremental export, v0.4)".into(),
        ]
    } else if p.contains("/views/") {
        vec![
            "zdk api GET /api/v2/views/{view_id}/export  (view export, cursor-based)".into(),
            "zdk search export <query>  for the same filter expressed as a search".into(),
        ]
    } else {
        vec![
            "the cursor dialect (`page[size]=…`) where the endpoint supports it — `zdk api ops --grep <resource>` shows PAGINATION".into(),
            "an incremental export (`zdk sync …`, v0.4) for full-table walks".into(),
        ]
    }
}

/// The early guard: refuse to request a page whose first record lies past the 10,000-record
/// limit (page 101 at `per_page=100`), and refuse a `--all` walk whose `count` already says it
/// cannot finish.
pub fn guard(
    path: &str,
    next_page: u32,
    per_page: u32,
    count: Option<u64>,
    all: bool,
    limit: Option<u64>,
) -> Result<()> {
    let first_record = u64::from(next_page.saturating_sub(1)) * u64::from(per_page.max(1));
    if first_record >= MAX_OFFSET_RECORDS {
        return Err(limit_error(path));
    }
    guard_count(path, count, all, limit)
}

/// The `count` half of [`guard`]: a `--all` walk that `count` says cannot finish fails before
/// its first page is emitted.
pub fn guard_count(path: &str, count: Option<u64>, all: bool, limit: Option<u64>) -> Result<()> {
    let wants_everything = all && limit.is_none_or(|l| l > MAX_OFFSET_RECORDS);
    if wants_everything && count.is_some_and(|c| c > MAX_OFFSET_RECORDS) {
        return Err(limit_error(path));
    }
    Ok(())
}

fn limit_error(path: &str) -> ZdkError {
    ZdkError::PaginationLimit {
        endpoint: template::normalize(path),
        alternatives: alternatives_for(path),
    }
}

/// See the module docs.
#[derive(Debug)]
pub struct OffsetPaginator {
    /// `/api/v2/search`: warn at the 1,000 cap and treat a 422 on `next_page` as the end.
    search: bool,
    tickets_index: bool,
    warned_cap: bool,
    page: u32,
}

impl OffsetPaginator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            search: false,
            tickets_index: false,
            warned_cap: false,
            page: 0,
        }
    }

    fn classify(&mut self, path: &str) {
        let p = template::normalize(path);
        self.search = p == "/api/v2/search";
        self.tickets_index = p == "/api/v2/tickets";
    }
}

impl Default for OffsetPaginator {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_page(s: &str) -> Option<u32> {
    s.trim().parse().ok().filter(|p| *p > 0)
}

/// `per_page` from the query, else `--page-size`.
fn per_page_of(spec: &RequestSpec, opts: &PageOptions) -> u32 {
    spec.query_value("per_page")
        .and_then(parse_page)
        .unwrap_or_else(|| opts.page_size.clamp(1, super::MAX_PAGE_SIZE))
}

impl Paginator for OffsetPaginator {
    fn dialect(&self) -> PageDialect {
        PageDialect::Offset
    }

    fn first(&mut self, spec: &RequestSpec, opts: &PageOptions) -> Result<RequestSpec> {
        self.classify(&spec.path);
        let mut next = spec
            .try_clone()
            .ok_or_else(|| ZdkError::Usage("cannot paginate a multipart request".into()))?;
        let page = opts
            .start_after
            .as_deref()
            .and_then(parse_page)
            .or_else(|| next.query_value("page").and_then(parse_page))
            .unwrap_or(1);
        let per_page = per_page_of(&next, opts);
        guard(&spec.path, page, per_page, None, opts.all, opts.limit)?;
        next.set_query("page", page);
        next.set_query("per_page", per_page);
        if self.tickets_index && page > TICKETS_INDEX_DEEP_PAGE {
            next = next.rate_rule("tickets_index_deep");
        }
        self.page = page;
        Ok(next)
    }

    fn inspect(&self, body: &Value, _headers: &http::HeaderMap) -> PageInfo {
        PageInfo {
            has_more: body.get("next_page").is_some_and(|v| !v.is_null()),
            count: body.get("count").and_then(Value::as_u64),
            cursor: None,
            page: Some(self.page),
        }
    }

    fn check_first(&self, page: &Page, prev: &RequestSpec, opts: &PageOptions) -> Result<()> {
        if !page.has_more {
            return Ok(());
        }
        guard_count(&prev.path, page.count, opts.all, opts.limit)
    }

    fn next(
        &mut self,
        page: &Page,
        prev: &RequestSpec,
        opts: &PageOptions,
    ) -> Result<Option<RequestSpec>> {
        if !page.has_more || page.items.is_empty() {
            return Ok(None);
        }
        let next_page = self.page + 1;
        guard(
            &prev.path,
            next_page,
            per_page_of(prev, opts),
            page.count,
            opts.all,
            opts.limit,
        )?;
        if self.search && !self.warned_cap && page.fetched_total >= SEARCH_CAP {
            self.warned_cap = true;
            crate::output::warn(&format!(
                "warning: Zendesk search returns at most {SEARCH_CAP} results (count says {}); use `zdk search export` for the full set",
                page.count
                    .map_or_else(|| "unknown".to_string(), |c| c.to_string())
            ));
        }
        let mut next = prev
            .try_clone()
            .ok_or_else(|| ZdkError::Usage("cannot paginate a multipart request".into()))?;
        next.set_query("page", next_page);
        if self.tickets_index && next_page > TICKETS_INDEX_DEEP_PAGE {
            next = next.rate_rule("tickets_index_deep");
        }
        self.page = next_page;
        Ok(Some(next))
    }

    fn ends_on_422(&self) -> bool {
        self.search && self.page > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_math() {
        assert!(guard("/api/v2/tickets", 100, 100, None, true, None).is_ok());
        let err = guard("/api/v2/tickets", 101, 100, None, false, None).unwrap_err();
        assert_eq!(err.exit_code(), 12);
        assert!(
            guard("/api/v2/tickets", 501, 10, None, true, None).is_ok(),
            "small per_page goes deeper"
        );
        assert!(guard("/api/v2/tickets", 1001, 10, None, true, None).is_err());
        assert!(guard("/api/v2/search", 2, 100, Some(20_000), true, None).is_err());
        assert!(
            guard("/api/v2/search", 2, 100, Some(20_000), true, Some(500)).is_ok(),
            "a small --limit finishes in time"
        );
        assert!(
            guard("/api/v2/search", 2, 100, Some(20_000), false, None).is_ok(),
            "single page is fine"
        );
        assert!(guard("/api/v2/search", 2, 100, Some(10_000), true, None).is_ok());
        if let ZdkError::PaginationLimit {
            endpoint,
            alternatives,
        } = guard("/api/v2/search.json?query=x", 101, 100, None, true, None).unwrap_err()
        {
            assert_eq!(endpoint, "/api/v2/search");
            assert!(
                alternatives[0].contains("zdk search export"),
                "{alternatives:?}"
            );
        } else {
            panic!("expected PaginationLimit");
        }
        assert!(alternatives_for("/api/v2/tickets")[0].contains("tickets list --all"));
        assert!(alternatives_for("/api/v2/views/1/tickets")[0].contains("export"));
        assert!(!alternatives_for("/api/v2/organizations").is_empty());
    }

    #[test]
    fn first_page_sets_page_and_per_page_and_honours_start_after() {
        let mut p = OffsetPaginator::new();
        let spec = RequestSpec::get("/api/v2/search").query("query", "type:ticket");
        let opts = PageOptions {
            page_size: 50,
            ..PageOptions::default()
        };
        let first = p.first(&spec, &opts).unwrap();
        assert_eq!(first.query_value("page"), Some("1"));
        assert_eq!(first.query_value("per_page"), Some("50"));
        assert_eq!(first.query_value("query"), Some("type:ticket"));
        let opts = PageOptions {
            start_after: Some("7".into()),
            ..PageOptions::default()
        };
        let first = p.first(&spec, &opts).unwrap();
        assert_eq!(first.query_value("page"), Some("7"));
        let opts = PageOptions {
            start_after: Some("101".into()),
            ..PageOptions::default()
        };
        assert_eq!(p.first(&spec, &opts).unwrap_err().exit_code(), 12);
        let deep = RequestSpec::get("/api/v2/tickets")
            .query("page", "501")
            .query("per_page", "10");
        let first = p.first(&deep, &PageOptions::default()).unwrap();
        assert_eq!(first.rate_rules, ["tickets_index_deep"]);
        assert_eq!(first.query_value("per_page"), Some("10"));
    }
}
