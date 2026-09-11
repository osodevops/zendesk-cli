//! Authorization code + PKCE (PRD §6.1): PKCE/state generation, the authorize URL, the
//! single-request loopback listener, redirect parsing for paste flows, and the
//! [`LoginFlow`] orchestrator that exchanges the code the instant it arrives.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use url::Url;

use super::GrantKind;
use super::oauth::{self, AUTHORIZE_PATH, CodeExchange};
use super::token::TokenSet;
use crate::error::AuthFailure;
use crate::{Result, ZdkError};

/// Path the loopback listener answers on.
pub const CALLBACK_PATH: &str = "/callback";
/// How long the browser flow waits for the redirect.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// Zendesk authorization codes are single-use and valid for this long.
pub const CODE_LIFETIME_SECS: u64 = 120;

/// Opens the authorize URL in a browser (injectable so tests launch nothing).
pub type BrowserOpener = Box<dyn Fn(&Url) -> Result<()> + Send + Sync>;
/// Reads one line (the pasted code or redirect URL) in manual flows.
pub type LineReader = Box<dyn FnMut() -> Result<String> + Send>;
/// Told the authorize URL so the CLI can print it; core never prints.
pub type UrlHook = Box<dyn Fn(&Url) + Send + Sync>;

/// PKCE verifier + S256 challenge.
#[derive(Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"[redacted]")
            .field("challenge", &self.challenge)
            .finish()
    }
}

fn random_urlsafe(len: usize) -> Result<String> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes)
        .map_err(|e| ZdkError::Other(format!("cannot generate random bytes: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(&bytes))
}

/// S256 challenge for a verifier (RFC 7636 §4.2).
#[must_use]
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// 32 random bytes → 43-char base64url verifier, plus its S256 challenge.
pub fn pkce() -> Result<Pkce> {
    let verifier = random_urlsafe(32)?;
    let challenge = challenge_for(&verifier);
    Ok(Pkce {
        verifier,
        challenge,
    })
}

/// 16 random bytes → 22-char base64url `state`.
pub fn state() -> Result<String> {
    random_urlsafe(16)
}

/// `{base}/oauth/authorizations/new?response_type=code&client_id=…&redirect_uri=…&scope=…
/// &state=…&code_challenge=…&code_challenge_method=S256` (spaces as `%20`).
pub fn authorize_url(
    base: &Url,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
    challenge: &str,
) -> Result<Url> {
    let mut url = oauth::endpoint(base, AUTHORIZE_PATH)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    // form-urlencoding writes spaces as `+`; literal `+` is already `%2B`, so this is safe.
    let query = url.query().map(|q| q.replace('+', "%20"));
    url.set_query(query.as_deref());
    Ok(url)
}

/// What came back from the authorization server.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizationResponse {
    pub code: String,
    /// Absent when the user pasted a bare code.
    pub state: Option<String>,
}

impl std::fmt::Debug for AuthorizationResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationResponse")
            .field("code", &"[redacted]")
            .field("state", &self.state)
            .finish()
    }
}

/// Interpret one redirect's query string.
enum Redirect {
    Code(AuthorizationResponse),
    Denied(String),
    Missing(&'static str),
}

fn interpret_query(url: &Url) -> Redirect {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" => error_description = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(err) = error {
        return Redirect::Denied(match error_description {
            Some(d) if !d.is_empty() => format!("{err}: {d}"),
            _ => err,
        });
    }
    match code {
        Some(c) if !c.is_empty() => Redirect::Code(AuthorizationResponse { code: c, state }),
        _ => Redirect::Missing("code"),
    }
}

/// Parse what a user pasted in `--no-browser` / `--redirect-uri` flows: a full redirect URL,
/// a bare `code=…&state=…` query, or just the code.
pub fn parse_redirect(input: &str) -> Result<AuthorizationResponse> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ZdkError::Auth(AuthFailure::FlowAborted {
            detail: "no authorization code was provided".into(),
        }));
    }
    let as_url = if input.contains("://") {
        Url::parse(input).ok()
    } else if input.contains("code=") {
        Url::parse(&format!(
            "http://localhost/?{}",
            input.trim_start_matches('?')
        ))
        .ok()
    } else {
        None
    };
    let Some(url) = as_url else {
        if input.chars().any(char::is_whitespace) {
            return Err(ZdkError::Auth(AuthFailure::FlowAborted {
                detail: "expected an authorization code or the full redirect URL".into(),
            }));
        }
        return Ok(AuthorizationResponse {
            code: input.to_string(),
            state: None,
        });
    };
    match interpret_query(&url) {
        Redirect::Code(r) => Ok(r),
        Redirect::Denied(detail) => Err(ZdkError::Auth(AuthFailure::FlowAborted { detail })),
        Redirect::Missing(what) => Err(ZdkError::Auth(AuthFailure::FlowAborted {
            detail: format!("the pasted URL has no `{what}` parameter"),
        })),
    }
}

