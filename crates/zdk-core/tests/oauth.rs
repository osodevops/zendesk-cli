//! OAuth engine against a wiremock `POST /oauth/tokens` (and the revocation endpoints).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};

use chrono::{TimeDelta, Utc};
use secrecy::{ExposeSecret, SecretString};
use url::Url;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zdk_core::auth::authorization_code::challenge_for;
use zdk_core::auth::token::{Credential, TokenSet};
use zdk_core::auth::{self, GrantKind, LoginOptions, client_credentials, oauth, revoke};
use zdk_core::config::{ConfigFile, EnvOverrides, GlobalArgs, Paths, Settings};
use zdk_core::error::AuthFailure;
use zdk_core::store::{CredentialStore, MemoryStore, SharedStore};
use zdk_core::{Result, ZdkError};

fn init_tls() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn http() -> reqwest::Client {
    init_tls();
    auth::http_client(std::time::Duration::from_secs(5)).expect("client")
}

fn base(server: &MockServer) -> Url {
    Url::parse(&server.uri()).expect("mock url")
}

fn paths() -> Paths {
    Paths {
        config_file: "/t/config.toml".into(),
        config_dir: "/t".into(),
        state_dir: "/t/state".into(),
        cache_dir: "/t/cache".into(),
    }
}

/// Settings whose base URL is the mock server (profile `default`, subdomain `test`).
fn settings(server: &MockServer, extra: &[(&str, &str)]) -> (Settings, EnvOverrides) {
    init_tls();
    let mut vars: Vec<(String, String)> = vec![
        ("ZENDESK_SUBDOMAIN".into(), "test".into()),
        ("ZENDESK_BASE_URL".into(), server.uri()),
    ];
    for (k, v) in extra {
        vars.push(((*k).to_string(), (*v).to_string()));
    }
    let env = EnvOverrides::from_pairs(vars);
    let s = Settings::resolve(
        &GlobalArgs::default(),
        &env,
        &ConfigFile::default(),
        &paths(),
    )
    .expect("settings");
    (s, env)
}

fn token_json(access: &str, refresh: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "access_token": access,
        "token_type": "bearer",
        "scope": "tickets:read users:read",
        "expires_in": 1800,
        "refresh_token_expires_in": 2_592_000
    });
    if let Some(r) = refresh {
        v["refresh_token"] = serde_json::Value::String(r.to_string());
    }
    v
}

/// An authorization-code token obtained `age_secs` ago with a 1800 s lifetime.
fn stored_token(age_secs: i64, refresh: Option<&str>) -> TokenSet {
    let obtained = Utc::now() - TimeDelta::seconds(age_secs);
    TokenSet {
        access_token: SecretString::from("A1".to_string()),
        token_type: "bearer".into(),
        scopes: vec!["tickets:read".into()],
        obtained_at: obtained,
        expires_at: Some(obtained + TimeDelta::seconds(1800)),
        refresh_token: refresh.map(|r| SecretString::from(r.to_string())),
        refresh_expires_at: Some(obtained + TimeDelta::days(30)),
        grant: GrantKind::AuthorizationCode,
        subdomain: "test".into(),
        client_id: "zdk".into(),
        client_secret: None,
    }
}

fn form_of(req: &wiremock::Request) -> HashMap<String, String> {
    url::form_urlencoded::parse(&req.body)
        .into_owned()
        .collect()
}

async fn token_posts(server: &MockServer) -> Vec<HashMap<String, String>> {
    server
        .received_requests()
        .await
        .expect("requests recorded")
        .iter()
        .filter(|r| r.method == "POST" && r.url.path() == "/oauth/tokens")
        .map(form_of)
        .collect()
}

// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn authorization_code_exchange_sends_pkce_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("client_id=zdk"))
        .and(body_string_contains("code=abc"))
        .and(body_string_contains("code_verifier=ver"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Fcallback",
        ))
        .and(body_string_contains("scope=tickets%3Aread+users%3Aread"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("A2", Some("R2"))))
        .expect(1)
        .mount(&server)
        .await;

    let req = oauth::CodeExchange {
        client_id: "zdk".into(),
        client_secret: None,
        code: "abc".into(),
        redirect_uri: "http://127.0.0.1:8080/callback".into(),
        code_verifier: "ver".into(),
        scope: Some("tickets:read users:read".into()),
        expires_in: Some(1800),
    };
    let now = Utc::now();
    let resp = oauth::exchange_code_at(&http(), &base(&server), &req)
        .await
        .unwrap();
    let t = resp.into_token_set(GrantKind::AuthorizationCode, "test", "zdk", None, now);
    assert_eq!(t.access_token.expose_secret(), "A2");
    assert_eq!(t.refresh_token.as_ref().unwrap().expose_secret(), "R2");
    assert_eq!(t.scopes, vec!["tickets:read", "users:read"]);
    assert_eq!(t.expires_at, Some(now + TimeDelta::seconds(1800)));
    assert_eq!(t.refresh_expires_at, Some(now + TimeDelta::days(30)));
    assert_eq!(t.grant, GrantKind::AuthorizationCode);

    let posts = token_posts(&server).await;
    assert_eq!(posts.len(), 1);
    assert!(
        !posts[0].contains_key("client_secret"),
        "public client must not send a secret"
    );
    assert_eq!(posts[0]["expires_in"], "1800");
}

#[tokio::test]
async fn invalid_grant_on_exchange_is_code_expired() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": "The provided authorization grant is invalid, expired, revoked"
        })))
        .mount(&server)
        .await;
    let req = oauth::CodeExchange {
        client_id: "zdk".into(),
        client_secret: Some(SecretString::from("s".to_string())),
        code: "old".into(),
        redirect_uri: "http://127.0.0.1:1/callback".into(),
        code_verifier: "v".into(),
        scope: None,
        expires_in: None,
    };
    let err = oauth::exchange_code_at(&http(), &base(&server), &req)
        .await
        .unwrap_err();
    assert!(matches!(err, ZdkError::Auth(AuthFailure::CodeExpired)));
    assert_eq!(err.exit_code(), 3);
    assert!(err.help_text().unwrap().contains("120 seconds"));
    let posts = token_posts(&server).await;
    assert_eq!(
        posts[0]["client_secret"], "s",
        "confidential client sends it"
    );
}

#[tokio::test]
async fn client_credentials_never_keeps_a_refresh_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("client_secret=sekrit"))
        .and(body_string_contains("scope=tickets%3Aread"))
        // Zendesk does not issue one for this grant; even if a server did, we must drop it.
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "CC1", "token_type": "bearer", "expires_in": 3600, "refresh_token": "nope"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let t = client_credentials::mint(
        &http(),
        &base(&server),
        "test",
        "zdk_ci",
        &SecretString::from("sekrit".to_string()),
        &["tickets:read".to_string()],
        Some(3600),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(t.access_token.expose_secret(), "CC1");
    assert!(t.refresh_token.is_none());
    assert!(t.refresh_expires_at.is_none());
    assert_eq!(t.grant, GrantKind::ClientCredentials);
    assert_eq!(t.scopes, vec!["tickets:read"], "falls back to the request");
    assert!(t.client_secret.is_none(), "secret not stored unless asked");
}

