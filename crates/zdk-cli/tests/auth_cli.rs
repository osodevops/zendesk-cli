//! Black-box tests for `zdk auth …` and `zdk doctor` against a wiremock Zendesk (P3b).
//!
//! Logins persist through the encrypted file store (`--credential-store file`) with the
//! machine key file, so consecutive invocations under one harness share the credential.

mod common;

use assert_cmd::Command;
use common::{FAST_CONFIG, Harness, json, stderr_error};
use predicates::prelude::*;
use wiremock::matchers::{body_string_contains, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `zdk` command using the file store against the mock, with no token in the environment.
fn zdk_file_store(h: &Harness, base_url: &str) -> Command {
    if !h.config_path().exists() {
        h.write_config(FAST_CONFIG);
    }
    let mut cmd = h.zdk();
    cmd.env("ZENDESK_BASE_URL", base_url)
        .args(["--credential-store", "file"]);
    cmd
}

fn user_body() -> serde_json::Value {
    serde_json::json!({
        "user": {"id": 42, "name": "Ada Lovelace", "email": "ada@acme.com", "role": "admin"}
    })
}

async fn mock_me(server: &MockServer, bearer: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .and(header("authorization", bearer))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(user_body())
                .insert_header("content-type", "application/json"),
        )
        .mount(server)
        .await;
}