/// A single-use HTTP listener on `127.0.0.1` for the browser redirect.
pub struct LoopbackListener {
    server: tiny_http::Server,
    addr: SocketAddr,
    redirect_uri: Url,
}

impl std::fmt::Debug for LoopbackListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopbackListener")
            .field("addr", &self.addr)
            .field("redirect_uri", &self.redirect_uri.as_str())
            .finish_non_exhaustive()
    }
}

impl LoopbackListener {
    /// Bind `127.0.0.1:port` (`0` = any free port; read it back with [`port`](Self::port)).
    pub fn bind(port: u16) -> Result<Self> {
        let server = tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| {
            ZdkError::Auth(AuthFailure::FlowAborted {
                detail: format!(
                    "cannot listen on 127.0.0.1:{port} for the browser redirect: {e}. \
                     Pass --port to choose another port, or use --no-browser"
                ),
            })
        })?;
        let addr = server.server_addr().to_ip().ok_or_else(|| {
            ZdkError::Auth(AuthFailure::FlowAborted {
                detail: "loopback listener has no IP address".into(),
            })
        })?;
        let redirect_uri = Url::parse(&format!("http://127.0.0.1:{}{CALLBACK_PATH}", addr.port()))?;
        Ok(Self {
            server,
            addr,
            redirect_uri,
        })
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// `http://127.0.0.1:{port}/callback`
    #[must_use]
    pub fn redirect_uri(&self) -> &Url {
        &self.redirect_uri
    }

    /// Serve requests until one carries a code with the expected `state`, the user denies,
    /// or `timeout` elapses. Runs on the blocking pool.
    pub async fn wait_for_code(
        self,
        expected_state: String,
        timeout: Duration,
    ) -> Result<AuthorizationResponse> {
        let Self { server, .. } = self;
        tokio::task::spawn_blocking(move || serve(&server, &expected_state, timeout))
            .await
            .map_err(|e| ZdkError::Other(format!("loopback listener task failed: {e}")))?
    }
}

fn serve(
    server: &tiny_http::Server,
    expected_state: &str,
    timeout: Duration,
) -> Result<AuthorizationResponse> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ZdkError::Auth(AuthFailure::FlowAborted {
                detail: format!(
                    "timed out after {}s waiting for the browser redirect",
                    timeout.as_secs()
                ),
            }));
        }
        let request = match server.recv_timeout(remaining) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                return Err(ZdkError::Auth(AuthFailure::FlowAborted {
                    detail: format!("loopback listener failed: {e}"),
                }));
            }
        };
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{}", request.url())) else {
            respond(request, 400, "Bad request.");
            continue;
        };
        if url.path() != CALLBACK_PATH {
            respond(request, 404, "Not found.");
            continue;
        }
        match interpret_query(&url) {
            Redirect::Denied(detail) => {
                respond(
                    request,
                    200,
                    "Login was cancelled. You can close this tab and return to the terminal.",
                );
                return Err(ZdkError::Auth(AuthFailure::FlowAborted { detail }));
            }
            Redirect::Missing(what) => {
                respond(request, 400, &format!("Missing `{what}` parameter."));
            }
            Redirect::Code(r) => {
                if r.state.as_deref() != Some(expected_state) {
                    tracing::warn!(target: "zdk::auth", "ignoring redirect with unexpected state");
                    respond(
                        request,
                        400,
                        "State mismatch — this redirect was not started by zdk. Keep waiting or restart `zdk auth login`.",
                    );
                    continue;
                }
                respond(
                    request,
                    200,
                    "Login complete. You can close this tab and return to the terminal.",
                );
                return Ok(r);
            }
        }
    }
}