#[tokio::test]
async fn invalid_scope_maps_to_invalid_scope_with_admin_center_help() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_scope",
            "error_description": "The requested scope is invalid, unknown, or malformed."
        })))
        .mount(&server)
        .await;
    let err = oauth::client_credentials_at(
        &http(),
        &base(&server),
        "zdk",
        &SecretString::from("s".to_string()),
        &["triggers:write".to_string()],
        None,
    )
    .await
    .unwrap_err();
    match &err {
        ZdkError::Auth(AuthFailure::InvalidScope { scope, detail }) => {
            assert_eq!(scope, "triggers:write");
            assert!(detail.contains("malformed"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(err.error_code(), "AUTH_INVALID_SCOPE");
    assert!(err.help_text().unwrap().contains("Admin Center"));
}

#[tokio::test]
async fn refresh_invalid_grant_is_revoked_for_the_profile() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=dead"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "revoked"
        })))
        .mount(&server)
        .await;
    let err = oauth::refresh_at(
        &http(),
        &base(&server),
        "zdk",
        None,
        &SecretString::from("dead".to_string()),
        "work",
    )
    .await
    .unwrap_err();
    match err {
        ZdkError::Auth(AuthFailure::Revoked { profile, detail }) => {
            assert_eq!(profile, "work");
            assert_eq!(detail, "revoked");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn provider_refreshes_at_80_percent_and_rotates_without_reusing_the_old_token() {
    let server = MockServer::start().await;
    // First rotation: R1 → (A2, R2). Second (forced by a 401): R2 → (A3, R3).
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=R1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("A2", Some("R2"))))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("refresh_token=R2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("A3", Some("R3"))))
        .expect(1)
        .mount(&server)
        .await;

    let store: SharedStore = Arc::new(MemoryStore::new());
    // 25 of 30 minutes elapsed → past the 80 % threshold (24 min), not yet expired.
    store
        .save(
            "default",
            &Credential::OAuth(stored_token(1500, Some("R1"))),
        )
        .unwrap();
    let (s, env) = settings(&server, &[]);
    let provider = auth::resolve_provider(&s, &env, store.clone()).unwrap();

    let h = provider.authorization().await.unwrap();
    assert_eq!(h.to_str().unwrap(), "Bearer A2");
    assert!(h.is_sensitive());
    let saved = match store.load("default").unwrap().unwrap() {
        Credential::OAuth(t) => t,
        Credential::ApiToken { .. } => panic!("expected an OAuth credential"),
    };
    assert_eq!(
        saved.refresh_token.unwrap().expose_secret(),
        "R2",
        "rotation persisted"
    );
    assert_eq!(saved.client_id, "zdk");
    assert_eq!(saved.subdomain, "test");
    assert!(provider.rotation_error().is_none());

    // Nothing due now: no extra POST.
    assert_eq!(
        provider.authorization().await.unwrap().to_str().unwrap(),
        "Bearer A2"
    );

    // A 401 → invalidate → forced refresh with the *new* refresh token.
    assert!(provider.invalidate().await.unwrap());
    assert_eq!(
        provider.authorization().await.unwrap().to_str().unwrap(),
        "Bearer A3"
    );
    let saved = match store.load("default").unwrap().unwrap() {
        Credential::OAuth(t) => t,
        Credential::ApiToken { .. } => panic!("expected an OAuth credential"),
    };
    assert_eq!(saved.refresh_token.unwrap().expose_secret(), "R3");
    assert_eq!(saved.access_token.expose_secret(), "A3");

    let posts = token_posts(&server).await;
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0]["refresh_token"], "R1");
    assert_eq!(posts[1]["refresh_token"], "R2");
    assert!(posts.iter().all(|p| !p.contains_key("client_secret")));
    let d = provider.description();
    assert_eq!(d.grant, GrantKind::AuthorizationCode);
    assert!(d.has_refresh_token);
    assert_eq!(
        d.scopes,
        vec!["tickets:read", "users:read"],
        "scopes from the response"
    );
}

#[tokio::test]
async fn rotation_persist_failure_keeps_the_fresh_bearer_and_reports_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("refresh_token=R1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("A2", Some("R2"))))
        .expect(1)
        .mount(&server)
        .await;

    let memory = Arc::new(MemoryStore::new());
    memory
        .save(
            "default",
            &Credential::OAuth(stored_token(1500, Some("R1"))),
        )
        .unwrap();
    memory.set_fail_saves(true);
    let store: SharedStore = memory.clone();
    let (s, env) = settings(&server, &[]);
    let provider = auth::resolve_provider(&s, &env, store.clone()).unwrap();

    let h = provider.authorization().await.unwrap();
    assert_eq!(
        h.to_str().unwrap(),
        "Bearer A2",
        "this process keeps working"
    );
    let err = provider.rotation_error().expect("rotation error recorded");
    assert!(err.contains("could not be saved"), "{err}");
    assert!(err.contains("zdk auth login"), "{err}");

    // The next call still works from the cache and does not retry the burned token.
    assert_eq!(
        provider.authorization().await.unwrap().to_str().unwrap(),
        "Bearer A2"
    );
    assert_eq!(token_posts(&server).await.len(), 1);
    // The store still holds the old (now burned) refresh token: exactly what exit 10 warns about.
    if let Credential::OAuth(t) = store.load("default").unwrap().unwrap() {
        assert_eq!(t.refresh_token.unwrap().expose_secret(), "R1");
    }
}

