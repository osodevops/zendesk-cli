//! The resolved, effective configuration: CLI → env → profile → `[default]` → built-in.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use url::Url;

use super::{
    ConfigFile, DEFAULT_PROFILE, EnvOverrides, Jitter, Paths, ProfileConfig, RateLimitStrategy,
};
use crate::auth::GrantKind;
use crate::output::OutputFormat;
use crate::store::StoreSelector;
use crate::{Result, ZdkError};

/// Built-in defaults that are not in any config section.
pub const DEFAULT_PAGE_SIZE: u32 = 100;
/// Per-request timeout when nothing overrides it.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// The global CLI flags (PRD §7.1) as plain data. The `clap` layer converts into this so
/// `zdk-core` can be driven — and tested — without argv.
#[derive(Debug, Clone, Default)]
pub struct GlobalArgs {
    pub profile: Option<String>,
    pub subdomain: Option<String>,
    pub output: Option<OutputFormat>,
    pub fields: Vec<String>,
    pub exclude: Vec<String>,
    pub compact: bool,
    pub jq: Option<String>,
    pub all: bool,
    pub limit: Option<u64>,
    pub page_size: Option<u32>,
    pub sideload: Vec<String>,
    pub dry_run: bool,
    pub yes: bool,
    pub idempotency_key: Option<String>,
    pub rate_limit_strategy: Option<RateLimitStrategy>,
    pub max_concurrency: Option<u32>,
    pub timeout: Option<u64>,
    pub retries: Option<u32>,
    pub quiet: bool,
    pub verbosity: u8,
    pub no_color: bool,
    pub audit_log: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub credential_store: Option<StoreSelector>,
    pub checkpoint: Option<PathBuf>,
    pub base_url: Option<String>,
    /// Not a flag: the CLI computes `stdout.is_terminal()` once and passes it here so
    /// output-format and colour detection happen in exactly one place.
    pub stdout_is_tty: bool,
}

/// Output-related settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputSettings {
    pub format: OutputFormat,
    pub fields: Vec<String>,
    pub exclude: Vec<String>,
    pub compact: bool,
    pub jq: Option<String>,
}

/// Rate-governor settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RateLimitSettings {
    pub strategy: RateLimitStrategy,
    pub max_concurrency: u32,
    pub reserve_percent: u8,
    pub warn_threshold: u8,
    pub respect_retry_after: bool,
    pub high_volume_addon: bool,
}

/// Retry/backoff settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetrySettings {
    pub max_attempts: u32,
    pub base_ms: u64,
    pub max_ms: u64,
    pub jitter: Jitter,
    pub retry_on: Vec<u16>,
}

/// Auth settings after merging profile and `[auth]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthSettings {
    pub method: GrantKind,
    pub callback_port: Option<u16>,
    pub auto_refresh: bool,
    pub refresh_at_percent: u8,
    pub suppress_deprecation: bool,
}

/// Everything a command needs, fully resolved. Contains no secrets.
#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub profile_name: String,
    /// The active profile with CLI/env overrides merged in (`grant_type` is always `Some`).
    pub profile: ProfileConfig,
    pub subdomain: Option<String>,
    /// `--base-url`/`ZENDESK_BASE_URL`, else `https://{subdomain}.zendesk.com`; `None` without a subdomain.
    #[serde(serialize_with = "serialize_url")]
    pub base_url: Option<Url>,
    pub output: OutputSettings,
    pub page_size: u32,
    pub all: bool,
    pub limit: Option<u64>,
    pub sideload: Vec<String>,
    pub dry_run: bool,
    pub yes: bool,
    pub confirm_destructive: bool,
    pub idempotency_key: Option<String>,
    pub rate_limit: RateLimitSettings,
    pub retry: RetrySettings,
    /// Same value as `rate_limit.max_concurrency`, surfaced for convenience.
    pub max_concurrency: u32,
    #[serde(serialize_with = "serialize_duration_secs", rename = "timeout_secs")]
    pub timeout: Duration,
    pub quiet: bool,
    pub verbosity: u8,
    /// Colour is on only when stdout is a TTY and neither `NO_COLOR`, `--no-color` nor `[default].color = false` disable it.
    pub color: bool,
    pub resolve_names: bool,
    pub no_cache: bool,
    pub audit_log: Option<PathBuf>,
    pub credential_store: StoreSelector,
    pub checkpoint: Option<PathBuf>,
    pub auth: AuthSettings,
    pub paths: Paths,
}

