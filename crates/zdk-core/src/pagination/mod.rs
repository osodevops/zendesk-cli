//! The pagination engine (PRD §9, plan A7): one [`Paginator`] per dialect behind
//! [`stream_pages`], which handles `--all`, `--limit` (mid-page), `--checkpoint`, ctrl-c and
//! progress on stderr.

pub mod audits;
pub mod cursor;
pub mod incremental;
pub mod link_header;
pub mod offset;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures_util::{Stream, stream};
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

pub use crate::api::PageDialect;
pub use cursor::CursorPaginator;
pub use offset::OffsetPaginator;

use crate::config::Settings;
use crate::http::{RateHeaders, RequestSpec, ZendeskClient};
use crate::output::progress::Progress;
use crate::{Result, ZdkError};

/// Zendesk's ceiling for `page[size]` / `per_page`.
pub const MAX_PAGE_SIZE: u32 = 100;
/// Query keys that vary between pages and are excluded from [`spec_hash`].
pub const PAGINATION_KEYS: &[&str] = &[
    "page",
    "per_page",
    "page[size]",
    "page[after]",
    "page[before]",
    "cursor",
    "start_time",
];
/// Keys of a list body that never hold the records.
const NON_RECORD_KEYS: &[&str] = &["meta", "links", "facets", "errors", "details", "count"];

/// How to walk.
#[derive(Debug, Clone)]
pub struct PageOptions {
    /// `--all` / `--paginate`: keep going after the first page.
    pub all: bool,
    /// `--limit`: stop after this many records (truncating mid-page).
    pub limit: Option<u64>,
    /// `--page-size` (1–100).
    pub page_size: u32,
    /// `--checkpoint`: write progress here after every page and resume from it.
    pub checkpoint: Option<PathBuf>,
    /// Start from this cursor / page number instead of the beginning.
    pub start_after: Option<String>,
    /// For `Dual` endpoints: cursor (default) or offset.
    pub prefer_cursor: bool,
    /// Show a spinner on stderr (hidden anyway when stderr is not a terminal or `--quiet`).
    pub progress: bool,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            all: false,
            limit: None,
            page_size: MAX_PAGE_SIZE,
            checkpoint: None,
            start_after: None,
            prefer_cursor: true,
            progress: true,
        }
    }
}

impl PageOptions {
    /// From the resolved global flags.
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            all: settings.all,
            limit: settings.limit,
            page_size: settings.page_size.clamp(1, MAX_PAGE_SIZE),
            checkpoint: settings.checkpoint.clone(),
            start_after: None,
            prefer_cursor: true,
            progress: !settings.quiet,
        }
    }
}

/// What a dialect reads off one response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageInfo {
    pub has_more: bool,
    /// `count` when the endpoint reports one (unreliable on search and incremental).
    pub count: Option<u64>,
    pub cursor: Option<String>,
    pub page: Option<u32>,
}

/// One fetched page.
#[derive(Debug, Clone)]
pub struct Page {
    /// 1-based page number within this walk.
    pub number: u32,
    /// The records (already truncated for `--limit`).
    pub items: Vec<Value>,
    /// The whole response body.
    pub body: Value,
    pub status: u16,
    pub headers: HeaderMap,
    pub rate: RateHeaders,
    pub request_id: Option<String>,
    pub has_more: bool,
    pub count: Option<u64>,
    /// Records emitted so far including this page (and a resumed checkpoint's count).
    pub fetched_total: u64,
}

/// One pagination dialect.
pub trait Paginator: Send {
    fn dialect(&self) -> PageDialect;

    /// The request for the first page.
    fn first(&mut self, spec: &RequestSpec, opts: &PageOptions) -> Result<RequestSpec>;

    /// Read `has_more` / `count` / cursor off a response.
    fn inspect(&self, body: &Value, headers: &HeaderMap) -> PageInfo;

    /// The request for the page after `page` (`None` = done). May fail early (offset guard).
    fn next(
        &mut self,
        page: &Page,
        prev: &RequestSpec,
        opts: &PageOptions,
    ) -> Result<Option<RequestSpec>>;

    /// The records of a body.
    fn items(&self, body: &Value, items_key: Option<&str>) -> Vec<Value> {
        extract_items(body, items_key)
    }