#[tokio::test]
async fn expired_token_with_dead_refresh_token_is_revoked() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "revoked elsewhere"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let store: SharedStore = Arc::new(MemoryStore::new());
    store
        .save(
            "default",
            &Credential::OAuth(stored_token(3600, Some("R1"))),
        )
        .unwrap();
    let (s, env) = settings(&server, &[]);
    let provider = auth::resolve_provider(&s, &env, store).unwrap();
    let err = provider.authorization().await.unwrap_err();
    assert_eq!(err.error_code(), "AUTH_REVOKED");
    assert_eq!(err.exit_code(), 3);
}

#[tokio::test]
async fn client_credentials_provider_remints_from_env_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("client_secret=from-env"))
        .and(body_string_contains("expires_in=1800"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "CC2", "token_type": "bearer", "expires_in": 1800, "scope": "tickets:read"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let store: SharedStore = Arc::new(MemoryStore::new());
    let mut t = stored_token(1800, None);
    t.grant = GrantKind::ClientCredentials;
    store.save("default", &Credential::OAuth(t)).unwrap();
    let (s, env) = settings(&server, &[("ZENDESK_CLIENT_SECRET", "from-env")]);
    let provider = auth::resolve_provider(&s, &env, store.clone()).unwrap();
    assert!(
        provider.invalidate().await.unwrap(),
        "a secret is available"
    );
    assert_eq!(
        provider.authorization().await.unwrap().to_str().unwrap(),
        "Bearer CC2"
    );
    if let Credential::OAuth(t) = store.load("default").unwrap().unwrap() {
        assert_eq!(t.access_token.expose_secret(), "CC2");
        assert!(t.client_secret.is_none(), "env secret is never persisted");
        assert!(t.refresh_token.is_none());
    }
}

#[tokio::test]
async fn revoke_current_at_looks_up_then_deletes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/oauth/tokens/current"))
        .and(header("authorization", "Bearer A1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "token": { "id": 4242, "client_id": 7, "scopes": ["read"] }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/oauth/tokens/4242"))
        .and(header("authorization", "Bearer A1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let bearer = auth::bearer_header(&SecretString::from("A1".to_string())).unwrap();
    let id = revoke::revoke_current_at(&http(), &base(&server), &bearer)
        .await
        .unwrap();
    assert_eq!(id, 4242);

    // Errors are returned, not swallowed.
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/oauth/tokens/current"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&other)
        .await;
    let err = revoke::revoke_current_at(&http(), &base(&other), &bearer)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("401"), "{err}");
}

#[tokio::test]
async fn logout_revokes_then_deletes_and_records_failures() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/oauth/tokens/current"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"token": {"id": 9}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v2/oauth/tokens/9"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let store: SharedStore = Arc::new(MemoryStore::new());
    store
        .save("default", &Credential::OAuth(stored_token(0, Some("R1"))))
        .unwrap();
    let (s, _env) = settings(&server, &[]);
    let report = auth::logout(&s, &store, auth::LogoutOptions::default())
        .await
        .unwrap();
    assert_eq!(report.revoked, vec!["default"]);
    assert_eq!(report.removed, vec!["default"]);
    assert!(report.revoke_errors.is_empty());
    assert!(store.load("default").unwrap().is_none());

    // Revocation failure still removes the credential and is reported.
    let failing = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/oauth/tokens/current"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&failing)
        .await;
    store
        .save("default", &Credential::OAuth(stored_token(0, Some("R1"))))
        .unwrap();
    let (s, _env) = settings(&failing, &[]);
    let report = auth::logout(&s, &store, auth::LogoutOptions::default())
        .await
        .unwrap();
    assert_eq!(report.removed, vec!["default"]);
    assert_eq!(report.revoke_errors.len(), 1);
    assert_eq!(report.revoke_errors[0].0, "default");
    assert!(store.load("default").unwrap().is_none());
}

// ---------------------------------------------------------------------------------------------
// Login flows end to end (loopback listener + token exchange)
// ---------------------------------------------------------------------------------------------

