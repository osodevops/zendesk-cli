//! `zdk doctor` — local configuration, credential store, credentials, one authenticated call
//! (identity, clock skew, rate-limit headers and plan hint) and the operation registry
//! (PRD §8.27). Every check is rendered; exit 1 when any check fails.

use chrono::{DateTime, Utc};
use clap::Args;
use serde_json::{Map, Value, json};
use zdk_core::api;
use zdk_core::auth::api_token::{DEADLINE_ALL_TOKENS_DEAD, DEADLINE_NO_NEW_TOKENS, days_until};
use zdk_core::auth::{self, AuthStatus, GrantKind};
use zdk_core::config::ConfigFile;
use zdk_core::error::AuthFailure;
use zdk_core::http::{ApiResponse, RequestSpec};
use zdk_core::output::OutputFormat;
use zdk_core::store::{self, MemoryStore, SharedStore, StoreDecision, StoreKind, StoreSelector};
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

/// Clock drift beyond this (seconds) is worth a warning: TLS and token expiry get flaky.
const CLOCK_SKEW_WARN_SECS: i64 = 60;
/// Warn when API tokens have fewer days than this before they stop working.
const API_TOKEN_WARN_DAYS: i64 = 60;

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Machine-readable output (same as -o json)
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug)]
struct Check {
    name: &'static str,
    status: Status,
    detail: String,
    /// Machine-readable error code when the check failed because of a `ZdkError`.
    code: Option<&'static str>,
    /// Structured facts a script may want (plan hint, skew, limits).
    data: Map<String, Value>,
}

impl Check {
    fn new(name: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
            code: None,
            data: Map::new(),
        }
    }

    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, Status::Ok, detail)
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, Status::Warn, detail)
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, Status::Fail, detail)
    }

    fn from_error(name: &'static str, err: &ZdkError) -> Self {
        let mut c = Self::fail(name, err.to_string());
        c.code = Some(err.error_code());
        c
    }

    fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.data.insert(key.to_string(), value.into());
        self
    }

    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("name".into(), self.name.into());
        map.insert("status".into(), self.status.as_str().into());
        map.insert("detail".into(), self.detail.clone().into());
        if let Some(code) = self.code {
            map.insert("code".into(), code.into());
        }
        for (k, v) in &self.data {
            map.insert(k.clone(), v.clone());
        }
        Value::Object(map)
    }
}

pub async fn run(args: DoctorArgs, ctx: &AppContext) -> Result<()> {
    let mut checks = Vec::with_capacity(9);
    checks.push(check_config(ctx));
    checks.push(check_profile(ctx));
    let (store_check, store) = check_store(ctx);
    checks.push(store_check);
    let (credential_check, status) = check_credentials(ctx, store);
    checks.push(credential_check);
    if let Some(c) = check_api_token_deadline(status.as_ref()) {
        checks.push(c);
    }
    let (auth_check, response) = check_auth(ctx).await;
    checks.push(auth_check);
    checks.push(check_clock(response.as_ref()));
    checks.push(check_rate_limit(ctx, response.as_ref()));
    checks.push(check_registry());

    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| c.status == Status::Fail)
        .map(|c| c.name)
        .collect();
    render(&args, ctx, &checks, failed.is_empty())?;
    ctx.finish()?;
    if failed.is_empty() {
        Ok(())
    } else {
        Err(ZdkError::Other(format!(
            "{} doctor check(s) failed: {}",
            failed.len(),
            failed.join(", ")
        )))
    }
}

fn render(args: &DoctorArgs, ctx: &AppContext, checks: &[Check], ok: bool) -> Result<()> {
    let human = !args.json && ctx.output == OutputFormat::Table && ctx.settings.output.jq.is_none();
    if human {
        let rows: Vec<Value> = checks
            .iter()
            .map(|c| json!({ "check": c.name, "status": c.status.as_str(), "detail": c.detail }))
            .collect();
        return ctx.emit(Value::Array(rows), None);
    }
    let value = json!({
        "ok": ok,
        "checks": checks.iter().map(Check::to_json).collect::<Vec<_>>(),
    });
    if args.json && ctx.output == OutputFormat::Table {
        let opts = ctx.render_options(None);
        return zdk_core::output::render(OutputFormat::Json, value, &opts);
    }
    ctx.emit(value, None)
}

