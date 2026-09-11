//! `stream_pages` against wiremock: cursor and offset walks, the early offset guards, the
//! search cap, Link-header pagination, `--limit`, checkpoints and single-page mode.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use zdk_core::ZdkError;
use zdk_core::auth::StaticBearer;
use zdk_core::http::{RateGovernor, RequestSpec, RetryPolicy, ZendeskClient};
use zdk_core::pagination::{Checkpoint, Page, PageDialect, PageOptions, stream_pages};

fn client(server: &MockServer) -> Arc<ZendeskClient> {
    Arc::new(
        ZendeskClient::builder(Url::parse(&server.uri()).expect("url"))
            .auth(Arc::new(StaticBearer::new("test-token", "p")))
            .governor(Arc::new(RateGovernor::unlimited()))
            .retry(RetryPolicy::instant(1))
            .build()
            .expect("client"),
    )
}

fn ids(from: u32, to: u32) -> Vec<Value> {
    (from..=to).map(|i| json!({"id": i})).collect()
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

async fn walk(
    server: &MockServer,
    spec: RequestSpec,
    dialect: PageDialect,
    items_key: &str,
    opts: PageOptions,
) -> Vec<Result<Page, ZdkError>> {
    stream_pages(
        client(server),
        spec,
        dialect,
        Some(items_key.to_string()),
        opts,
        CancellationToken::new(),
    )
    .collect()
    .await
}

fn all() -> PageOptions {
    PageOptions {
        all: true,
        progress: false,
        ..PageOptions::default()
    }
}

fn page_of(results: &[Result<Page, ZdkError>], i: usize) -> &Page {
    results[i]
        .as_ref()
        .unwrap_or_else(|e| panic!("page {i}: {e}"))
}

#[tokio::test]
async fn cursor_walk_follows_links_next_then_after_cursor_and_stops_on_has_more_false() {
    let server = MockServer::start().await;
    // Page 1: absolute `links.next` on a *foreign* host — only path and query may survive.
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .and(query_param("page[size]", "2"))
        .respond_with(ok(json!({
            "tickets": ids(1, 2),
            "meta": {"has_more": true, "after_cursor": "c2"},
            "links": {"next": "https://acme.zendesk.com/api/v2/tickets.json?page%5Bsize%5D=2&page%5Bafter%5D=c2"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Page 2: no `links.next`, so `meta.after_cursor` must be used.
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets.json"))
        .and(query_param("page[after]", "c2"))
        .respond_with(ok(json!({
            "tickets": ids(3, 4),
            "meta": {"has_more": true, "after_cursor": "c3"},
            "links": {"next": null}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets.json"))
        .and(query_param("page[after]", "c3"))
        .respond_with(ok(json!({
            "tickets": ids(5, 5),
            "meta": {"has_more": false, "after_cursor": "c4"},
            "links": {"next": null}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let opts = PageOptions {
        page_size: 2,
        ..all()
    };
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/tickets"),
        PageDialect::Cursor,
        "tickets",
        opts,
    )
    .await;
    assert_eq!(pages.len(), 3, "{pages:?}");
    assert_eq!(page_of(&pages, 0).items.len(), 2);
    assert_eq!(page_of(&pages, 1).items.len(), 2);
    assert_eq!(page_of(&pages, 2).items.len(), 1);
    assert_eq!(page_of(&pages, 2).number, 3);
    assert_eq!(page_of(&pages, 2).fetched_total, 5);
    assert!(!page_of(&pages, 2).has_more);
    let requests = server.received_requests().await.expect("recorded");
    assert!(
        requests
            .iter()
            .all(|r| r.url.host_str() != Some("acme.zendesk.com"))
    );
}

#[tokio::test]
async fn offset_walk_stops_on_null_next_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .and(query_param("query", "type:ticket"))
        .respond_with(ok(json!({
            "results": ids(1, 2), "count": 3,
            "next_page": format!("{}/api/v2/search.json?query=type:ticket&page=2", server.uri())
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .and(query_param("page", "2"))
        .respond_with(ok(
            json!({"results": ids(3, 3), "count": 3, "next_page": null}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let spec = RequestSpec::get("/api/v2/search").query("query", "type:ticket");
    let pages = walk(&server, spec, PageDialect::Offset, "results", all()).await;
    assert_eq!(pages.len(), 2, "{pages:?}");
    assert_eq!(page_of(&pages, 0).count, Some(3));
    assert!(page_of(&pages, 0).has_more);
    assert!(!page_of(&pages, 1).has_more);
    assert_eq!(page_of(&pages, 1).fetched_total, 3);
}

#[tokio::test]
async fn offset_guard_fires_before_page_101_is_requested() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations"))
        .and(query_param("page", "100"))
        .respond_with(ok(json!({
            "organizations": ids(1, 1),
            "next_page": format!("{}/api/v2/organizations.json?page=101", server.uri())
        })))
        .expect(1)
        .mount(&server)
        .await;
    let opts = PageOptions {
        start_after: Some("100".into()),
        ..all()
    };
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/organizations"),
        PageDialect::Offset,
        "organizations",
        opts,
    )
    .await;
    assert_eq!(pages.len(), 2, "{pages:?}");
    assert!(pages[0].is_ok());
    let err = pages[1].as_ref().unwrap_err();
    assert_eq!(err.exit_code(), 12, "{err}");
    server.verify().await;
}

#[tokio::test]
async fn offset_count_guard_fails_before_yielding_anything() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .respond_with(ok(json!({
            "results": ids(1, 100), "count": 20_000,
            "next_page": format!("{}/api/v2/search.json?page=2", server.uri())
        })))
        .expect(1)
        .mount(&server)
        .await;
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/search"),
        PageDialect::Offset,
        "results",
        all(),
    )
    .await;
    assert_eq!(pages.len(), 1, "{pages:?}");
    let err = pages[0].as_ref().unwrap_err();
    assert!(matches!(err, ZdkError::PaginationLimit { .. }), "{err}");
    assert!(err.help_text().is_some_and(|h| h.contains("search export")));

    // A bounded walk is fine even when count is huge.
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .respond_with(ok(
            json!({"results": ids(1, 100), "count": 20_000, "next_page": "x"}),
        ))
        .mount(&server)
        .await;
    let opts = PageOptions {
        limit: Some(150),
        ..all()
    };
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/search"),
        PageDialect::Offset,
        "results",
        opts,
    )
    .await;
    assert!(pages.iter().all(Result::is_ok), "{pages:?}");
    assert_eq!(pages.len(), 2);
    assert_eq!(
        page_of(&pages, 1).items.len(),
        50,
        "--limit truncates the second page"
    );
}

/// Ten pages of 100 results, then the documented 422 on the page past the 1,000 cap.
struct SearchPages;

impl Respond for SearchPages {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let page: u32 = request
            .url
            .query_pairs()
            .find(|(k, _)| k == "page")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(1);
        if page > 10 {
            return ResponseTemplate::new(422).set_body_json(json!({
                "error": "InvalidPage",
                "description": "Page number too large: search results are limited to 1000"
            }));
        }
        let from = (page - 1) * 100 + 1;
        ok(json!({
            "results": ids(from, from + 99),
            "count": 4321,
            "next_page": format!("{}?page={}", request.url.path(), page + 1)
        }))
    }
}

#[tokio::test]
async fn search_walk_treats_422_past_the_cap_as_end_of_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .respond_with(SearchPages)
        .expect(11)
        .mount(&server)
        .await;
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/search").query("query", "x"),
        PageDialect::Offset,
        "results",
        all(),
    )
    .await;
    assert!(pages.iter().all(Result::is_ok), "{pages:?}");
    assert_eq!(pages.len(), 10);
    assert_eq!(page_of(&pages, 9).fetched_total, 1000);
}

#[tokio::test]
async fn link_header_walk_for_help_center() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/help_center/en-us/articles"))
        .respond_with(
            ok(json!({"articles": ids(1, 2)})).insert_header(
                "link",
                format!(
                    "<{}/api/v2/help_center/en-us/articles.json?page=2&per_page=30>; rel=\"next\"",
                    server.uri()
                )
                .as_str(),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/help_center/en-us/articles.json"))
        .and(query_param("page", "2"))
        .respond_with(ok(json!({"articles": ids(3, 3)})))
        .expect(1)
        .mount(&server)
        .await;
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/help_center/en-us/articles"),
        PageDialect::Cursor,
        "articles",
        all(),
    )
    .await;
    assert_eq!(pages.len(), 2, "{pages:?}");
    assert!(page_of(&pages, 0).has_more);
    assert!(!page_of(&pages, 1).has_more);
    assert_eq!(page_of(&pages, 1).fetched_total, 3);
}

#[tokio::test]
async fn limit_truncates_mid_page_without_fetching_more() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users"))
        .respond_with(ok(json!({
            "users": ids(1, 5),
            "meta": {"has_more": true, "after_cursor": "c2"},
            "links": {"next": format!("{}/api/v2/users?page%5Bafter%5D=c2", server.uri())}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let opts = PageOptions {
        limit: Some(3),
        ..all()
    };
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/users"),
        PageDialect::Dual,
        "users",
        opts,
    )
    .await;
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert_eq!(page_of(&pages, 0).items.len(), 3);
    assert_eq!(page_of(&pages, 0).fetched_total, 3);
    server.verify().await;
}

#[tokio::test]
async fn single_page_when_all_is_off_and_explicit_page_forces_offset_on_dual() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users"))
        .and(query_param("page[size]", "100"))
        .respond_with(ok(
            json!({"users": ids(1, 2), "meta": {"has_more": true, "after_cursor": "c2"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let opts = PageOptions {
        progress: false,
        ..PageOptions::default()
    };
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/users"),
        PageDialect::Dual,
        "users",
        opts,
    )
    .await;
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert!(
        page_of(&pages, 0).has_more,
        "more exists, but --all was off"
    );

    Mock::given(method("GET"))
        .and(path("/api/v2/users"))
        .and(query_param("page", "3"))
        .and(query_param("per_page", "100"))
        .respond_with(ok(json!({"users": ids(1, 1), "next_page": null})))
        .expect(1)
        .mount(&server)
        .await;
    let pages = walk(
        &server,
        RequestSpec::get("/api/v2/users").query("page", 3),
        PageDialect::Dual,
        "users",
        all(),
    )
    .await;
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert!(!page_of(&pages, 0).has_more);
}

/// Page 1 always works; page `c2` fails with 500 while `fail_c2` is set.
struct Flaky {
    fail_c2: Arc<AtomicBool>,
}

impl Respond for Flaky {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let after = request
            .url
            .query_pairs()
            .find(|(k, _)| k == "page[after]")
            .map(|(_, v)| v.into_owned());
        match after.as_deref() {
            None => {
                ok(json!({"tickets": ids(1, 2), "meta": {"has_more": true, "after_cursor": "c2"}}))
            }
            Some("c2") if self.fail_c2.load(Ordering::SeqCst) => ResponseTemplate::new(500),
            Some("c2") => ok(json!({"tickets": ids(3, 4), "meta": {"has_more": false}})),
            Some(other) => panic!("unexpected cursor {other}"),
        }
    }
}

#[tokio::test]
async fn checkpoint_is_written_per_page_and_resumed_then_removed() {
    let server = MockServer::start().await;
    let fail = Arc::new(AtomicBool::new(true));
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .respond_with(Flaky {
            fail_c2: fail.clone(),
        })
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let cp_path = dir.path().join("walk.json");
    let opts = PageOptions {
        checkpoint: Some(cp_path.clone()),
        ..all()
    };
    let spec = || RequestSpec::get("/api/v2/tickets").query("sort", "-updated_at");

    let pages = walk(
        &server,
        spec(),
        PageDialect::Cursor,
        "tickets",
        opts.clone(),
    )
    .await;
    assert_eq!(pages.len(), 2, "{pages:?}");
    assert!(pages[0].is_ok());
    assert_eq!(pages[1].as_ref().unwrap_err().exit_code(), 9);
    let cp = Checkpoint::load(&cp_path)
        .expect("readable")
        .expect("written after page 1");
    assert_eq!(cp.cursor.as_deref(), Some("c2"));
    assert_eq!(cp.fetched, 2);
    assert_eq!(cp.dialect, "cursor");

    fail.store(false, Ordering::SeqCst);
    let pages = walk(
        &server,
        spec(),
        PageDialect::Cursor,
        "tickets",
        opts.clone(),
    )
    .await;
    assert_eq!(pages.len(), 1, "resumed from c2: {pages:?}");
    let resumed = page_of(&pages, 0);
    assert_eq!(resumed.items[0]["id"], 3);
    assert_eq!(
        resumed.fetched_total, 4,
        "counts the records the first run already emitted"
    );
    assert!(!cp_path.exists(), "a finished walk removes its checkpoint");

    // A checkpoint for a different request is ignored (fresh walk from page 1).
    Checkpoint::for_next(
        "other-hash",
        PageDialect::Cursor,
        &RequestSpec::get("/api/v2/tickets").query("page[after]", "c2"),
        9,
    )
    .save(&cp_path)
    .expect("save");
    let pages = walk(&server, spec(), PageDialect::Cursor, "tickets", opts).await;
    assert_eq!(pages.len(), 2, "{pages:?}");
    assert_eq!(page_of(&pages, 0).items[0]["id"], 1);
}

#[tokio::test]
async fn unsupported_dialects_fail_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    for dialect in [
        PageDialect::Incremental,
        PageDialect::Audits,
        PageDialect::None,
    ] {
        let pages = walk(
            &server,
            RequestSpec::get("/api/v2/incremental/tickets/cursor"),
            dialect,
            "tickets",
            all(),
        )
        .await;
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].as_ref().unwrap_err().exit_code(), 2, "{dialect:?}");
    }
}