// serde's `serialize_with` contract fixes the `&Option<T>` shape.
#[allow(clippy::ref_option)]
fn serialize_url<S: serde::Serializer>(
    url: &Option<Url>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match url {
        Some(u) => s.serialize_some(u.as_str()),
        None => s.serialize_none(),
    }
}

fn serialize_duration_secs<S: serde::Serializer>(
    d: &Duration,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_u64(d.as_secs())
}

impl Settings {
    /// Merge every source by precedence. Errors are `ZdkError::Config` naming the offending source.
    pub fn resolve(
        cli: &GlobalArgs,
        env: &EnvOverrides,
        file: &ConfigFile,
        paths: &Paths,
    ) -> Result<Self> {
        let profile_name = cli
            .profile
            .clone()
            .or_else(|| env.profile.clone())
            .or_else(|| file.default.active_profile.clone())
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string());

        let mut profile = file
            .profiles
            .get(&profile_name)
            .cloned()
            .unwrap_or_default();

        // Per-field overrides onto the profile.
        if let Some(s) = cli.subdomain.clone().or_else(|| env.subdomain.clone()) {
            profile.subdomain = Some(s);
        }
        if let Some(c) = env.client_id.clone() {
            profile.client_id = Some(c);
        }
        if let Some(e) = env.email.clone() {
            profile.email = Some(e);
        }
        if let Some(scopes) = env.scopes.clone() {
            profile.scopes = scopes;
        }
        if profile.grant_type.is_none() {
            profile.grant_type = Some(file.auth.method);
        }
        if profile.callback_port.is_none() {
            profile.callback_port = file.auth.callback_port;
        }

        let credential_store = match cli.credential_store {
            Some(s) => s,
            None => match env.credential_store.as_deref() {
                Some(raw) => StoreSelector::parse(raw).ok_or_else(|| {
                    ZdkError::Config(format!("ZENDESK_CREDENTIAL_STORE: '{raw}' is not one of auto, keyring, file, env, none"))
                })?,
                None => profile.credential_store.unwrap_or(file.auth.credential_store),
            },
        };
        profile.credential_store = Some(credential_store);

        let subdomain = profile
            .subdomain
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let base_url = match (&cli.base_url, &env.base_url) {
            (Some(raw), _) => Some(parse_base_url(raw, "--base-url")?),
            (None, Some(raw)) => Some(parse_base_url(raw, "ZENDESK_BASE_URL")?),
            (None, None) => match &subdomain {
                Some(sub) => Some(parse_base_url(
                    &format!("https://{sub}.zendesk.com"),
                    "subdomain",
                )?),
                None => None,
            },
        };

        let output_format = OutputFormat::detect(
            cli.output,
            env.output.as_deref(),
            file.default.output,
            cli.stdout_is_tty,
        )?;

        let page_size = match cli.page_size {
            Some(n) => n,
            None => match env.page_size.as_deref() {
                Some(raw) => parse_env_number::<u32>("ZENDESK_PAGE_SIZE", raw)?,
                None => file.default.page_size.unwrap_or(DEFAULT_PAGE_SIZE),
            },
        };

        let strategy = match cli.rate_limit_strategy {
            Some(s) => s,
            None => match env.rate_limit_strategy.as_deref() {
                Some(raw) => RateLimitStrategy::parse(raw).ok_or_else(|| {
                    ZdkError::Config(format!(
                        "ZENDESK_RATE_LIMIT_STRATEGY: '{raw}' is not one of wait, fail, burst"
                    ))
                })?,
                None => file.rate_limit.strategy,
            },
        };

        let max_concurrency = match cli.max_concurrency {
            Some(n) => n,
            None => match env.max_concurrency.as_deref() {
                Some(raw) => parse_env_number::<u32>("ZENDESK_MAX_CONCURRENCY", raw)?,
                None => file.rate_limit.max_concurrency,
            },
        }
        .max(1);

        let color = cli.stdout_is_tty
            && !cli.no_color
            && !env.no_color
            && file.default.color.unwrap_or(true);

        let auth = AuthSettings {
            method: profile_grant(file, &profile_name),
            callback_port: file
                .profiles
                .get(&profile_name)
                .and_then(|p| p.callback_port)
                .or(file.auth.callback_port),
            auto_refresh: file.auth.auto_refresh,
            refresh_at_percent: file.auth.refresh_at_percent.clamp(1, 99),
            suppress_deprecation: file.auth.suppress_deprecation,
        };