// ---------------------------------------------------------------------------------------------
// local checks
// ---------------------------------------------------------------------------------------------

fn check_config(ctx: &AppContext) -> Check {
    let path = &ctx.settings.paths.config_file;
    if !path.exists() {
        return Check::warn(
            "config",
            format!(
                "no config file at {} — built-in defaults apply (run `zdk config init`)",
                path.display()
            ),
        );
    }
    match ConfigFile::load(path) {
        Ok(file) => {
            let unknown = file.unknown_keys();
            let profiles = file.profiles.len();
            if unknown.is_empty() {
                Check::ok(
                    "config",
                    format!("{} ({profiles} profile(s))", path.display()),
                )
            } else {
                Check::warn(
                    "config",
                    format!(
                        "{} parses; {} unknown key(s) ignored: {}",
                        path.display(),
                        unknown.len(),
                        unknown.join(", ")
                    ),
                )
            }
        }
        Err(e) => Check::from_error("config", &e),
    }
}

fn check_profile(ctx: &AppContext) -> Check {
    let name = &ctx.settings.profile_name;
    let in_file = ConfigFile::load(&ctx.settings.paths.config_file)
        .is_ok_and(|f| f.profiles.contains_key(name));
    let Some(subdomain) = &ctx.settings.subdomain else {
        return Check::fail(
            "profile",
            format!(
                "profile '{name}' has no subdomain — pass --subdomain, set ZENDESK_SUBDOMAIN, or run `zdk config init`"
            ),
        );
    };
    let base_note = match ctx.settings.base_url.as_ref().map(url::Url::as_str) {
        Some(u) if !u.starts_with(&format!("https://{subdomain}.zendesk.com")) => {
            format!(" (base URL override: {u})")
        }
        _ => String::new(),
    };
    let detail = format!("profile '{name}' → {subdomain}.zendesk.com{base_note}");
    if in_file {
        Check::ok("profile", detail)
    } else {
        Check::warn(
            "profile",
            format!(
                "{detail}; the profile is not in the config file (subdomain from a flag or ZENDESK_SUBDOMAIN)"
            ),
        )
    }
}

/// Forces a fresh keyring probe so the report reflects the machine as it is now.
fn check_store(ctx: &AppContext) -> (Check, Option<SharedStore>) {
    if ctx.env.access_token.is_some() {
        return (
            Check::ok(
                "credential_store",
                "env (ZENDESK_ACCESS_TOKEN) — the credential store is bypassed",
            )
            .with("backend", "env"),
            Some(std::sync::Arc::new(MemoryStore::new())),
        );
    }
    if ctx.env.api_token.is_some() {
        return (
            Check::ok(
                "credential_store",
                "env (ZENDESK_EMAIL + ZENDESK_API_TOKEN) — the credential store is bypassed",
            )
            .with("backend", "env"),
            Some(std::sync::Arc::new(MemoryStore::new())),
        );
    }
    let selector = ctx.settings.credential_store;
    let paths = &ctx.settings.paths;
    match store::open_with(selector, paths, &ctx.env, true) {
        Ok(store) => {
            let kind = store.kind();
            let check = match (selector, kind) {
                (StoreSelector::Auto, StoreKind::Keyring) => {
                    Check::ok("credential_store", "keyring (OS keychain probe succeeded)")
                }
                (StoreSelector::Auto, StoreKind::File) => {
                    let why = StoreDecision::read(paths)
                        .and_then(|d| d.reason)
                        .unwrap_or_else(|| "keyring probe failed".into());
                    Check::warn(
                        "credential_store",
                        format!(
                            "encrypted file {} — keyring unavailable: {why}",
                            paths.config_dir.join("credentials.enc").display()
                        ),
                    )
                }
                (_, StoreKind::Keyring) => Check::ok(
                    "credential_store",
                    format!("keyring (--credential-store {})", selector.as_str()),
                ),
                (_, StoreKind::File) => Check::ok(
                    "credential_store",
                    format!(
                        "encrypted file {} (--credential-store {})",
                        paths.config_dir.join("credentials.enc").display(),
                        selector.as_str()
                    ),
                ),
                (_, StoreKind::Env) => Check::ok(
                    "credential_store",
                    "env (read-only: ZENDESK_ACCESS_TOKEN / ZENDESK_EMAIL + ZENDESK_API_TOKEN)",
                ),
                (_, StoreKind::Memory) => Check::warn(
                    "credential_store",
                    "none (in-memory only — credentials are not persisted between commands)",
                ),
            };
            (check.with("backend", kind.as_str()), Some(store))
        }
        Err(e) => (Check::from_error("credential_store", &e), None),
    }
}