/// Plain HTTP GET with the standard library (reqwest's blocking client is not enabled).
fn raw_get(url: &Url) {
    use std::io::{Read, Write};
    let host = url.host_str().expect("host");
    let port = url.port().expect("port");
    let mut stream =
        std::net::TcpStream::connect((host, port)).expect("connect to the loopback listener");
    let path_and_query = format!("{}?{}", url.path(), url.query().unwrap_or(""));
    write!(
        stream,
        "GET {path_and_query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out);
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
}

#[tokio::test]
async fn browser_login_flow_exchanges_the_code_immediately_and_saves_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains("client_id=zdk_local"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("A9", Some("R9"))))
        .expect(1)
        .mount(&server)
        .await;

    let seen_challenge: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let announced: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let challenge_sink = seen_challenge.clone();
    let announced_sink = announced.clone();

    // The "browser": read redirect_uri/state/challenge off the authorize URL and complete the
    // redirect from another thread, exactly as a real browser would.
    let open_browser: auth::authorization_code::BrowserOpener = Box::new(move |url: &Url| {
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["scope"], "tickets:read users:read");
        *challenge_sink.lock().unwrap() = Some(q["code_challenge"].clone());
        let mut redirect = Url::parse(&q["redirect_uri"]).unwrap();
        assert_eq!(redirect.host_str(), Some("127.0.0.1"));
        redirect
            .query_pairs_mut()
            .append_pair("code", "the-code")
            .append_pair("state", &q["state"]);
        std::thread::spawn(move || raw_get(&redirect));
        Ok(())
    });
    let on_url: auth::authorization_code::UrlHook = Box::new(move |url: &Url| {
        *announced_sink.lock().unwrap() = Some(url.to_string());
    });

    let store: SharedStore = Arc::new(MemoryStore::new());
    let (s, env) = settings(&server, &[]);
    let opts = LoginOptions {
        client_id: Some("zdk_local".into()),
        scopes: vec!["tickets:read".into(), "users:read".into()],
        port: Some(0),
        timeout: Some(std::time::Duration::from_secs(15)),
        open_browser: Some(open_browser),
        on_authorize_url: Some(on_url),
        ..Default::default()
    };
    let token = auth::login_authorization_code(&s, &env, &store, opts)
        .await
        .unwrap();
    assert_eq!(token.access_token.expose_secret(), "A9");
    assert_eq!(token.subdomain, "test");
    assert_eq!(token.client_id, "zdk_local");
    assert!(token.client_secret.is_none());
    assert!(
        announced
            .lock()
            .unwrap()
            .as_deref()
            .unwrap()
            .contains("/oauth/authorizations/new?")
    );

    // PKCE: the verifier sent on exchange hashes to the challenge sent on authorize.
    let posts = token_posts(&server).await;
    assert_eq!(posts.len(), 1);
    let verifier = &posts[0]["code_verifier"];
    assert_eq!(verifier.len(), 43);
    assert_eq!(
        Some(challenge_for(verifier)),
        *seen_challenge.lock().unwrap()
    );
    assert!(posts[0]["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(posts[0]["redirect_uri"].ends_with("/callback"));

    match store.load("default").unwrap().unwrap() {
        Credential::OAuth(t) => assert_eq!(t.access_token.expose_secret(), "A9"),
        Credential::ApiToken { .. } => panic!("expected an OAuth credential"),
    }
}

#[tokio::test]
async fn manual_login_flow_reads_the_pasted_redirect_and_checks_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("code=pasted"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A8484%2Fcallback",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("A5", Some("R5"))))
        .expect(1)
        .mount(&server)
        .await;

    let state_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let sink = state_seen.clone();
    let on_url: auth::authorization_code::UrlHook = Box::new(move |url: &Url| {
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:8484/callback");
        *sink.lock().unwrap() = Some(q["state"].clone());
    });
    let source = state_seen.clone();
    let reader: auth::authorization_code::LineReader = Box::new(move || -> Result<String> {
        let state = source.lock().unwrap().clone().expect("URL announced first");
        Ok(format!(
            "http://127.0.0.1:8484/callback?code=pasted&state={state}\n"
        ))
    });

    let store: SharedStore = Arc::new(MemoryStore::new());
    let (s, env) = settings(&server, &[("ZENDESK_CLIENT_ID", "zdk")]);
    let opts = LoginOptions {
        scopes: vec!["tickets:read".into()],
        no_browser: true,
        stdin_reader: Some(reader),
        on_authorize_url: Some(on_url),
        open_browser: Some(Box::new(|_| panic!("no browser in --no-browser mode"))),
        ..Default::default()
    };
    let token = auth::login_authorization_code(&s, &env, &store, opts)
        .await
        .unwrap();
    assert_eq!(token.access_token.expose_secret(), "A5");
    assert!(store.load("default").unwrap().is_some());

    // A pasted redirect with a foreign state is refused before any exchange.
    let reader: auth::authorization_code::LineReader =
        Box::new(|| Ok("http://127.0.0.1:8484/callback?code=x&state=forged".into()));
    let opts = LoginOptions {
        scopes: vec!["tickets:read".into()],
        no_browser: true,
        stdin_reader: Some(reader),
        ..Default::default()
    };
    let err = auth::login_authorization_code(&s, &env, &store, opts)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ZdkError::Auth(AuthFailure::FlowAborted { .. })),
        "{err}"
    );
    assert_eq!(token_posts(&server).await.len(), 1);
}