        Ok(Self {
            profile_name,
            profile,
            subdomain,
            base_url,
            output: OutputSettings {
                format: output_format,
                fields: cli.fields.clone(),
                exclude: cli.exclude.clone(),
                compact: cli.compact,
                jq: cli.jq.clone(),
            },
            page_size,
            all: cli.all,
            limit: cli.limit,
            sideload: cli.sideload.clone(),
            dry_run: cli.dry_run,
            yes: cli.yes,
            confirm_destructive: file.default.confirm_destructive.unwrap_or(true),
            idempotency_key: cli.idempotency_key.clone(),
            rate_limit: RateLimitSettings {
                strategy,
                max_concurrency,
                reserve_percent: file.rate_limit.reserve_percent,
                warn_threshold: file.rate_limit.warn_threshold,
                respect_retry_after: file.rate_limit.respect_retry_after,
                high_volume_addon: file.rate_limit.high_volume_addon,
            },
            retry: RetrySettings {
                max_attempts: cli.retries.unwrap_or(file.retry.max_attempts),
                base_ms: file.retry.base_ms,
                max_ms: file.retry.max_ms,
                jitter: file.retry.jitter,
                retry_on: file.retry.retry_on.clone(),
            },
            max_concurrency,
            timeout: Duration::from_secs(cli.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS)),
            quiet: cli.quiet,
            verbosity: cli.verbosity,
            color,
            resolve_names: file.default.resolve_names.unwrap_or(false),
            no_cache: env.no_cache,
            audit_log: cli.audit_log.clone().or_else(|| {
                (file.audit.enabled).then(|| {
                    file.audit.path.as_ref().map_or_else(
                        || paths.state_dir.join("audit.ndjson"),
                        |p| PathBuf::from(expand_tilde(p)),
                    )
                })
            }),
            credential_store,
            checkpoint: cli.checkpoint.clone(),
            auth,
            paths: paths.clone(),
        })
    }

    /// The base URL, or the standard "no subdomain" configuration error.
    pub fn require_base_url(&self) -> Result<&Url> {
        self.base_url.as_ref().ok_or_else(|| {
            ZdkError::Config(format!(
                "no subdomain configured for profile '{}': pass --subdomain, set ZENDESK_SUBDOMAIN, or run `zdk config init`",
                self.profile_name
            ))
        })
    }

    /// `acme` for `https://acme.zendesk.com`; the host for custom base URLs.
    #[must_use]
    pub fn instance_label(&self) -> String {
        self.subdomain
            .clone()
            .or_else(|| {
                self.base_url
                    .as_ref()
                    .and_then(|u| u.host_str().map(str::to_string))
            })
            .unwrap_or_else(|| "(no subdomain)".into())
    }
}

fn profile_grant(file: &ConfigFile, profile_name: &str) -> GrantKind {
    file.profiles
        .get(profile_name)
        .and_then(|p| p.grant_type)
        .unwrap_or(file.auth.method)
}

fn parse_base_url(raw: &str, source: &str) -> Result<Url> {
    let url = Url::parse(raw)
        .map_err(|e| ZdkError::Config(format!("{source}: invalid base URL '{raw}': {e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ZdkError::Config(format!(
            "{source}: invalid base URL '{raw}': expected http(s)://host"
        )));
    }
    Ok(url)
}

fn parse_env_number<T: std::str::FromStr>(name: &str, raw: &str) -> Result<T> {
    raw.trim()
        .parse::<T>()
        .map_err(|_| ZdkError::Config(format!("{name}: '{raw}' is not a valid number")))
}