fn check_credentials(ctx: &AppContext, store: Option<SharedStore>) -> (Check, Option<AuthStatus>) {
    let Some(store) = store else {
        return (
            Check::fail(
                "credentials",
                "skipped: the credential store could not be opened",
            ),
            None,
        );
    };
    match auth::status(&ctx.settings, &ctx.env, &store) {
        Ok(s) => {
            let d = &s.description;
            let grant = d.grant.as_str();
            let profile = &d.profile;
            let check = match d.grant {
                GrantKind::StaticToken => Check::ok(
                    "credentials",
                    format!("static token from ZENDESK_ACCESS_TOKEN for profile '{profile}' (never refreshed)"),
                ),
                GrantKind::ApiToken => Check::ok(
                    "credentials",
                    format!(
                        "API token for {} on profile '{profile}' (deprecated — see api_token_deadline)",
                        s.email.as_deref().unwrap_or("?")
                    ),
                ),
                GrantKind::AuthorizationCode | GrantKind::ClientCredentials => {
                    let scopes = if d.scopes.is_empty() {
                        "no scopes recorded".to_string()
                    } else {
                        format!("scopes: {}", d.scopes.join(", "))
                    };
                    let refresh = if d.grant == GrantKind::ClientCredentials {
                        format!(
                            "re-mint secret {}",
                            if s.can_renew { "available" } else { "missing" }
                        )
                    } else {
                        format!("refresh token: {}", if d.has_refresh_token { "yes" } else { "no" })
                    };
                    match s.expires_in_secs {
                        Some(secs) if secs <= 0 && s.can_renew => Check::warn(
                            "credentials",
                            format!("{grant} token for profile '{profile}' expired {} ago; the next request renews it ({refresh}; {scopes})", human_secs(secs)),
                        ),
                        Some(secs) if secs <= 0 => Check::fail(
                            "credentials",
                            format!("{grant} token for profile '{profile}' expired {} ago and cannot be renewed ({refresh}) — run `zdk auth login`", human_secs(secs)),
                        ),
                        Some(secs) => Check::ok(
                            "credentials",
                            format!("{grant} token for profile '{profile}' expires in {} ({refresh}; {scopes})", human_secs(secs)),
                        ),
                        None => Check::ok(
                            "credentials",
                            format!("{grant} token for profile '{profile}' with no expiry ({refresh}; {scopes})"),
                        ),
                    }
                }
            }
            .with("grant", grant)
            .with("profile", profile.as_str())
            .with("expires_at", d.expires_at.map_or(Value::Null, |t| json!(t)))
            .with("store", d.store.clone().map_or(Value::Null, Value::String));
            (check, Some(s))
        }
        Err(e @ ZdkError::Auth(AuthFailure::NotLoggedIn { .. })) => (
            {
                let mut c = Check::fail(
                    "credentials",
                    format!(
                        "not logged in for profile '{}' — run `zdk auth login`",
                        ctx.settings.profile_name
                    ),
                );
                c.code = Some(e.error_code());
                c
            },
            None,
        ),
        Err(e) => (Check::from_error("credentials", &e), None),
    }
}