// ---------------------------------------------------------------------------------------------
// login (client credentials) → status → token → scopes check → refresh → logout
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn client_credentials_login_persists_and_drives_status_token_check_refresh_logout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("client_id=c"))
        .and(body_string_contains("scope=tickets%3Aread"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "cc-token",
            "token_type": "bearer",
            "scope": "tickets:read",
            "expires_in": 3600
        })))
        .expect(2)
        .mount(&server)
        .await;
    mock_me(&server, "Bearer cc-token").await;

    let h = Harness::new();
    let out = zdk_file_store(&h, &server.uri())
        .args([
            "--subdomain",
            "acme",
            "auth",
            "login",
            "--client-credentials",
            "--client-id",
            "c",
            "--client-secret",
            "s3cret-value",
            "--scopes",
            "tickets:read",
            "--store-secret",
            "-o",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let summary = json(&out.stdout);
    assert_eq!(summary["grant"], "client_credentials");
    assert_eq!(summary["profile"], "default");
    assert_eq!(summary["scopes"], serde_json::json!(["tickets:read"]));
    assert_eq!(summary["user"]["name"], "Ada Lovelace");
    assert_eq!(summary["user"]["role"], "admin");
    assert_eq!(summary["store"], "file");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("s3cret-value"),
        "secret on stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("s3cret-value"),
        "secret on stderr:\n{stderr}"
    );
    assert!(!stdout.contains("cc-token"), "token on stdout:\n{stdout}");
    assert!(stderr.contains("Logged in"), "{stderr}");
    // First login for a profile missing from the config file: flags are persisted.
    assert!(stderr.contains("Saved profile 'default'"), "{stderr}");
    let config = h.read_config();
    assert!(config.contains("[profiles.default]"), "{config}");
    assert!(config.contains("subdomain = \"acme\""), "{config}");
    assert!(config.contains("client_id = \"c\""), "{config}");
    assert!(
        config.contains("grant_type = \"client_credentials\""),
        "{config}"
    );

    // status: the stored credential, no network.
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "status", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = json(&out);
    assert_eq!(status["grant"], "client_credentials");
    assert_eq!(status["profile"], "default");
    assert_eq!(status["store"], "file");
    assert_eq!(status["subdomain"], "acme");
    assert_eq!(status["client_id"], "c");
    assert_eq!(status["scopes"], serde_json::json!(["tickets:read"]));
    assert_eq!(status["has_refresh_token"], false);
    assert_eq!(status["can_renew"], true, "secret was stored");
    assert!(status["expires_in_secs"].as_i64().is_some_and(|s| s > 3000));
    assert!(
        status["expires_in"]
            .as_str()
            .is_some_and(|s| s.contains('m'))
    );
    assert!(
        !String::from_utf8_lossy(&out).contains("cc-token"),
        "status must not print the token"
    );
    // Table mode is a FIELD / VALUE table.
    zdk_file_store(&h, &server.uri())
        .args(["auth", "status", "-o", "table"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("FIELD")
                .and(predicate::str::contains("Grant"))
                .and(predicate::str::contains("client_credentials"))
                .and(predicate::str::contains("cc-token").not()),
        );

    // token: the one place the secret is printed.
    zdk_file_store(&h, &server.uri())
        .args(["auth", "token"])
        .assert()
        .success()
        .stdout("cc-token\n");
    zdk_file_store(&h, &server.uri())
        .args(["auth", "token", "--format", "bearer"])
        .assert()
        .success()
        .stdout("Bearer cc-token\n");
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "token", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["access_token"], "cc-token");
    assert_eq!(json(&out)["token_type"], "bearer");

    // scopes check: tickets:read does not cover tickets:write → exit 4; list is fine.
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "scopes", "check", "tickets", "update", "-o", "json"])
        .assert()
        .code(4)
        .get_output()
        .clone();
    let check = json(&out.stdout);
    assert_eq!(check["satisfied"], false);
    assert_eq!(check["missing"], serde_json::json!(["tickets:write"]));
    assert_eq!(stderr_error(&out.stderr)["error"]["code"], "SCOPE_MISSING");
    zdk_file_store(&h, &server.uri())
        .args([
            "auth", "scopes", "check", "tickets", "update", "-o", "table",
        ])
        .assert()
        .code(4)
        .stdout(predicate::str::contains(
            "requires tickets:write — you have tickets:read",
        ));
    zdk_file_store(&h, &server.uri())
        .args(["auth", "scopes", "check", "tickets", "list", "-o", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "requires tickets:read — you have tickets:read",
        ));
    let out = zdk_file_store(&h, &server.uri())
        .args([
            "auth", "scopes", "show", "--preset", "exporter", "-o", "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let show = json(&out);
    assert_eq!(show["preset"], "exporter");
    assert_eq!(show["satisfied"], false);
    assert!(
        show["missing"]
            .as_array()
            .is_some_and(|m| m.contains(&serde_json::json!("users:read"))),
        "{show}"
    );

    // refresh --force re-mints with the stored secret (second POST /oauth/tokens).
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "refresh", "--force", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let refreshed = json(&out);
    assert_eq!(refreshed["refreshed"], true);
    assert_eq!(refreshed["grant"], "client_credentials");
    // Not due and not forced: no third mint (the mock expects exactly two).
    zdk_file_store(&h, &server.uri())
        .args(["auth", "refresh", "-o", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not due"));

    // auth test: users/me ok, Help Center skipped for a tickets:read-only token.
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "test", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows = json(&out);
    let rows = rows.as_array().expect("array");
    assert_eq!(rows[0]["api"], "support");
    assert_eq!(rows[0]["status"], "ok");
    assert_eq!(rows[1]["api"], "help_center");
    assert_eq!(rows[1]["status"], "skip");

    // logout (no revoke: the mock has no /oauth/tokens/current) then status is exit 3.
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "logout", "--no-revoke", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["removed"], serde_json::json!(["default"]));
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "status", "-o", "json"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert_eq!(
        stderr_error(&out.stderr)["error"]["code"],
        "AUTH_NOT_LOGGED_IN"
    );
    // Logging out again is idempotent.
    zdk_file_store(&h, &server.uri())
        .args(["auth", "logout", "--no-revoke", "-o", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to log out"));
}

// ---------------------------------------------------------------------------------------------
// login --no-browser (authorization code + PKCE with the code on stdin)
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn no_browser_login_reads_the_code_from_stdin_and_exchanges_with_pkce() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("client_id=zdk_local"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A8484%2Fcallback",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "ac-token",
            "token_type": "bearer",
            "refresh_token": "rt-1",
            "scope": "tickets:read tickets:write users:read organizations:read hc:read",
            "expires_in": 1800,
            "refresh_token_expires_in": 86400
        })))
        .expect(1)
        .mount(&server)
        .await;
    mock_me(&server, "Bearer ac-token").await;

    let h = Harness::new();
    let out = zdk_file_store(&h, &server.uri())
        .args([
            "--subdomain",
            "acme",
            "auth",
            "login",
            "--no-browser",
            "--client-id",
            "zdk_local",
            "--preset",
            "agent",
            "-o",
            "json",
        ])
        .write_stdin("the-code\n")
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("/oauth/authorizations/new?"),
        "authorize URL on stderr:\n{stderr}"
    );
    assert!(stderr.contains("code_challenge_method=S256"), "{stderr}");
    assert!(stderr.contains("code_challenge="), "{stderr}");
    assert!(stderr.contains("scope=tickets%3Aread"), "{stderr}");
    assert!(stderr.contains("state="), "{stderr}");
    assert!(stderr.contains("paste the authorization code"), "{stderr}");
    let summary = json(&out.stdout);
    assert_eq!(summary["grant"], "authorization_code");
    assert_eq!(summary["user"]["id"], 42);
    assert!(
        summary["scopes"]
            .as_array()
            .is_some_and(|s| s.len() == 5 && s.contains(&serde_json::json!("hc:read")))
    );

    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "status", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = json(&out);
    assert_eq!(status["grant"], "authorization_code");
    assert_eq!(status["has_refresh_token"], true);
    assert_eq!(status["can_renew"], true);
    assert!(status["refresh_expires_at"].is_string());

    // whoami through the stored OAuth token (no env token).
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "whoami", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["email"], "ada@acme.com");
}