fn respond(request: tiny_http::Request, status: u16, message: &str) {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>zendesk-cli</title>\
         <style>body{{font-family:system-ui,sans-serif;margin:3rem;color:#222}}</style></head>\
         <body><h1>zendesk-cli</h1><p>{message}</p></body></html>"
    );
    let mut response = tiny_http::Response::from_string(html).with_status_code(status);
    if let Ok(h) = tiny_http::Header::from_bytes("Content-Type", "text/html; charset=utf-8") {
        response = response.with_header(h);
    }
    if let Ok(h) = tiny_http::Header::from_bytes("Cache-Control", "no-store") {
        response = response.with_header(h);
    }
    if let Err(e) = request.respond(response) {
        tracing::debug!(target: "zdk::auth", error = %e, "could not answer the browser");
    }
}

/// Everything fixed for one login attempt.
#[derive(Clone)]
pub struct LoginFlow {
    pub http: reqwest::Client,
    pub base: Url,
    pub subdomain: String,
    pub client_id: String,
    pub client_secret: Option<SecretString>,
    pub scopes: Vec<String>,
    pub expires_in: Option<u64>,
    pub timeout: Duration,
}

impl std::fmt::Debug for LoginFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginFlow")
            .field("base", &self.base.as_str())
            .field("subdomain", &self.subdomain)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[redacted]"),
            )
            .field("scopes", &self.scopes)
            .field("expires_in", &self.expires_in)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl LoginFlow {
    /// Browser flow: bind the loopback listener, announce + open the URL, wait, exchange.
    pub async fn run_browser(
        &self,
        port: u16,
        open: &BrowserOpener,
        announce: Option<&UrlHook>,
    ) -> Result<TokenSet> {
        let listener = LoopbackListener::bind(port)?;
        let redirect_uri = listener.redirect_uri().to_string();
        let pkce = pkce()?;
        let state = state()?;
        let url = authorize_url(
            &self.base,
            &self.client_id,
            &redirect_uri,
            &self.scopes,
            &state,
            &pkce.challenge,
        )?;
        if let Some(hook) = announce {
            hook(&url);
        }
        if let Err(e) = open(&url) {
            tracing::warn!(target: "zdk::auth", error = %e, "could not open a browser; visit the URL manually");
        }
        let response = listener.wait_for_code(state, self.timeout).await?;
        self.exchange(
            &response.code,
            &redirect_uri,
            &pkce.verifier,
            Instant::now(),
        )
        .await
    }

    /// Manual flow (`--no-browser`, `--redirect-uri`): announce the URL, read the pasted
    /// code/URL, verify `state` when present, exchange.
    pub async fn run_manual(
        &self,
        redirect_uri: &Url,
        read_line: &mut LineReader,
        announce: Option<&UrlHook>,
    ) -> Result<TokenSet> {
        let pkce = pkce()?;
        let state = state()?;
        let url = authorize_url(
            &self.base,
            &self.client_id,
            redirect_uri.as_str(),
            &self.scopes,
            &state,
            &pkce.challenge,
        )?;
        if let Some(hook) = announce {
            hook(&url);
        }
        let line = read_line()?;
        let arrived = Instant::now();
        let response = parse_redirect(&line)?;
        if let Some(s) = &response.state
            && s != &state
        {
            return Err(ZdkError::Auth(AuthFailure::FlowAborted {
                detail:
                    "the pasted redirect carries a different `state` than this login started with"
                        .into(),
            }));
        }
        self.exchange(
            &response.code,
            redirect_uri.as_str(),
            &pkce.verifier,
            arrived,
        )
        .await
    }

    async fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        arrived: Instant,
    ) -> Result<TokenSet> {
        let req = CodeExchange {
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            code: code.to_string(),
            redirect_uri: redirect_uri.to_string(),
            code_verifier: verifier.to_string(),
            scope: Some(self.scopes.join(" ")),
            expires_in: self.expires_in,
        };
        match oauth::exchange_code_at(&self.http, &self.base, &req).await {
            Ok(resp) => Ok(resp.into_token_set(
                GrantKind::AuthorizationCode,
                &self.subdomain,
                &self.client_id,
                self.client_secret.clone(),
                Utc::now(),
            )),
            Err(ZdkError::Auth(AuthFailure::CodeExpired)) => {
                let elapsed = arrived.elapsed().as_secs();
                tracing::warn!(
                    target: "zdk::auth",
                    elapsed_secs = elapsed,
                    limit_secs = CODE_LIFETIME_SECS,
                    "authorization code rejected as invalid_grant"
                );
                Err(ZdkError::Auth(AuthFailure::CodeExpired))
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_has_rfc7636_shape_and_vector() {
        let p = pkce().unwrap();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(p.challenge.len(), 43);
        assert!(
            p.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(pkce().unwrap().verifier, p.verifier);
        // RFC 7636 appendix B.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(state().unwrap().len(), 22);
        assert!(!format!("{p:?}").contains(&p.verifier));
    }

    #[test]
    fn authorize_url_carries_every_parameter() {
        let base = Url::parse("https://acme.zendesk.com").unwrap();
        let url = authorize_url(
            &base,
            "zdk_local",
            "http://127.0.0.1:8080/callback",
            &["tickets:read".into(), "users:read".into()],
            "st4te",
            "ch4llenge",
        )
        .unwrap();
        assert!(
            url.as_str()
                .starts_with("https://acme.zendesk.com/oauth/authorizations/new?")
        );
        let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "zdk_local");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:8080/callback");
        assert_eq!(q["scope"], "tickets:read users:read");
        assert_eq!(q["state"], "st4te");
        assert_eq!(q["code_challenge"], "ch4llenge");
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(url.as_str().contains("scope=tickets%3Aread%20users%3Aread"));
        assert!(!url.as_str().contains('+'));
    }

    #[test]
    fn parse_redirect_accepts_url_query_or_bare_code() {
        let r = parse_redirect("http://127.0.0.1:8080/callback?code=abc&state=xyz").unwrap();
        assert_eq!(r.code, "abc");
        assert_eq!(r.state.as_deref(), Some("xyz"));
        let r = parse_redirect("  https://localhost/?state=xyz&code=abc  ").unwrap();
        assert_eq!(r.code, "abc");
        let r = parse_redirect("code=abc&state=xyz").unwrap();
        assert_eq!(r.code, "abc");
        assert_eq!(r.state.as_deref(), Some("xyz"));
        let r = parse_redirect("abc").unwrap();
        assert_eq!(r.code, "abc");
        assert!(r.state.is_none());

        let err = parse_redirect("").unwrap_err();
        assert!(matches!(
            err,
            ZdkError::Auth(AuthFailure::FlowAborted { .. })
        ));
        let err = parse_redirect(
            "http://127.0.0.1/callback?error=access_denied&error_description=User+denied",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("access_denied: User denied"),
            "{err}"
        );
        let err = parse_redirect("http://127.0.0.1/callback?state=only").unwrap_err();
        assert!(err.to_string().contains("no `code`"), "{err}");
        assert!(parse_redirect("two words").is_err());
        assert!(!format!("{r:?}").contains("abc"));
    }

    #[test]
    fn listener_binds_ephemeral_loopback_port() {
        let l = LoopbackListener::bind(0).unwrap();
        assert_ne!(l.port(), 0);
        assert_eq!(
            l.redirect_uri().as_str(),
            format!("http://127.0.0.1:{}/callback", l.port())
        );
        assert_eq!(l.addr.ip().to_string(), "127.0.0.1");
    }

    /// A raw HTTP GET on the blocking pool, so the current-thread test runtime keeps polling
    /// the spawned waiter (blocking the test thread with `join` would deadlock it).
    async fn get(port: u16, path: &'static str) -> String {
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                s,
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            out
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn listener_ignores_wrong_state_and_favicon_then_accepts_the_code() {
        let l = LoopbackListener::bind(0).unwrap();
        let port = l.port();
        let waiter = tokio::spawn(l.wait_for_code("good".into(), Duration::from_secs(10)));

        assert!(get(port, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
        assert!(
            get(port, "/callback?code=x&state=bad")
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(
            get(port, "/callback?state=good")
                .await
                .starts_with("HTTP/1.1 400")
        );
        let ok = get(port, "/callback?code=the-code&state=good").await;
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("close this tab"));

        let r = waiter.await.unwrap().unwrap();
        assert_eq!(r.code, "the-code");
        assert_eq!(r.state.as_deref(), Some("good"));
    }

    #[tokio::test]
    async fn listener_times_out_and_reports_denial() {
        let l = LoopbackListener::bind(0).unwrap();
        let err = l
            .wait_for_code("s".into(), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");

        let l = LoopbackListener::bind(0).unwrap();
        let port = l.port();
        let waiter = tokio::spawn(l.wait_for_code("s".into(), Duration::from_secs(10)));
        let page = get(port, "/callback?error=access_denied").await;
        assert!(page.contains("cancelled"), "{page}");
        let err = waiter.await.unwrap().unwrap_err();
        assert!(
            matches!(err, ZdkError::Auth(AuthFailure::FlowAborted { ref detail }) if detail == "access_denied"),
            "{err}"
        );
    }
}