    /// A guard evaluated on the first page before it is yielded (offset: `count > 10,000`).
    fn check_first(&self, _page: &Page, _prev: &RequestSpec, _opts: &PageOptions) -> Result<()> {
        Ok(())
    }

    /// Whether a 422 on a follow-up page means "end of results" (search past 1,000).
    fn ends_on_422(&self) -> bool {
        false
    }

    /// Restore dialect state from a checkpoint before resuming.
    fn resume(&mut self, _spec: &RequestSpec, _checkpoint: &Checkpoint) {}
}

/// The paginator for a dialect. `Dual` follows `prefer_cursor`; incremental and audits are
/// not walkable until `zdk sync` (v0.4).
pub fn paginator_for(dialect: PageDialect, prefer_cursor: bool) -> Result<Box<dyn Paginator>> {
    match dialect {
        PageDialect::Cursor => Ok(Box::new(CursorPaginator::new())),
        PageDialect::Offset => Ok(Box::new(OffsetPaginator::new())),
        PageDialect::Dual => Ok(if prefer_cursor {
            Box::new(CursorPaginator::new())
        } else {
            Box::new(OffsetPaginator::new())
        }),
        PageDialect::Incremental => Err(incremental::unsupported()),
        PageDialect::Audits => Err(audits::unsupported()),
        PageDialect::None => Err(ZdkError::Usage(
            "this endpoint is not paginated (the registry lists no pagination dialect); drop --paginate/--all".into(),
        )),
    }
}

