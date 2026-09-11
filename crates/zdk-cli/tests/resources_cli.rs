//! Black-box tests for the curated resource commands (`tickets`, `comments`, `users`, `orgs`,
//! `search`) against a wiremock Zendesk. Fixtures live under the workspace `tests/fixtures/`.

mod common;

use common::{Harness, json, stderr_error};
use predicates::prelude::*;
use wiremock::matchers::{body_json, body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

macro_rules! fixture {
    ($rel:literal) => {
        include_str!(concat!("../../../tests/fixtures/", $rel))
    };
}

fn json_response(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_string(body)
        .insert_header("content-type", "application/json; charset=utf-8")
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::to_string)
        .collect()
}

/// A base URL nothing listens on: usage errors must surface before any request.
const UNREACHABLE: &str = "http://127.0.0.1:1";

// ---------------------------------------------------------------------------------------------
// tickets: reads
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tickets_list_streams_cursor_pages_into_one_json_array() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .and(query_param("page[after]", "cursor-page-2"))
        .respond_with(json_response(200, fixture!("tickets/list_page2.json")))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .and(query_param("page[size]", "100"))
        .respond_with(json_response(200, fixture!("tickets/list_page1.json")))
        .expect(2)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "list", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let rows = v.as_array().expect("array");
    assert_eq!(rows.len(), 3, "{v}");
    let ids: Vec<u64> = rows.iter().filter_map(|r| r["id"].as_u64()).collect();
    assert_eq!(ids, [1, 2, 3]);
    assert_eq!(rows[0]["custom_fields"][0]["value"], "production");

    // Without --all only the first page is fetched.
    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out).as_array().expect("array").len(), 2);
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_list_table_snapshot_and_sideload_join() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets"))
        .and(query_param("include", "users,groups"))
        .respond_with(json_response(200, fixture!("tickets/list_sideloaded.json")))
        .expect(2)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .env("TZ", "UTC")
        .args([
            "tickets",
            "list",
            "--sideload",
            "users,groups",
            "-o",
            "table",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let table = String::from_utf8_lossy(&out);
    assert!(
        table.contains("Ada Lovelace"),
        "joined assignee name:\n{table}"
    );
    insta::assert_snapshot!("tickets_list_table", table);

    let out = h
        .zdk_api(&server.uri())
        .args([
            "tickets",
            "list",
            "--sideload",
            "users,groups",
            "--fields",
            "id,assignee.name,group.name",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!([
            {"id": 1, "assignee": {"name": "Ada Lovelace"}, "group": {"name": "Platform Support"}},
            {"id": 2, "assignee": {"name": null}, "group": {"name": null}}
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_list_with_filters_compiles_a_search_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .respond_with(json_response(200, fixture!("search/results.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "tickets",
            "list",
            "--status",
            "open",
            "--assignee",
            "42",
            "--tag",
            "urgent",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out).as_array().expect("array").len(), 2);

    let received = server.received_requests().await.expect("recording");
    assert_eq!(received.len(), 1);
    let query = received[0]
        .url
        .query_pairs()
        .find(|(k, _)| k == "query")
        .map(|(_, v)| v.into_owned())
        .expect("query param");
    for term in ["type:ticket", "status:open", "assignee:42", "tags:urgent"] {
        assert!(query.contains(term), "{query}");
    }
    assert_eq!(received[0].url.path(), "/api/v2/search");
    assert!(
        received[0]
            .url
            .query_pairs()
            .any(|(k, v)| k == "per_page" && v == "100"),
        "offset pagination on search"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_list_filters_with_all_walks_search_export() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search/export"))
        .and(query_param("page[after]", "export-cursor-2"))
        .respond_with(json_response(200, fixture!("search/export_page2.json")))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search/export"))
        .and(query_param("filter[type]", "ticket"))
        .respond_with(json_response(200, fixture!("search/export_page1.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "tickets", "list", "--status", "open", "--all", "-o", "ndjson",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(lines(&out).len(), 3);
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_get_joins_sideloads_and_embeds_comments() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/1"))
        .and(query_param("include", "users"))
        .respond_with(json_response(
            200,
            fixture!("tickets/ticket_sideloaded.json"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/1"))
        .respond_with(json_response(200, fixture!("tickets/ticket.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/1/comments"))
        .and(query_param("include", "users"))
        .respond_with(json_response(200, fixture!("tickets/comments.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "tickets",
            "get",
            "1",
            "--sideload",
            "users",
            "--fields",
            "id,assignee.name",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!({"id": 1, "assignee": {"name": "Ada Lovelace"}})
    );

    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "get", "1", "--with-comments"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["id"], 1);
    let comments = v["comments"].as_array().expect("comments");
    assert_eq!(comments.len(), 3);
    assert_eq!(comments[1]["author"]["name"], "Ada Lovelace");
    assert_eq!(comments[1]["public"], false);
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_show_renders_a_transcript() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/1"))
        .and(query_param("include", "users,groups,organizations"))
        .respond_with(json_response(
            200,
            fixture!("tickets/ticket_sideloaded.json"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/1/comments"))
        .respond_with(json_response(200, fixture!("tickets/comments.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .env("TZ", "UTC")
        .args(["tickets", "show", "1", "-o", "table"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("#1  API latency on eu-west-1\n"), "{text}");
    assert!(
        text.contains("requester: Grace Hopper <grace@example.com>"),
        "{text}"
    );
    assert!(text.contains("group: Platform Support   organization: Acme Corp"));
    assert!(text.contains("#2  Ada Lovelace  2026-09-10 09:02  [internal]\nInvestigating"));
    assert!(text.contains("#3  Ada Lovelace  2026-09-11 06:34  [public]"));
    insta::assert_snapshot!("tickets_show_transcript", text);
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_count_plain_and_filtered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/count"))
        .respond_with(json_response(200, fixture!("tickets/count.json")))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search/count"))
        .respond_with(json_response(200, fixture!("search/count.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    h.zdk_api(&server.uri())
        .args(["tickets", "count", "-o", "table"])
        .assert()
        .success()
        .stdout("1234\n");
    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "count"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["count"], 1234);
    assert_eq!(v["refreshed_at"], "2026-09-11T06:00:00Z");

    h.zdk_api(&server.uri())
        .args(["tickets", "count", "--status", "pending", "-o", "table"])
        .assert()
        .success()
        .stdout("57\n");
    let received = server.received_requests().await.expect("recording");
    let search = received
        .iter()
        .find(|r| r.url.path() == "/api/v2/search/count")
        .expect("search count request");
    let query = search
        .url
        .query_pairs()
        .find(|(k, _)| k == "query")
        .map(|(_, v)| v.into_owned())
        .expect("query");
    assert_eq!(query, "type:ticket status:pending");
}

// ---------------------------------------------------------------------------------------------
// tickets: writes
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tickets_create_posts_the_ticket_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .and(body_partial_json(serde_json::json!({
            "ticket": {
                "subject": "S",
                "comment": {"body": "C", "public": true},
                "requester": {"email": "a@b.c"},
                "priority": "high",
                "tags": ["latency"],
                "custom_fields": [{"id": 360_000_001, "value": "production"}],
                "type": "incident"
            }
        })))
        .respond_with(json_response(201, fixture!("tickets/ticket.json")))
        .expect(2)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "tickets",
            "create",
            "--subject",
            "S",
            "--comment",
            "C",
            "--requester",
            "a@b.c",
            "--priority",
            "high",
            "--tag",
            "latency",
            "--custom-field",
            "360000001=production",
            "--field",
            "type=incident",
            "-o",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(json(&out.stdout)["id"], 1);
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = h
        .zdk_api(&server.uri())
        .args([
            "tickets",
            "create",
            "--subject",
            "S",
            "--comment",
            "C",
            "--requester",
            "a@b.c",
            "--priority",
            "high",
            "--tag",
            "latency",
            "--custom-field",
            "360000001=production",
            "--field",
            "type=incident",
            "-o",
            "table",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Created ticket #1"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("FIELD"));
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_create_from_file_merges_flags_and_fields_last() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .and(body_json(serde_json::json!({
            "ticket": {
                "subject": "from flag",
                "comment": {"body": "from file", "public": false},
                "priority": "urgent",
                "tags": ["file"]
            }
        })))
        .respond_with(json_response(201, fixture!("tickets/ticket.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let doc = h.home.join("ticket.json");
    std::fs::write(
        &doc,
        r#"{"ticket": {"subject": "from file", "comment": {"body": "from file", "public": false}, "priority": "low", "tags": ["file"]}}"#,
    )
    .expect("write");
    h.zdk_api(&server.uri())
        .args([
            "tickets",
            "create",
            "--file",
            doc.to_str().expect("utf8"),
            "--subject",
            "from flag",
            "--priority",
            "high",
            "--field",
            "priority=urgent",
        ])
        .assert()
        .success();
    server.verify().await;

    // Without any comment source the command refuses before any request.
    h.zdk_api(UNREACHABLE)
        .args(["tickets", "create", "--subject", "S"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--comment"));
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_update_puts_fields_and_tags_separately() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1"))
        .and(body_json(
            serde_json::json!({"ticket": {"status": "pending"}}),
        ))
        .respond_with(json_response(200, fixture!("tickets/ticket.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1/tags"))
        .and(body_json(serde_json::json!({"tags": ["x"]})))
        .respond_with(json_response(
            200,
            r#"{"tags": ["latency", "eu-west-1", "x"]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/tickets/1/tags"))
        .and(body_json(serde_json::json!({"tags": ["latency"]})))
        .respond_with(json_response(200, r#"{"tags": ["eu-west-1"]}"#))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "tickets",
            "update",
            "1",
            "--status",
            "pending",
            "--add-tag",
            "x",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["id"], 1);
    assert_eq!(v["tags"], serde_json::json!(["latency", "eu-west-1", "x"]));

    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "update", "1", "--remove-tag", "latency"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!({"id": 1, "tags": ["eu-west-1"]})
    );

    h.zdk_api(UNREACHABLE)
        .args(["tickets", "update", "1"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("nothing to update"));
    h.zdk_api(UNREACHABLE)
        .args(["tickets", "update", "1", "--safe-update"])
        .assert()
        .code(2);
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_reply_note_solve_and_assign_put_the_right_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1"))
        .and(body_json(serde_json::json!({
            "ticket": {"comment": {"body": "hi", "public": true}}
        })))
        .respond_with(json_response(200, fixture!("tickets/ticket.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1"))
        .and(body_json(serde_json::json!({
            "ticket": {"comment": {"body": "internal\n", "public": false, "author_id": 42}}
        })))
        .respond_with(json_response(200, fixture!("tickets/ticket.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1"))
        .and(body_json(serde_json::json!({
            "ticket": {"status": "solved", "comment": {"body": "done", "public": true}}
        })))
        .respond_with(json_response(200, fixture!("tickets/ticket.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1"))
        .and(body_json(serde_json::json!({"ticket": {"status": "open"}})))
        .respond_with(json_response(200, fixture!("tickets/ticket.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(json_response(200, fixture!("users/me.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1"))
        .and(body_json(serde_json::json!({
            "ticket": {"assignee_id": 42, "group_id": 501}
        })))
        .respond_with(json_response(200, fixture!("tickets/ticket.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    h.zdk_api(&server.uri())
        .args(["tickets", "reply", "1", "--body", "hi"])
        .assert()
        .success();
    let note = h.home.join("note.md");
    std::fs::write(&note, "internal\n").expect("write");
    h.zdk_api(&server.uri())
        .args([
            "tickets",
            "note",
            "1",
            "--body-file",
            note.to_str().expect("utf8"),
            "--author-id",
            "42",
        ])
        .assert()
        .success();
    h.zdk_api(&server.uri())
        .args(["tickets", "solve", "1", "--body", "done", "-o", "table"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Ticket #1 is now solved"));
    h.zdk_api(&server.uri())
        .args(["tickets", "reopen", "1"])
        .assert()
        .success();
    h.zdk_api(&server.uri())
        .args(["tickets", "assign", "1", "--to", "me", "--group-id", "501"])
        .assert()
        .success();
    // A reply needs a body source; `--editor` without an editor is a usage error.
    h.zdk_api(UNREACHABLE)
        .args(["tickets", "reply", "1"])
        .assert()
        .code(2);
    h.zdk_api(UNREACHABLE)
        .args(["tickets", "reply", "1", "--editor"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("EDITOR"));
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_delete_confirms_restores_and_purges() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/tickets/1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/deleted_tickets/1/restore"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/deleted_tickets/1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    // Non-TTY without --yes: exit 2, nothing sent.
    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "delete", "1"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    let err = stderr_error(&out.stderr);
    assert_eq!(err["error"]["code"], "USAGE");
    assert!(
        err["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("--yes") && m.contains("profile 'default'")),
        "{err}"
    );
    assert_eq!(server.received_requests().await.map(|r| r.len()), Some(0));

    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "delete", "1", "--yes"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out), serde_json::json!({"id": 1, "deleted": true}));
    h.zdk_api(&server.uri())
        .args(["tickets", "restore", "1", "-o", "table"])
        .assert()
        .success()
        .stdout("Restored ticket #1\n");
    h.zdk_api(&server.uri())
        .args(["tickets", "permanently-delete", "1"])
        .assert()
        .code(2);
    h.zdk_api(&server.uri())
        .args(["tickets", "permanently-delete", "1", "-y", "-o", "table"])
        .assert()
        .success()
        .stdout("Permanently deleted ticket #1\n");
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_dry_run_sends_nothing_and_exits_zero() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/tickets/1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "tickets",
            "create",
            "--subject",
            "S",
            "--comment",
            "C",
            "--requester",
            "a@b.c",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let v = json(&out.stdout);
    assert_eq!(v["method"], "POST");
    assert_eq!(v["body"]["ticket"]["requester"]["email"], "a@b.c");
    assert!(out.stderr.is_empty());
    // Destructive + dry-run: no confirmation needed, nothing sent.
    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "delete", "1", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["method"], "DELETE");
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_errors_map_to_exit_codes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/999"))
        .respond_with(
            json_response(404, fixture!("errors/404.json")).insert_header("x-request-id", "rid-1"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1"))
        .respond_with(json_response(403, fixture!("errors/403.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/tickets"))
        .respond_with(json_response(422, fixture!("errors/422.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "get", "999"])
        .assert()
        .code(5)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    let err = stderr_error(&out.stderr);
    assert_eq!(err["error"]["code"], "NOT_FOUND");
    assert_eq!(err["error"]["request_id"], "rid-1");
    assert!(
        err["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("ticket '999' not found")),
        "{err}"
    );

    // The registry knows UpdateTicket needs tickets:write, so a 403 names that scope even
    // when the token's grants are unknown (static token); the help says "Granted: (unknown)".
    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "update", "1", "--status", "open"])
        .assert()
        .code(4)
        .get_output()
        .clone();
    let err = stderr_error(&out.stderr);
    assert_eq!(err["error"]["code"], "SCOPE_MISSING");
    assert_eq!(err["error"]["exit_code"], 4);
    assert!(
        err["error"]["help"]
            .as_str()
            .is_some_and(|h| h.contains("tickets:write") && h.contains("(unknown)")),
        "{err}"
    );

    let out = h
        .zdk_api(&server.uri())
        .args(["tickets", "create", "--comment", "x", "--requester", "1"])
        .assert()
        .code(6)
        .get_output()
        .clone();
    let err = stderr_error(&out.stderr);
    assert_eq!(err["error"]["code"], "VALIDATION");
    assert!(
        err["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("RecordInvalid")),
        "{err}"
    );

    // Bad filter values never reach the network.
    h.zdk_api(UNREACHABLE)
        .args(["tickets", "list", "--status", "done"])
        .assert()
        .code(2);
    h.zdk_api(UNREACHABLE)
        .args(["tickets", "list", "--assignee", "Ada Lovelace"])
        .assert()
        .code(2);
}

// ---------------------------------------------------------------------------------------------
// comments
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn comments_list_get_count_and_writes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/1/comments"))
        .and(query_param("include", "users"))
        .respond_with(json_response(200, fixture!("tickets/comments.json")))
        .expect(4)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/tickets/1/comments/count"))
        .respond_with(json_response(
            200,
            r#"{"count": {"value": 3, "refreshed_at": "2026-09-11T06:00:00Z"}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1/comments/9003/make_private"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/tickets/1/comments/9001/redact"))
        .and(body_json(serde_json::json!({"text": "5 seconds"})))
        .respond_with(json_response(
            200,
            r#"{"comment": {"id": 9001, "body": "redacted"}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["comments", "list", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let all = json(&out);
    assert_eq!(all.as_array().expect("array").len(), 3);
    assert_eq!(all[0]["author"]["name"], "Grace Hopper");

    let out = h
        .zdk_api(&server.uri())
        .args(["comments", "list", "1", "--public-only"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let public = json(&out);
    let ids: Vec<u64> = public
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|c| c["id"].as_u64())
        .collect();
    assert_eq!(ids, [9001, 9003]);

    let out = h
        .zdk_api(&server.uri())
        .args(["comments", "get", "1", "--comment", "9002"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["body"], "Investigating with the platform team.");
    let out = h
        .zdk_api(&server.uri())
        .args(["comments", "get", "1", "--comment", "4"])
        .assert()
        .code(5)
        .get_output()
        .clone();
    assert_eq!(stderr_error(&out.stderr)["error"]["code"], "NOT_FOUND");

    h.zdk_api(&server.uri())
        .args(["comments", "count", "1", "-o", "table"])
        .assert()
        .success()
        .stdout("3\n");
    h.zdk_api(&server.uri())
        .args([
            "comments",
            "make-private",
            "1",
            "--comment",
            "9003",
            "-o",
            "table",
        ])
        .assert()
        .success()
        .stdout("Comment 9003 on ticket #1 is now private\n");
    h.zdk_api(&server.uri())
        .args([
            "comments",
            "redact",
            "1",
            "--comment",
            "9001",
            "--text",
            "5 seconds",
        ])
        .assert()
        .code(2);
    let out = h
        .zdk_api(&server.uri())
        .args([
            "comments",
            "redact",
            "1",
            "--comment",
            "9001",
            "--text",
            "5 seconds",
            "--yes",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["comment"]["id"], 9001);
    server.verify().await;
}

// ---------------------------------------------------------------------------------------------
// users
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn users_list_search_get_and_me() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users"))
        .and(query_param("role[]", "agent"))
        .and(query_param("role[]", "admin"))
        .respond_with(json_response(200, fixture!("users/list.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations/1001/users"))
        .respond_with(json_response(200, fixture!("users/list.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/search"))
        .and(query_param("query", "foo"))
        .respond_with(json_response(200, fixture!("users/search_email.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/search"))
        .and(query_param("query", "email:grace@example.com"))
        .respond_with(json_response(200, fixture!("users/search_email.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/search"))
        .and(query_param("external_id", "CRM-0042"))
        .respond_with(json_response(200, fixture!("users/list.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(json_response(200, fixture!("users/me.json")))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/43"))
        .respond_with(json_response(200, fixture!("users/user.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/42"))
        .respond_with(json_response(200, fixture!("users/user.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["users", "list", "--role", "agent,admin"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out).as_array().expect("array").len(), 3);
    h.zdk_api(&server.uri())
        .args(["users", "list", "--organization-id", "1001", "-o", "table"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("NAME")
                .and(predicate::str::contains("EMAIL"))
                .and(predicate::str::contains("Grace Hopper")),
        );
    let out = h
        .zdk_api(&server.uri())
        .args(["users", "search", "foo"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)[0]["email"], "grace@example.com");
    h.zdk_api(&server.uri())
        .args(["users", "search", "--external-id", "CRM-0042"])
        .assert()
        .success();
    h.zdk_api(UNREACHABLE)
        .args(["users", "search"])
        .assert()
        .code(2);

    let out = h
        .zdk_api(&server.uri())
        .args(["users", "get", "me"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["id"], 42);
    let out = h
        .zdk_api(&server.uri())
        .args(["users", "me", "--fields", "id,email"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!({"id": 42, "email": "ada@example.com"})
    );
    let out = h
        .zdk_api(&server.uri())
        .args(["users", "get", "grace@example.com"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["id"], 42, "resolved 43 via search, then fetched");
    h.zdk_api(&server.uri())
        .args(["users", "get", "42"])
        .assert()
        .success();
    h.zdk_api(UNREACHABLE)
        .args(["users", "get", "Ada Lovelace"])
        .assert()
        .code(2);
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn users_create_update_and_delete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/users"))
        .and(body_json(serde_json::json!({
            "user": {
                "name": "Jane Doe",
                "email": "jane@example.com",
                "role": "end-user",
                "verified": true,
                "organization_id": 1001,
                "user_fields": {"tier": "gold"}
            }
        })))
        .respond_with(json_response(201, fixture!("users/user.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/users/create_or_update"))
        .and(body_json(serde_json::json!({
            "user": {"name": "Jane Doe", "email": "jane@example.com"}
        })))
        .respond_with(json_response(200, fixture!("users/user.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/users/42"))
        .and(body_json(serde_json::json!({
            "user": {"name": "Jane Smith", "suspended": true}
        })))
        .respond_with(json_response(200, fixture!("users/user.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/users/42"))
        .respond_with(json_response(200, fixture!("users/user.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "users",
            "create",
            "--name",
            "Jane Doe",
            "--email",
            "jane@example.com",
            "--role",
            "end-user",
            "--verified",
            "--organization-id",
            "1001",
            "--custom-field",
            "tier=gold",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["id"], 42);
    h.zdk_api(&server.uri())
        .args([
            "users",
            "create-or-update",
            "--name",
            "Jane Doe",
            "--email",
            "jane@example.com",
        ])
        .assert()
        .success();
    h.zdk_api(&server.uri())
        .args([
            "users",
            "update",
            "42",
            "--name",
            "Jane Smith",
            "--suspended",
        ])
        .assert()
        .success();
    h.zdk_api(UNREACHABLE)
        .args(["users", "update", "42", "--suspended", "--no-suspended"])
        .assert()
        .code(2);
    h.zdk_api(UNREACHABLE)
        .args(["users", "create", "--role", "owner", "--name", "x"])
        .assert()
        .code(2);
    h.zdk_api(&server.uri())
        .args(["users", "delete", "42"])
        .assert()
        .code(2);
    let out = h
        .zdk_api(&server.uri())
        .args(["users", "delete", "42", "--yes"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["id"], 42);
    server.verify().await;
}

// ---------------------------------------------------------------------------------------------
// orgs
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn orgs_reads_and_writes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations"))
        .respond_with(json_response(200, fixture!("organizations/list.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations/1001"))
        .respond_with(json_response(
            200,
            fixture!("organizations/organization.json"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations/search"))
        .and(query_param("external_id", "ACME-1"))
        .respond_with(json_response(200, fixture!("organizations/list.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations/autocomplete"))
        .and(query_param("name", "ac"))
        .respond_with(json_response(200, fixture!("organizations/list.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations/count"))
        .respond_with(json_response(
            200,
            r#"{"count": {"value": 2, "refreshed_at": null}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations/1001/related"))
        .respond_with(json_response(
            200,
            r#"{"organization_related": {"users_count": 2, "tickets_count": 5}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations/1001/tickets"))
        .respond_with(json_response(200, fixture!("tickets/list_page2.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/organizations/1001/users"))
        .respond_with(json_response(200, fixture!("users/list.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/organizations"))
        .and(body_json(serde_json::json!({
            "organization": {
                "name": "X",
                "domain_names": ["a.com", "b.com"],
                "tags": ["enterprise"],
                "shared_tickets": true,
                "shared_comments": true,
                "organization_fields": {"region": "EMEA"}
            }
        })))
        .respond_with(json_response(
            201,
            fixture!("organizations/organization.json"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/organizations/1001"))
        .and(body_json(serde_json::json!({
            "organization": {"name": "Acme Corporation", "domain_names": ["acme.com"]}
        })))
        .respond_with(json_response(
            200,
            fixture!("organizations/organization.json"),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v2/organizations/1001/tags"))
        .and(body_json(serde_json::json!({"tags": ["renewal-q4"]})))
        .respond_with(json_response(
            200,
            r#"{"tags": ["enterprise", "emea", "renewal-q4"]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/organizations/1001"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["orgs", "list", "-o", "table"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let table = String::from_utf8_lossy(&out);
    for header in ["ID", "NAME", "DOMAINS", "TAGS", "UPDATED"] {
        assert!(table.contains(header), "{table}");
    }
    assert!(table.contains("acme.com, acme.co.uk"), "{table}");
    let out = h
        .zdk_api(&server.uri())
        .args([
            "orgs",
            "get",
            "1001",
            "--fields",
            "name,organization_fields.region",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!({"name": "Acme Corp", "organization_fields": {"region": "EMEA"}})
    );
    h.zdk_api(&server.uri())
        .args(["orgs", "search", "--external-id", "ACME-1"])
        .assert()
        .success();
    h.zdk_api(UNREACHABLE)
        .args(["orgs", "search"])
        .assert()
        .code(2);
    h.zdk_api(&server.uri())
        .args(["orgs", "autocomplete", "ac"])
        .assert()
        .success();
    h.zdk_api(&server.uri())
        .args(["orgs", "count", "-o", "table"])
        .assert()
        .success()
        .stdout("2\n");
    let out = h
        .zdk_api(&server.uri())
        .args(["orgs", "related", "1001"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["tickets_count"], 5);
    let out = h
        .zdk_api(&server.uri())
        .args(["orgs", "tickets", "1001"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)[0]["id"], 3);
    let out = h
        .zdk_api(&server.uri())
        .args(["orgs", "users", "1001"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out).as_array().expect("array").len(), 3);

    let out = h
        .zdk_api(&server.uri())
        .args([
            "orgs",
            "create",
            "--name",
            "X",
            "--domain",
            "a.com",
            "--domain",
            "b.com",
            "--tag",
            "enterprise",
            "--shared-comments",
            "--custom-field",
            "region=EMEA",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["id"], 1001);
    h.zdk_api(UNREACHABLE)
        .args(["orgs", "create", "--domain", "a.com"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--name"));
    let out = h
        .zdk_api(&server.uri())
        .args([
            "orgs",
            "update",
            "1001",
            "--name",
            "Acme Corporation",
            "--domain",
            "acme.com",
            "--add-tag",
            "renewal-q4",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out)["tags"],
        serde_json::json!(["enterprise", "emea", "renewal-q4"])
    );
    h.zdk_api(&server.uri())
        .args(["orgs", "delete", "1001"])
        .assert()
        .code(2);
    h.zdk_api(&server.uri())
        .args(["orgs", "delete", "1001", "--yes", "-o", "table"])
        .assert()
        .success()
        .stdout("Deleted organization #1001\n");
    server.verify().await;
}

// ---------------------------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn search_results_count_export_and_explain() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search"))
        .and(query_param("query", "type:ticket status:open"))
        .and(query_param("sort_by", "updated_at"))
        .and(query_param("sort_order", "desc"))
        .respond_with(json_response(200, fixture!("search/mixed_results.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search/count"))
        .and(query_param("query", "type:ticket"))
        .respond_with(json_response(200, fixture!("search/count.json")))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search/export"))
        .and(query_param("page[after]", "export-cursor-2"))
        .respond_with(json_response(200, fixture!("search/export_page2.json")))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/search/export"))
        .and(query_param("query", "status:open"))
        .and(query_param("filter[type]", "ticket"))
        .respond_with(json_response(200, fixture!("search/export_page1.json")))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args([
            "search",
            "type:ticket status:open",
            "--sort-by",
            "updated_at",
            "--sort-order",
            "desc",
            "-o",
            "table",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let table = String::from_utf8_lossy(&out);
    for header in ["ID", "TYPE", "TITLE", "STATUS", "UPDATED"] {
        assert!(table.contains(header), "{table}");
    }
    assert!(
        table.contains("organization") && table.contains("Acme Corp"),
        "{table}"
    );
    h.zdk_api(UNREACHABLE)
        .args(["search", "x", "--sort-by", "subject"])
        .assert()
        .code(2);

    h.zdk_api(&server.uri())
        .args(["search", "count", "type:ticket", "-o", "table"])
        .assert()
        .success()
        .stdout("57\n");
    let out = h
        .zdk_api(&server.uri())
        .args(["search", "count", "type:ticket"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out), serde_json::json!({"count": 57}));

    // export: --filter-type is mandatory when the query does not say type:.
    let out = h
        .zdk_api(UNREACHABLE)
        .args(["search", "export", "status:open"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--filter-type"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let target = h.home.join("out.ndjson");
    h.zdk_api(&server.uri())
        .args([
            "search",
            "export",
            "status:open",
            "--filter-type",
            "ticket",
            "--to",
            target.to_str().expect("utf8"),
            "-o",
            "table",
        ])
        .assert()
        .success()
        .stdout(predicate::str::starts_with(
            "Exported 3 ticket record(s) to ",
        ));
    let written = std::fs::read_to_string(&target).expect("ndjson");
    let rows: Vec<serde_json::Value> = written
        .lines()
        .map(|l| serde_json::from_str(l).expect("line is JSON"))
        .collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2]["id"], 3);

    // explain is offline.
    let out = h
        .zdk_api(UNREACHABLE)
        .args([
            "search",
            "explain",
            "--status",
            "open,pending",
            "--assignee",
            "42",
            "--unassigned",
            "--tag",
            "vip customer",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        json(&out),
        serde_json::json!({
            "query": "type:ticket status:open status:pending assignee:none tags:\"vip customer\"",
            "endpoint": "/api/v2/search"
        })
    );
    h.zdk_api(UNREACHABLE)
        .args([
            "search",
            "explain",
            "--requester",
            "me",
            "--all",
            "-o",
            "table",
        ])
        .assert()
        .success()
        .stdout("type:ticket requester:me\n");
    h.zdk_api(UNREACHABLE)
        .args([
            "search",
            "explain",
            "priority:urgent",
            "--status",
            "open",
            "-o",
            "table",
        ])
        .assert()
        .success()
        .stdout("type:ticket status:open priority:urgent\n");
    h.zdk_api(UNREACHABLE)
        .args(["search", "explain"])
        .assert()
        .code(2);
    server.verify().await;
}

// ---------------------------------------------------------------------------------------------
// help snapshots
// ---------------------------------------------------------------------------------------------

#[test]
fn resource_help_snapshots() {
    let h = Harness::new();
    for (name, cmd) in [
        ("tickets_help", "tickets"),
        ("comments_help", "comments"),
        ("users_help", "users"),
        ("orgs_help", "orgs"),
        ("search_help", "search"),
    ] {
        let out = h.zdk().args([cmd, "--help"]).output().expect("run");
        assert!(out.status.success(), "{cmd} --help");
        insta::assert_snapshot!(name, String::from_utf8_lossy(&out.stdout));
    }
    let out = h
        .zdk()
        .args(["tickets", "list", "--help"])
        .output()
        .expect("run");
    assert!(out.status.success());
    insta::assert_snapshot!("tickets_list_help", String::from_utf8_lossy(&out.stdout));
}