#[test]
fn login_usage_errors_happen_before_any_network() {
    let h = Harness::new();
    // No client id anywhere → usage error naming the fix.
    h.zdk()
        .args(["auth", "login", "--no-browser", "-o", "json"])
        .write_stdin("code\n")
        .assert()
        .code(10)
        .stderr(predicate::str::contains("client id").or(predicate::str::contains("client-id")));
    // Unknown preset / scope.
    h.zdk()
        .args(["auth", "login", "--client-id", "c", "--preset", "root"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown preset"));
    h.zdk()
        .args([
            "auth",
            "login",
            "--client-id",
            "c",
            "--scopes",
            "ticket:read",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown scope"));
    // Client credentials without a secret.
    let out = h
        .zdk()
        .args([
            "auth",
            "login",
            "--client-credentials",
            "--client-id",
            "c",
            "--scopes",
            "tickets:read",
            "-o",
            "json",
        ])
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert_eq!(
        stderr_error(&out.stderr)["error"]["code"],
        "AUTH_CLIENT_SECRET_REQUIRED"
    );
    // Mutually exclusive grants.
    h.zdk()
        .args(["auth", "login", "--client-credentials", "--api-token"])
        .assert()
        .code(2);
    // --api-token needs an email and a token.
    h.zdk()
        .args(["auth", "login", "--api-token", "--token", "t"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--email"));
}

// ---------------------------------------------------------------------------------------------
// login --api-token: deprecation warning on stderr, suppressed by -q
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn api_token_login_warns_about_the_deadline_unless_quiet() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(user_body()))
        .expect(2)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = zdk_file_store(&h, &server.uri())
        .args([
            "--subdomain",
            "acme",
            "auth",
            "login",
            "--api-token",
            "--email",
            "ada@acme.com",
            "--token",
            "api-secret",
            "-o",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("deprecated"), "{stderr}");
    assert!(stderr.contains("30 Apr 2027"), "{stderr}");
    assert!(stderr.contains("zdk auth login"), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("api-secret"), "{stdout}");
    assert!(!stderr.contains("api-secret"), "{stderr}");
    let summary = json(&out.stdout);
    assert_eq!(summary["grant"], "api_token");
    assert_eq!(summary["user"]["name"], "Ada Lovelace");
    assert!(h.read_config().contains("email = \"ada@acme.com\""));

    let out = zdk_file_store(&h, &server.uri())
        .args([
            "-q",
            "--subdomain",
            "acme",
            "auth",
            "login",
            "--api-token",
            "--email",
            "ada@acme.com",
            "--token",
            "api-secret",
            "-o",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("deprecated"),
        "-q must silence the warning:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // status shows the API-token countdown fields; refresh refuses.
    let out = zdk_file_store(&h, &server.uri())
        .args(["auth", "status", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = json(&out);
    assert_eq!(status["grant"], "api_token");
    assert_eq!(status["email"], "ada@acme.com");
    assert!(status["api_token_days_remaining"].is_number());
    assert!(status["deprecation_warning"].is_string());
    zdk_file_store(&h, &server.uri())
        .args(["auth", "status", "-o", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains("API token deadline"));
    zdk_file_store(&h, &server.uri())
        .args(["auth", "refresh", "--force"])
        .assert()
        .code(2);
    // token --format bearer prints the Basic header; raw prints the API token itself.
    zdk_file_store(&h, &server.uri())
        .args(["-q", "auth", "token", "--format", "bearer"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("Basic "));
    zdk_file_store(&h, &server.uri())
        .args(["-q", "auth", "token"])
        .assert()
        .success()
        .stdout("api-secret\n");
}

// ---------------------------------------------------------------------------------------------
// whoami / test with a static token, and a 401
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn whoami_and_test_with_a_static_token() {
    let server = MockServer::start().await;
    mock_me(&server, "Bearer test-token").await;
    Mock::given(method("GET"))
        .and(path("/api/v2/help_center/locales"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "locales": ["en-us", "fr"], "default_locale": "en-us"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["auth", "whoami"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let user = json(&out);
    assert_eq!(user["id"], 42);
    assert_eq!(user["name"], "Ada Lovelace");
    assert_eq!(user["role"], "admin");
    // Table mode: the `users` preset (a one-row table), not a FIELD/VALUE dump.
    h.zdk_api(&server.uri())
        .args(["auth", "whoami", "-o", "table"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("NAME")
                .and(predicate::str::contains("ROLE"))
                .and(predicate::str::contains("Ada Lovelace")),
        );

    // Static token: scopes unknown → Help Center is probed too.
    h.zdk_api(&server.uri())
        .args(["auth", "test"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("support")
                .and(predicate::str::contains("help_center"))
                .and(predicate::str::contains("2 locale(s)")),
        );

    // status for a static token needs no store and no network.
    let out = h
        .zdk_api(&server.uri())
        .args(["auth", "status", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["grant"], "static_token");
    assert_eq!(json(&out)["store"], "env");
}

#[tokio::test(flavor = "multi_thread")]
async fn whoami_on_401_exits_3_with_an_auth_error_code() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Couldn't authenticate you"
        })))
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["auth", "whoami", "-o", "json"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    let err = stderr_error(&out.stderr);
    assert!(
        err["error"]["code"]
            .as_str()
            .is_some_and(|c| c.starts_with("AUTH_")),
        "{err}"
    );
    assert_eq!(err["error"]["exit_code"], 3);

    // auth test reports the failure row and still exits 3.
    let out = h
        .zdk_api(&server.uri())
        .args(["auth", "test", "-o", "json"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let rows = json(&out.stdout);
    assert_eq!(rows[0]["status"], "fail");
    assert_eq!(rows[1]["status"], "skip");
}

// ---------------------------------------------------------------------------------------------
// scopes: catalogue, presets, check without a known scope set
// ---------------------------------------------------------------------------------------------

#[test]
fn scopes_list_and_presets() {
    let h = Harness::new();
    let out = h
        .zdk()
        .args(["auth", "scopes", "list", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let list = json(&out);
    let list = list.as_array().expect("array");
    assert_eq!(list.len(), 52);
    assert_eq!(list[0]["scope"], "read");
    assert!(list.iter().any(|s| s["scope"] == "tickets:write"
        && s["family"] == "tickets"
        && s["access"] == "write"));
    h.zdk()
        .args(["auth", "scopes", "list", "-o", "table"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("SCOPE")
                .and(predicate::str::contains("FAMILY"))
                .and(predicate::str::contains("ACCESS"))
                .and(predicate::str::contains("DESCRIPTION"))
                .and(predicate::str::contains("ticket_views:write")),
        );

    let out = h
        .zdk()
        .args(["auth", "scopes", "preset", "agent", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let preset = json(&out);
    assert_eq!(preset["preset"], "agent");
    assert!(
        preset["scopes"]
            .as_array()
            .is_some_and(|s| s.contains(&serde_json::json!("tickets:read"))),
        "{preset}"
    );
    h.zdk()
        .args(["auth", "scopes", "preset", "admin", "-o", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains("triggers:write").and(predicate::str::contains("SCOPE")));
    h.zdk()
        .args(["auth", "scopes", "preset", "root"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown preset"));
}

#[test]
fn scopes_check_without_known_scopes_skips_and_unknown_commands_are_usage_errors() {
    let h = Harness::new();
    // A static token has no scope list: the server decides, exit 0.
    let out = h
        .zdk()
        .env("ZENDESK_ACCESS_TOKEN", "test-token")
        .args(["auth", "scopes", "check", "tickets", "update", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let check = json(&out);
    assert_eq!(check["required"], serde_json::json!(["tickets:write"]));
    assert!(check["satisfied"].is_null());
    assert!(check["granted"].is_null());
    h.zdk()
        .env("ZENDESK_ACCESS_TOKEN", "test-token")
        .args(["auth", "scopes", "check", "organizations", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("organizations:read"));
    // Local commands need nothing (no credential needed to say so).
    h.zdk()
        .args(["auth", "scopes", "check", "config", "init", "-o", "table"])
        .assert()
        .success()
        .stdout(predicate::str::contains("requires no OAuth scope"));
    // Unknown command path → exit 2.
    h.zdk()
        .args(["auth", "scopes", "check", "frobnicate"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown command"));
    // Not logged in: granted scopes are unknown, a warning says so, exit 0.
    h.zdk()
        .args(["auth", "scopes", "check", "tickets", "update"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not logged in"));
}

// ---------------------------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn doctor_json_reports_every_check_and_the_plan_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/users/me"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(user_body())
                .insert_header("x-rate-limit", "700")
                .insert_header("x-rate-limit-remaining", "699")
                .insert_header("date", chrono::Utc::now().to_rfc2822().as_str()),
        )
        .expect(2)
        .mount(&server)
        .await;

    let h = Harness::new();
    let out = h
        .zdk_api(&server.uri())
        .args(["doctor", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let report = json(&out.stdout);
    assert_eq!(report["ok"], true);
    let checks = report["checks"].as_array().expect("checks array");
    let find = |name: &str| {
        checks
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no check named {name}: {report}"))
    };
    for name in [
        "config",
        "profile",
        "credential_store",
        "credentials",
        "auth",
        "clock_skew",
        "rate_limit",
        "registry",
    ] {
        assert_ne!(find(name)["status"], "fail", "{name}: {report}");
    }
    let rate = find("rate_limit");
    assert_eq!(rate["status"], "ok");
    assert!(
        rate["detail"].as_str().is_some_and(|d| d.contains("700")),
        "{rate}"
    );
    assert_eq!(rate["plan"], "Enterprise");
    assert_eq!(rate["limit"], 700);
    let auth = find("auth");
    assert_eq!(auth["status"], "ok");
    assert_eq!(auth["user"], "Ada Lovelace");
    assert_eq!(auth["role"], "admin");
    let store = find("credential_store");
    assert_eq!(store["backend"], "env");
    let clock = find("clock_skew");
    assert_eq!(clock["status"], "ok", "{clock}");
    let registry = find("registry");
    assert!(registry["operations"].as_u64().is_some_and(|n| n > 500));

    // Table mode: CHECK STATUS DETAIL.
    h.zdk_api(&server.uri())
        .args(["doctor", "-o", "table"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("CHECK")
                .and(predicate::str::contains("STATUS"))
                .and(predicate::str::contains("DETAIL"))
                .and(predicate::str::contains("registry")),
        );
}

#[test]
fn doctor_fails_when_not_logged_in_and_keeps_the_error_code() {
    let h = Harness::new();
    h.write_config(FAST_CONFIG);
    let out = h
        .zdk()
        .args(["doctor", "-o", "json"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let report = json(&out.stdout);
    assert_eq!(report["ok"], false);
    let credentials = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["name"] == "credentials")
        .cloned()
        .expect("credentials check");
    assert_eq!(credentials["status"], "fail");
    assert_eq!(credentials["code"], "AUTH_NOT_LOGGED_IN");
    let store = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|c| c["name"] == "credential_store")
        .cloned()
        .expect("store check");
    assert_eq!(
        store["status"], "warn",
        "memory store is a warning: {store}"
    );
    let err = stderr_error(&out.stderr);
    assert!(
        err["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("credentials") && m.contains("auth")),
        "{err}"
    );
}

// ---------------------------------------------------------------------------------------------
// help snapshots
// ---------------------------------------------------------------------------------------------

#[test]
fn help_snapshots() {
    let h = Harness::new();
    let auth = h.zdk().args(["auth", "--help"]).output().expect("run");
    assert!(auth.status.success());
    insta::assert_snapshot!("auth_help", String::from_utf8_lossy(&auth.stdout));

    let login = h
        .zdk()
        .args(["auth", "login", "--help"])
        .output()
        .expect("run");
    assert!(login.status.success());
    insta::assert_snapshot!("auth_login_help", String::from_utf8_lossy(&login.stdout));

    let scopes = h
        .zdk()
        .args(["auth", "scopes", "--help"])
        .output()
        .expect("run");
    assert!(scopes.status.success());
    insta::assert_snapshot!("auth_scopes_help", String::from_utf8_lossy(&scopes.stdout));

    let doctor = h.zdk().args(["doctor", "--help"]).output().expect("run");
    assert!(doctor.status.success());
    insta::assert_snapshot!("doctor_help", String::from_utf8_lossy(&doctor.stdout));
}