/// The records of a list body: `body[items_key]`, else the first top-level array that is not
/// envelope metadata, else nothing.
#[must_use]
pub fn extract_items(body: &Value, items_key: Option<&str>) -> Vec<Value> {
    match body {
        Value::Array(a) => a.clone(),
        Value::Object(map) => {
            if let Some(key) = items_key {
                return map
                    .get(key)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
            }
            detect_items_key(body)
                .and_then(|k| map.get(&k).and_then(Value::as_array).cloned())
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// The first top-level key holding an array of records, ignoring envelope metadata.
#[must_use]
pub fn detect_items_key(body: &Value) -> Option<String> {
    body.as_object()?
        .iter()
        .find(|(k, v)| v.is_array() && !NON_RECORD_KEYS.contains(&k.as_str()))
        .map(|(k, _)| k.clone())
}

/// A stable fingerprint of "which list is this" (method, path, non-pagination query), so a
/// checkpoint is only ever resumed for the same request.
#[must_use]
pub fn spec_hash(spec: &RequestSpec) -> String {
    let mut query: Vec<&(String, String)> = spec
        .query
        .iter()
        .filter(|(k, _)| !PAGINATION_KEYS.contains(&k.as_str()))
        .collect();
    query.sort();
    let mut hasher = Sha256::new();
    hasher.update(spec.method.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(crate::api::template::normalize(&spec.path).as_bytes());
    for (k, v) in query {
        hasher.update(b"\n");
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Written after every page of a `--checkpoint` walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub spec_hash: String,
    pub dialect: String,
    /// `page[after]` / `cursor` of the next request, when the dialect uses one.
    pub cursor: Option<String>,
    /// `page` of the next request, when the dialect uses one.
    pub page: Option<u32>,
    /// Records emitted so far.
    pub fetched: u64,
    /// The next request to send.
    pub next_path: String,
    pub next_query: Vec<(String, String)>,
    pub updated_at: DateTime<Utc>,
}

impl Checkpoint {
    /// Build for the request that would be sent next.
    #[must_use]
    pub fn for_next(hash: &str, dialect: PageDialect, next: &RequestSpec, fetched: u64) -> Self {
        Self {
            spec_hash: hash.to_string(),
            dialect: dialect.as_str().to_string(),
            cursor: next
                .query_value("page[after]")
                .or_else(|| next.query_value("cursor"))
                .map(str::to_string),
            page: next.query_value("page").and_then(|p| p.parse().ok()),
            fetched,
            next_path: next.path.clone(),
            next_query: next.query.clone(),
            updated_at: Utc::now(),
        }
    }

    /// `Ok(None)` when the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
                ZdkError::Usage(format!(
                    "checkpoint {} is not a valid zdk checkpoint: {e}",
                    path.display()
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ZdkError::Io(e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        crate::util::fs::atomic_write_0600(path, text.as_bytes()).map_err(ZdkError::Io)
    }

    /// Apply to a spec: the next request's path and query.
    pub fn apply(&self, spec: &mut RequestSpec) {
        spec.path.clone_from(&self.next_path);
        spec.query.clone_from(&self.next_query);
    }
}

/// Walk `spec` page by page. Each item is one [`Page`]; the stream ends after the last page,
/// after `--limit` records, after a single page when `opts.all` is false, or with one error.
#[allow(clippy::needless_pass_by_value)] // an owned spec keeps the returned stream `'static`
pub fn stream_pages(
    client: Arc<ZendeskClient>,
    spec: RequestSpec,
    dialect: PageDialect,
    items_key: Option<String>,
    opts: PageOptions,
    cancel: CancellationToken,
) -> impl Stream<Item = Result<Page>> + Send {
    let state = match Walk::start(client, &spec, dialect, items_key, opts, cancel) {
        Ok(walk) => WalkState::Running(Box::new(walk)),
        Err(e) => WalkState::Failed(e),
    };
    stream::unfold(state, |state| async move {
        match state {
            WalkState::Done => None,
            WalkState::Failed(e) => Some((Err(e), WalkState::Done)),
            WalkState::Running(mut walk) => match walk.step().await {
                Ok(Some(page)) => Some((Ok(page), WalkState::Running(walk))),
                Ok(None) => None,
                Err(e) => Some((Err(e), WalkState::Done)),
            },
        }
    })
}

enum WalkState {
    Running(Box<Walk>),
    Failed(ZdkError),
    Done,
}

struct Walk {
    client: Arc<ZendeskClient>,
    paginator: Box<dyn Paginator>,
    dialect: PageDialect,
    next: Option<RequestSpec>,
    /// An error from computing the next request, surfaced after the current page is yielded.
    pending_error: Option<ZdkError>,
    opts: PageOptions,
    items_key: Option<String>,
    cancel: CancellationToken,
    hash: String,
    page_number: u32,
    /// Emitted in this run (drives `--limit`).
    emitted: u64,
    /// Emitted including what a resumed checkpoint already covered.
    fetched_total: u64,
    resumed: bool,
    wrote_checkpoint: bool,
    progress: Option<Progress>,
}

impl Walk {
    fn start(
        client: Arc<ZendeskClient>,
        spec: &RequestSpec,
        dialect: PageDialect,
        items_key: Option<String>,
        opts: PageOptions,
        cancel: CancellationToken,
    ) -> Result<Self> {
        // An explicit `page=` on a dual endpoint means the caller chose offset.
        let dialect = if dialect == PageDialect::Dual && spec.query_value("page").is_some() {
            PageDialect::Offset
        } else {
            dialect
        };
        let mut paginator = paginator_for(dialect, opts.prefer_cursor)?;
        let effective = paginator.dialect();
        let hash = spec_hash(spec);
        let mut fetched_total = 0;
        let mut resumed = false;

        let checkpoint = match opts.checkpoint.as_deref().map(Checkpoint::load) {
            Some(Ok(cp)) => cp,
            Some(Err(e)) => {
                crate::output::warn(&format!("warning: {e}; starting from the first page"));
                None
            }
            None => None,
        };
        let first = match checkpoint {
            Some(cp) if cp.spec_hash == hash && cp.dialect == effective.as_str() => {
                crate::output::warn(&format!(
                    "resuming from checkpoint ({} records already fetched, updated {})",
                    cp.fetched,
                    cp.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
                ));
                let mut s = spec
                    .try_clone()
                    .ok_or_else(|| ZdkError::Usage("cannot paginate a multipart request".into()))?;
                cp.apply(&mut s);
                paginator.resume(&s, &cp);
                fetched_total = cp.fetched;
                resumed = true;
                s
            }
            Some(cp) => {
                crate::output::warn(&format!(
                    "warning: checkpoint {} belongs to a different request ({} {}); starting from the first page",
                    opts.checkpoint
                        .as_deref()
                        .map(Path::display)
                        .map_or_else(String::new, |p| p.to_string()),
                    cp.dialect,
                    cp.next_path
                ));
                paginator.first(spec, &opts)?
            }
            None => paginator.first(spec, &opts)?,
        };

        let progress = opts.progress.then(|| Progress::spinner("fetching page 1"));
        Ok(Self {
            client,
            paginator,
            dialect: effective,
            next: Some(first),
            pending_error: None,
            opts,
            items_key,
            cancel,
            hash,
            page_number: 0,
            emitted: 0,
            fetched_total,
            resumed,
            wrote_checkpoint: false,
            progress,
        })
    }

    fn write_checkpoint(&mut self, next: &RequestSpec) {
        let Some(path) = self.opts.checkpoint.clone() else {
            return;
        };
        let cp = Checkpoint::for_next(&self.hash, self.dialect, next, self.fetched_total);
        match cp.save(&path) {
            Ok(()) => self.wrote_checkpoint = true,
            Err(e) => crate::output::warn(&format!(
                "warning: cannot write checkpoint {}: {e}",
                path.display()
            )),
        }
    }

    /// The walk completed: a checkpoint would only replay the end, so remove it.
    fn finish(&mut self) {
        if let Some(p) = &self.progress {
            p.finish();
        }
        if (self.wrote_checkpoint || self.resumed)
            && let Some(path) = &self.opts.checkpoint
            && let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(error = %e, "could not remove finished checkpoint");
        }
    }

    async fn step(&mut self) -> Result<Option<Page>> {
        if let Some(e) = self.pending_error.take() {
            return Err(e);
        }
        let Some(spec) = self.next.take() else {
            self.finish();
            return Ok(None);
        };
        let prev = spec
            .try_clone()
            .ok_or_else(|| ZdkError::Usage("cannot paginate a multipart request".into()))?;
        if self.cancel.is_cancelled() {
            self.write_checkpoint(&prev);
            return Err(ZdkError::Interrupted);
        }
        let result = tokio::select! {
            biased;
            () = self.cancel.cancelled() => {
                self.write_checkpoint(&prev);
                return Err(ZdkError::Interrupted);
            }
            r = self.client.execute(spec) => r,
        };
        let resp = match result {
            Ok(r) => r,
            Err(ZdkError::Validation { status: 422, .. }) if self.paginator.ends_on_422() => {
                crate::output::warn(&format!(
                    "warning: Zendesk refused page {} (422): the search result set is capped at {} records; use `zdk search export` for the full set",
                    self.page_number + 1,
                    offset::SEARCH_CAP
                ));
                self.finish();
                return Ok(None);
            }
            Err(e) => {
                self.write_checkpoint(&prev);
                return Err(e);
            }
        };

        let body = resp.value()?;
        let info = self.paginator.inspect(&body, &resp.headers);
        let mut items = self.paginator.items(&body, self.items_key.as_deref());
        self.page_number += 1;

        let mut truncated = false;
        if let Some(limit) = self.opts.limit {
            let room = limit.saturating_sub(self.emitted);
            let room = usize::try_from(room).unwrap_or(usize::MAX);
            if items.len() > room {
                items.truncate(room);
                truncated = true;
            }
        }
        let n = items.len() as u64;
        self.emitted += n;
        self.fetched_total += n;

        let page = Page {
            number: self.page_number,
            items,
            body,
            status: resp.status,
            headers: resp.headers,
            rate: resp.rate,
            request_id: resp.request_id,
            has_more: info.has_more,
            count: info.count,
            fetched_total: self.fetched_total,
        };
        if self.page_number == 1 && !self.resumed {
            self.paginator.check_first(&page, &prev, &self.opts)?;
        }

        let limit_reached = truncated || self.opts.limit.is_some_and(|l| self.emitted >= l);
        if limit_reached || !self.opts.all {
            self.next = None;
            if let Some(p) = &self.progress {
                p.finish();
            }
            if !limit_reached {
                self.finish();
            }
        } else {
            // A guard error here (page 101, count > 10,000) is surfaced *after* this page.
            self.next = match self.paginator.next(&page, &prev, &self.opts) {
                Ok(next) => next,
                Err(e) => {
                    self.pending_error = Some(e);
                    None
                }
            };
            match &self.next {
                Some(next) => {
                    let next_spec = next.try_clone().ok_or_else(|| {
                        ZdkError::Usage("cannot paginate a multipart request".into())
                    })?;
                    self.write_checkpoint(&next_spec);
                    if let Some(p) = &self.progress {
                        p.set_message(format!(
                            "fetched {} records ({} pages), fetching page {}",
                            self.fetched_total,
                            self.page_number,
                            self.page_number + 1
                        ));
                    }
                }
                None if self.pending_error.is_none() => self.finish(),
                None => {}
            }
        }
        Ok(Some(page))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_items_prefers_the_key_then_the_first_record_array() {
        let body = json!({"meta": {"has_more": false}, "links": {"next": null}, "count": 2, "tickets": [{"id": 1}, {"id": 2}], "users": [{"id": 9}]});
        assert_eq!(extract_items(&body, Some("users")).len(), 1);
        assert_eq!(extract_items(&body, None).len(), 2);
        assert_eq!(detect_items_key(&body).as_deref(), Some("tickets"));
        assert!(extract_items(&body, Some("nope")).is_empty());
        assert_eq!(extract_items(&json!([1, 2, 3]), None).len(), 3);
        assert!(extract_items(&json!({"ticket": {"id": 1}}), None).is_empty());
        assert!(detect_items_key(&json!("x")).is_none());
    }

    #[test]
    fn spec_hash_ignores_pagination_keys_and_order() {
        let a = RequestSpec::get("/api/v2/tickets")
            .query("sort", "x")
            .query("page[size]", "5")
            .query("page[after]", "c");
        let b = RequestSpec::get("/api/v2/tickets.json")
            .query("page", "9")
            .query("sort", "x");
        assert_eq!(spec_hash(&a), spec_hash(&b));
        let c = RequestSpec::get("/api/v2/tickets").query("sort", "y");
        assert_ne!(spec_hash(&a), spec_hash(&c));
        assert_ne!(
            spec_hash(&a),
            spec_hash(&RequestSpec::delete("/api/v2/tickets"))
        );
        assert_eq!(spec_hash(&a).len(), 64);
    }

    #[test]
    fn checkpoint_round_trip_and_apply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cp.json");
        assert!(Checkpoint::load(&path).unwrap().is_none());
        let next = RequestSpec::get("/api/v2/tickets")
            .query("page[size]", "100")
            .query("page[after]", "c7");
        let cp = Checkpoint::for_next("h", PageDialect::Cursor, &next, 300);
        assert_eq!(cp.cursor.as_deref(), Some("c7"));
        assert_eq!(cp.page, None);
        cp.save(&path).unwrap();
        let back = Checkpoint::load(&path).unwrap().unwrap();
        assert_eq!(back, cp);
        let mut spec = RequestSpec::get("/api/v2/tickets").query("page[size]", "100");
        back.apply(&mut spec);
        assert_eq!(spec.query_value("page[after]"), Some("c7"));
        let offset = Checkpoint::for_next(
            "h",
            PageDialect::Offset,
            &RequestSpec::get("/x").query("page", "4"),
            1,
        );
        assert_eq!(offset.page, Some(4));
        std::fs::write(&path, "nope").unwrap();
        assert_eq!(Checkpoint::load(&path).unwrap_err().exit_code(), 2);
    }

    #[test]
    fn paginator_for_maps_dialects() {
        assert_eq!(
            paginator_for(PageDialect::Cursor, true).unwrap().dialect(),
            PageDialect::Cursor
        );
        assert_eq!(
            paginator_for(PageDialect::Dual, true).unwrap().dialect(),
            PageDialect::Cursor
        );
        assert_eq!(
            paginator_for(PageDialect::Dual, false).unwrap().dialect(),
            PageDialect::Offset
        );
        assert_eq!(
            paginator_for(PageDialect::Offset, true).unwrap().dialect(),
            PageDialect::Offset
        );
        for d in [
            PageDialect::Incremental,
            PageDialect::Audits,
            PageDialect::None,
        ] {
            let err = paginator_for(d, true).err().expect("unsupported dialect");
            assert_eq!(err.exit_code(), 2, "{d:?}");
        }
        let err = paginator_for(PageDialect::Incremental, true)
            .err()
            .expect("unsupported");
        assert!(err.to_string().contains("zdk sync"));
    }
}