fn check_api_token_deadline(status: Option<&AuthStatus>) -> Option<Check> {
    let status = status?;
    if status.description.grant != GrantKind::ApiToken {
        return Some(Check::ok(
            "api_token_deadline",
            format!(
                "not using API tokens ({} grant)",
                status.description.grant.as_str()
            ),
        ));
    }
    let today = Utc::now().date_naive();
    let dead_in = days_until(DEADLINE_ALL_TOKENS_DEAD, today);
    let no_new_in = days_until(DEADLINE_NO_NEW_TOKENS, today);
    let check = if dead_in <= 0 {
        Check::fail(
            "api_token_deadline",
            format!(
                "API tokens stopped working on 30 Apr 2027 ({} days ago) — run `zdk auth login` to switch to OAuth",
                -dead_in
            ),
        )
    } else if dead_in < API_TOKEN_WARN_DAYS {
        Check::warn(
            "api_token_deadline",
            format!(
                "{dead_in} days until every API token stops working (30 Apr 2027) — run `zdk auth login` to switch to OAuth"
            ),
        )
    } else {
        let cutoff = if no_new_in > 0 {
            format!("; new tokens cannot be created after 27 Oct 2026 ({no_new_in} days)")
        } else {
            "; Zendesk no longer issues new API tokens".to_string()
        };
        Check::ok(
            "api_token_deadline",
            format!("{dead_in} days until every API token stops working (30 Apr 2027){cutoff}"),
        )
    };
    Some(check.with("days_remaining", dead_in).with(
        "days_until_creation_cutoff",
        (no_new_in > 0).then_some(no_new_in),
    ))
}

// ---------------------------------------------------------------------------------------------
// the one authenticated request
// ---------------------------------------------------------------------------------------------

async fn check_auth(ctx: &AppContext) -> (Check, Option<ApiResponse>) {
    let client = match ctx.client().await {
        Ok(c) => c,
        Err(e) => return (Check::from_error("auth", &e), None),
    };
    match client
        .execute(RequestSpec::get("/api/v2/users/me").no_preflight(true))
        .await
    {
        Ok(resp) => {
            let body = resp.value().unwrap_or(Value::Null);
            let user = body.get("user").cloned().unwrap_or(body);
            let name = user["name"].as_str().unwrap_or("?").to_string();
            let role = user["role"].as_str().unwrap_or("?").to_string();
            let email = user["email"].as_str().map(str::to_string);
            let who = match &email {
                Some(e) => format!("{name} <{e}> ({role})"),
                None => format!("{name} ({role})"),
            };
            let check = Check::ok(
                "auth",
                format!(
                    "GET /api/v2/users/me → {who} in {} ms",
                    resp.elapsed.as_millis()
                ),
            )
            .with("user", name)
            .with("role", role)
            .with("email", email.map_or(Value::Null, Value::String))
            .with("user_id", user.get("id").cloned().unwrap_or(Value::Null));
            (check, Some(resp))
        }
        Err(ZdkError::DryRun) => (Check::warn("auth", "skipped (--dry-run)"), None),
        Err(e) => (Check::from_error("auth", &e), None),
    }
}

fn check_clock(response: Option<&ApiResponse>) -> Check {
    let Some(resp) = response else {
        return Check::warn(
            "clock_skew",
            "skipped (no authenticated response to compare against)",
        );
    };
    let Some(date) = resp.headers.get("date").and_then(|v| v.to_str().ok()) else {
        return Check::warn("clock_skew", "the response carried no Date header");
    };
    match DateTime::parse_from_rfc2822(date) {
        Ok(server) => {
            let skew = (server.with_timezone(&Utc) - Utc::now()).num_seconds();
            let check = if skew.abs() > CLOCK_SKEW_WARN_SECS {
                Check::warn(
                    "clock_skew",
                    format!(
                        "local clock is {} {} Zendesk — TLS validation and token expiry can misbehave; sync the system clock",
                        human_secs(skew),
                        if skew > 0 { "behind" } else { "ahead of" }
                    ),
                )
            } else {
                Check::ok(
                    "clock_skew",
                    format!("local clock within {}s of Zendesk", skew.abs()),
                )
            };
            check.with("skew_secs", skew)
        }
        Err(e) => Check::warn(
            "clock_skew",
            format!("could not parse the response Date header '{date}': {e}"),
        ),
    }
}

/// Zendesk's documented per-plan account limits (requests per minute).
fn plan_hint(limit: u32) -> Option<&'static str> {
    match limit {
        200 => Some("Team"),
        400 => Some("Growth/Professional"),
        700 => Some("Enterprise"),
        2500 => Some("Enterprise Plus/High Volume"),
        _ => None,
    }
}