#[tokio::test]
async fn client_credentials_login_stores_the_secret_only_when_asked() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("client_id=zdk_ci"))
        .and(body_string_contains("client_secret=cli-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "CC9", "token_type": "bearer", "expires_in": 3600, "scope": "tickets:read"
        })))
        .expect(2)
        .mount(&server)
        .await;
    let store: SharedStore = Arc::new(MemoryStore::new());
    let (s, env) = settings(&server, &[]);
    let opts = LoginOptions {
        client_id: Some("zdk_ci".into()),
        client_secret: Some(SecretString::from("cli-secret".to_string())),
        scopes: vec!["tickets:read".into()],
        ..Default::default()
    };
    let t = auth::login_client_credentials(&s, &env, &store, opts)
        .await
        .unwrap();
    assert_eq!(t.access_token.expose_secret(), "CC9");
    assert!(t.client_secret.is_none());

    let opts = LoginOptions {
        client_id: Some("zdk_ci".into()),
        client_secret: Some(SecretString::from("cli-secret".to_string())),
        scopes: vec!["tickets:read".into()],
        store_secret: true,
        ..Default::default()
    };
    let t = auth::login_client_credentials(&s, &env, &store, opts)
        .await
        .unwrap();
    assert_eq!(t.client_secret.unwrap().expose_secret(), "cli-secret");
    let st = auth::status(&s, &env, &store).unwrap();
    assert_eq!(st.description.grant, GrantKind::ClientCredentials);
    assert!(st.can_renew, "stored secret allows re-mint");
    assert!(!st.description.has_refresh_token);
}

#[tokio::test]
async fn refresh_sends_the_env_client_secret_for_confidential_clients_without_storing_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=R1"))
        .and(body_string_contains("client_secret=conf-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("A2", Some("R2"))))
        .expect(1)
        .mount(&server)
        .await;
    let store: SharedStore = Arc::new(MemoryStore::new());
    store
        .save(
            "default",
            &Credential::OAuth(stored_token(1500, Some("R1"))),
        )
        .unwrap();
    let (s, env) = settings(&server, &[("ZENDESK_CLIENT_SECRET", "conf-secret")]);
    let provider = auth::resolve_provider(&s, &env, store.clone()).unwrap();
    assert_eq!(
        provider.authorization().await.unwrap().to_str().unwrap(),
        "Bearer A2"
    );
    match store.load("default").unwrap().unwrap() {
        Credential::OAuth(t) => {
            assert_eq!(t.refresh_token.unwrap().expose_secret(), "R2");
            assert!(
                t.client_secret.is_none(),
                "runtime secret is never persisted"
            );
        }
        Credential::ApiToken { .. } => panic!("expected an OAuth credential"),
    }

    // `auth refresh --force` takes the same path.
    Mock::given(method("POST"))
        .and(path("/oauth/tokens"))
        .and(body_string_contains("refresh_token=R2"))
        .and(body_string_contains("client_secret=conf-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("A3", Some("R3"))))
        .expect(1)
        .mount(&server)
        .await;
    let fresh = auth::refresh_now(&s, &env, &store, true).await.unwrap();
    assert_eq!(fresh.access_token.expose_secret(), "A3");
    assert!(fresh.client_secret.is_none());
}