/// Expand a leading `~/` using `HOME` semantics from `dirs` (no env access here).
fn expand_tilde(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map_or_else(
            || p.to_string(),
            |h| h.join(rest).to_string_lossy().into_owned(),
        ),
        None => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths {
            config_file: "/t/config.toml".into(),
            config_dir: "/t".into(),
            state_dir: "/t/state".into(),
            cache_dir: "/t/cache".into(),
        }
    }

    fn file() -> ConfigFile {
        ConfigFile::from_toml(
            r#"
[default]
active_profile = "prod"
output = "yaml"
page_size = 25
color = true

[auth]
method = "client_credentials"
callback_port = 9000
refresh_at_percent = 70

[rate_limit]
strategy = "burst"
max_concurrency = 8

[retry]
max_attempts = 3

[profiles.prod]
subdomain = "acme"
client_id = "zdk_prod"
grant_type = "authorization_code"
scopes = ["tickets:read"]
callback_port = 8080

[profiles.other]
subdomain = "other"
"#,
        )
        .unwrap()
    }

    #[test]
    fn built_in_defaults_when_nothing_is_configured() {
        let s = Settings::resolve(
            &GlobalArgs::default(),
            &EnvOverrides::default(),
            &ConfigFile::default(),
            &paths(),
        )
        .unwrap();
        assert_eq!(s.profile_name, "default");
        assert!(s.subdomain.is_none());
        assert!(s.base_url.is_none());
        assert_eq!(s.output.format, OutputFormat::Json, "not a TTY → json");
        assert_eq!(s.page_size, 100);
        assert_eq!(s.rate_limit.strategy, RateLimitStrategy::Wait);
        assert_eq!(s.max_concurrency, 4);
        assert_eq!(s.retry.max_attempts, 6);
        assert_eq!(s.timeout, Duration::from_secs(30));
        assert_eq!(s.credential_store, StoreSelector::Auto);
        assert_eq!(s.auth.method, GrantKind::AuthorizationCode);
        assert_eq!(s.auth.refresh_at_percent, 80);
        assert!(!s.color);
        assert!(s.confirm_destructive);
        assert!(s.require_base_url().is_err());
        assert_eq!(s.require_base_url().unwrap_err().exit_code(), 10);
        assert_eq!(s.profile.grant_type, Some(GrantKind::AuthorizationCode));
    }

    #[test]
    fn precedence_matrix_cli_env_profile_default_builtin() {
        let f = file();
        let p = paths();

        // [default] / profile layer only.
        let s =
            Settings::resolve(&GlobalArgs::default(), &EnvOverrides::default(), &f, &p).unwrap();
        assert_eq!(s.profile_name, "prod");
        assert_eq!(s.subdomain.as_deref(), Some("acme"));
        assert_eq!(
            s.base_url.as_ref().unwrap().as_str(),
            "https://acme.zendesk.com/"
        );
        assert_eq!(s.output.format, OutputFormat::Yaml);
        assert_eq!(s.page_size, 25);
        assert_eq!(s.rate_limit.strategy, RateLimitStrategy::Burst);
        assert_eq!(s.max_concurrency, 8);
        assert_eq!(s.retry.max_attempts, 3);
        assert_eq!(
            s.auth.method,
            GrantKind::AuthorizationCode,
            "profile grant_type beats [auth].method"
        );
        assert_eq!(
            s.auth.callback_port,
            Some(8080),
            "profile callback_port beats [auth]"
        );
        assert_eq!(s.auth.refresh_at_percent, 70);

        // Env layer beats file.
        let env = EnvOverrides::from_pairs([
            ("ZENDESK_PROFILE", "other"),
            ("ZENDESK_SUBDOMAIN", "fromenv"),
            ("ZENDESK_OUTPUT", "csv"),
            ("ZENDESK_PAGE_SIZE", "10"),
            ("ZENDESK_RATE_LIMIT_STRATEGY", "fail"),
            ("ZENDESK_MAX_CONCURRENCY", "2"),
            ("ZENDESK_CREDENTIAL_STORE", "file"),
            ("ZENDESK_BASE_URL", "http://127.0.0.1:9999"),
            ("ZENDESK_CLIENT_ID", "env_client"),
        ]);
        let s = Settings::resolve(&GlobalArgs::default(), &env, &f, &p).unwrap();
        assert_eq!(s.profile_name, "other");
        assert_eq!(s.subdomain.as_deref(), Some("fromenv"));
        assert_eq!(
            s.base_url.as_ref().unwrap().as_str(),
            "http://127.0.0.1:9999/"
        );
        assert_eq!(s.output.format, OutputFormat::Csv);
        assert_eq!(s.page_size, 10);
        assert_eq!(s.rate_limit.strategy, RateLimitStrategy::Fail);
        assert_eq!(s.max_concurrency, 2);
        assert_eq!(s.credential_store, StoreSelector::File);
        assert_eq!(s.profile.client_id.as_deref(), Some("env_client"));
        assert_eq!(
            s.auth.method,
            GrantKind::ClientCredentials,
            "'other' has no grant_type → [auth].method"
        );
        assert_eq!(s.auth.callback_port, Some(9000));

        // CLI layer beats env.
        let cli = GlobalArgs {
            profile: Some("prod".into()),
            subdomain: Some("fromcli".into()),
            output: Some(OutputFormat::Ndjson),
            page_size: Some(7),
            rate_limit_strategy: Some(RateLimitStrategy::Wait),
            max_concurrency: Some(1),
            credential_store: Some(StoreSelector::None),
            base_url: Some("https://proxy.example".into()),
            retries: Some(1),
            timeout: Some(5),
            stdout_is_tty: true,
            ..Default::default()
        };
        let s = Settings::resolve(&cli, &env, &f, &p).unwrap();
        assert_eq!(s.profile_name, "prod");
        assert_eq!(s.subdomain.as_deref(), Some("fromcli"));
        assert_eq!(
            s.base_url.as_ref().unwrap().as_str(),
            "https://proxy.example/"
        );
        assert_eq!(s.output.format, OutputFormat::Ndjson);
        assert_eq!(s.page_size, 7);
        assert_eq!(s.rate_limit.strategy, RateLimitStrategy::Wait);
        assert_eq!(s.max_concurrency, 1);
        assert_eq!(s.credential_store, StoreSelector::None);
        assert_eq!(s.retry.max_attempts, 1);
        assert_eq!(s.timeout, Duration::from_secs(5));
    }

    #[test]
    fn colour_requires_tty_and_no_opt_out() {
        let f = ConfigFile::default();
        let p = paths();
        let tty = GlobalArgs {
            stdout_is_tty: true,
            ..Default::default()
        };
        assert!(
            Settings::resolve(&tty, &EnvOverrides::default(), &f, &p)
                .unwrap()
                .color
        );
        assert!(
            !Settings::resolve(&GlobalArgs::default(), &EnvOverrides::default(), &f, &p)
                .unwrap()
                .color
        );
        let no_color = GlobalArgs {
            stdout_is_tty: true,
            no_color: true,
            ..Default::default()
        };
        assert!(
            !Settings::resolve(&no_color, &EnvOverrides::default(), &f, &p)
                .unwrap()
                .color
        );
        let env = EnvOverrides::from_pairs([("NO_COLOR", "1")]);
        assert!(!Settings::resolve(&tty, &env, &f, &p).unwrap().color);
        let mut off = ConfigFile::default();
        off.default.color = Some(false);
        assert!(
            !Settings::resolve(&tty, &EnvOverrides::default(), &off, &p)
                .unwrap()
                .color
        );
    }

    #[test]
    fn output_defaults_to_table_on_a_tty() {
        let tty = GlobalArgs {
            stdout_is_tty: true,
            ..Default::default()
        };
        let s = Settings::resolve(
            &tty,
            &EnvOverrides::default(),
            &ConfigFile::default(),
            &paths(),
        )
        .unwrap();
        assert_eq!(s.output.format, OutputFormat::Table);
    }

    #[test]
    fn bad_env_values_are_config_errors_naming_the_variable() {
        let f = ConfigFile::default();
        let p = paths();
        for (k, v) in [
            ("ZENDESK_PAGE_SIZE", "lots"),
            ("ZENDESK_MAX_CONCURRENCY", "-1"),
            ("ZENDESK_RATE_LIMIT_STRATEGY", "yolo"),
            ("ZENDESK_CREDENTIAL_STORE", "cloud"),
            ("ZENDESK_OUTPUT", "xml"),
            ("ZENDESK_BASE_URL", "not a url"),
        ] {
            let env = EnvOverrides::from_pairs([(k, v)]);
            let err = Settings::resolve(&GlobalArgs::default(), &env, &f, &p).unwrap_err();
            assert_eq!(err.exit_code(), 10, "{k}");
            assert!(err.to_string().contains(k), "{k}: {err}");
        }
    }

    #[test]
    fn audit_log_flag_beats_audit_section() {
        let mut f = ConfigFile::default();
        f.audit.enabled = true;
        f.audit.path = Some("/var/log/zdk.ndjson".into());
        let s = Settings::resolve(
            &GlobalArgs::default(),
            &EnvOverrides::default(),
            &f,
            &paths(),
        )
        .unwrap();
        assert_eq!(
            s.audit_log.as_deref(),
            Some(std::path::Path::new("/var/log/zdk.ndjson"))
        );
        let cli = GlobalArgs {
            audit_log: Some("/tmp/a.ndjson".into()),
            ..Default::default()
        };
        let s = Settings::resolve(&cli, &EnvOverrides::default(), &f, &paths()).unwrap();
        assert_eq!(
            s.audit_log.as_deref(),
            Some(std::path::Path::new("/tmp/a.ndjson"))
        );
        f.audit.enabled = false;
        let s = Settings::resolve(
            &GlobalArgs::default(),
            &EnvOverrides::default(),
            &f,
            &paths(),
        )
        .unwrap();
        assert!(s.audit_log.is_none());
    }

    #[test]
    fn settings_serialise_without_secrets_or_url_type_leaks() {
        let s = Settings::resolve(
            &GlobalArgs::default(),
            &EnvOverrides::default(),
            &file(),
            &paths(),
        )
        .unwrap();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["base_url"], "https://acme.zendesk.com/");
        assert_eq!(json["timeout_secs"], 30);
        assert_eq!(json["profile"]["subdomain"], "acme");
    }
}