fn check_rate_limit(ctx: &AppContext, response: Option<&ApiResponse>) -> Check {
    let Some(resp) = response else {
        return Check::warn(
            "rate_limit",
            "skipped (no authenticated response to read headers from)",
        );
    };
    let Some(limit) = resp.rate.limit else {
        return Check::warn(
            "rate_limit",
            "the response carried no rate-limit headers (X-Rate-Limit / RateLimit-Limit)",
        );
    };
    let remaining = resp.rate.remaining;
    let plan = plan_hint(limit);
    let configured = ctx.settings.profile.plan.as_deref();
    let plan_text = match (plan, configured) {
        (Some(p), Some(c)) if !c.eq_ignore_ascii_case(p) => {
            format!("plan hint: {p} (profile says '{c}')")
        }
        (Some(p), _) => format!("plan hint: {p}"),
        (None, Some(c)) => format!("plan hint: unknown limit (profile says '{c}')"),
        (None, None) => "plan hint: unknown limit".to_string(),
    };
    let remaining_text = remaining.map_or("?".to_string(), |r| r.to_string());
    Check::ok(
        "rate_limit",
        format!("limit {limit}/min, remaining {remaining_text} — {plan_text}"),
    )
    .with("limit", limit)
    .with("remaining", remaining.map_or(Value::Null, |r| json!(r)))
    .with("plan", plan.map_or(Value::Null, Value::from))
}

fn check_registry() -> Check {
    let ops = api::operations().len();
    let versions = api::spec_versions();
    let summary: Vec<String> = versions
        .iter()
        .map(|v| {
            format!(
                "{} {} ({} ops, fetched {})",
                v.spec.as_str(),
                v.info_version,
                v.operations,
                v.fetched_at
            )
        })
        .collect();
    if ops == 0 || versions.is_empty() {
        return Check::fail(
            "registry",
            "the generated operation registry is empty — rebuild with `cargo xtask generate`",
        );
    }
    Check::ok(
        "registry",
        format!(
            "{ops} operations from {} spec(s): {}",
            versions.len(),
            summary.join("; ")
        ),
    )
    .with("operations", ops)
    .with("specs", versions.len())
}

/// `1d 2h`, `29m 59s`, `45s` (sign dropped).
fn human_secs(secs: i64) -> String {
    let secs = secs.unsigned_abs();
    let (d, h, m, s) = (
        secs / 86_400,
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60,
    );
    let parts: Vec<String> = [(d, "d"), (h, "h"), (m, "m"), (s, "s")]
        .into_iter()
        .filter(|(n, unit)| *n > 0 || (*unit == "s" && d == 0 && h == 0 && m == 0))
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect();
    parts.into_iter().take(2).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_hints_follow_the_documented_limits() {
        assert_eq!(plan_hint(200), Some("Team"));
        assert_eq!(plan_hint(400), Some("Growth/Professional"));
        assert_eq!(plan_hint(700), Some("Enterprise"));
        assert_eq!(plan_hint(2500), Some("Enterprise Plus/High Volume"));
        assert_eq!(plan_hint(100), None);
    }

    #[test]
    fn check_json_carries_code_and_data() {
        let c = Check::from_error(
            "auth",
            &ZdkError::Auth(AuthFailure::NotLoggedIn {
                profile: "p".into(),
            }),
        )
        .with("extra", 1);
        let v = c.to_json();
        assert_eq!(v["name"], "auth");
        assert_eq!(v["status"], "fail");
        assert_eq!(v["code"], "AUTH_NOT_LOGGED_IN");
        assert_eq!(v["extra"], 1);
        assert!(Check::ok("x", "fine").to_json().get("code").is_none());
    }

    #[test]
    fn registry_check_is_ok_on_the_committed_specs() {
        let c = check_registry();
        assert_eq!(c.status, Status::Ok);
        assert!(c.detail.contains("operations"));
    }

    #[test]
    fn human_secs_drops_sign_and_keeps_two_units() {
        assert_eq!(human_secs(-90), "1m 30s");
        assert_eq!(human_secs(3661), "1h 1m");
        assert_eq!(human_secs(0), "0s");
    }
}
