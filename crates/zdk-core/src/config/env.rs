//! The one place that reads the process environment (PRD §13.2).
//!
//! Every `ZENDESK_*` variable is read exactly once into [`EnvOverrides`]; the rest of the
//! workspace receives that struct instead of calling `std::env::var`, which `clippy.toml`
//! forbids everywhere else (`disallowed-methods`). Tests build the struct with
//! [`EnvOverrides::from_pairs`] so they never depend on the real environment.

// This module is the single sanctioned reader of the process environment.
#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use secrecy::SecretString;

/// Every environment variable the CLI honours, read once at startup.
///
/// Numeric and enum-valued variables are kept as raw strings here and parsed (with a
/// `ZdkError::Config` on failure) in [`super::settings::Settings::resolve`], so a typo in
/// `ZENDESK_PAGE_SIZE` is reported with the variable name rather than silently ignored.
#[derive(Clone, Default)]
pub struct EnvOverrides {
    /// `ZENDESK_SUBDOMAIN`
    pub subdomain: Option<String>,
    /// `ZENDESK_CLIENT_ID`
    pub client_id: Option<String>,
    /// `ZENDESK_CLIENT_SECRET`
    pub client_secret: Option<SecretString>,
    /// `ZENDESK_ACCESS_TOKEN` — bypasses the credential store entirely.
    pub access_token: Option<SecretString>,
    /// `ZENDESK_REFRESH_TOKEN`
    pub refresh_token: Option<SecretString>,
    /// `ZENDESK_SCOPES` — comma- or space-separated granular scopes.
    pub scopes: Option<Vec<String>>,
    /// `ZENDESK_EMAIL` — legacy API-token auth.
    pub email: Option<String>,
    /// `ZENDESK_API_TOKEN` — legacy API-token auth (deprecated by Zendesk).
    pub api_token: Option<SecretString>,
    /// `ZENDESK_PROFILE`
    pub profile: Option<String>,
    /// `ZENDESK_CONFIG` — config file path.
    pub config: Option<PathBuf>,
    /// `ZENDESK_OUTPUT`
    pub output: Option<String>,
    /// `ZENDESK_PAGE_SIZE`
    pub page_size: Option<String>,
    /// `ZENDESK_MAX_CONCURRENCY`
    pub max_concurrency: Option<String>,
    /// `ZENDESK_RATE_LIMIT_STRATEGY`
    pub rate_limit_strategy: Option<String>,
    /// `ZENDESK_NO_CACHE` — any non-empty value other than `0`/`false` enables it.
    pub no_cache: bool,
    /// `ZENDESK_CREDENTIAL_STORE`
    pub credential_store: Option<String>,
    /// `ZENDESK_CREDENTIALS_PASSPHRASE` — key for the encrypted file store.
    pub credentials_passphrase: Option<SecretString>,
    /// `ZENDESK_LOG` — tracing filter.
    pub log: Option<String>,
    /// `ZENDESK_BASE_URL` — base URL override (tests, proxies).
    pub base_url: Option<String>,
    /// `NO_COLOR` — set (to anything) disables colour.
    pub no_color: bool,
    /// `VISUAL`, then `EDITOR` — for `zdk config edit`.
    pub editor: Option<String>,
    /// `XDG_CONFIG_HOME` — honoured on every OS when set.
    pub xdg_config_home: Option<PathBuf>,
    /// `XDG_STATE_HOME`
    pub xdg_state_home: Option<PathBuf>,
    /// `XDG_CACHE_HOME`
    pub xdg_cache_home: Option<PathBuf>,
}

impl fmt::Debug for EnvOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = |s: &Option<SecretString>| s.as_ref().map(|_| "[redacted]");
        f.debug_struct("EnvOverrides")
            .field("subdomain", &self.subdomain)
            .field("client_id", &self.client_id)
            .field("client_secret", &redacted(&self.client_secret))
            .field("access_token", &redacted(&self.access_token))
            .field("refresh_token", &redacted(&self.refresh_token))
            .field("scopes", &self.scopes)
            .field("email", &self.email)
            .field("api_token", &redacted(&self.api_token))
            .field("profile", &self.profile)
            .field("config", &self.config)
            .field("output", &self.output)
            .field("page_size", &self.page_size)
            .field("max_concurrency", &self.max_concurrency)
            .field("rate_limit_strategy", &self.rate_limit_strategy)
            .field("no_cache", &self.no_cache)
            .field("credential_store", &self.credential_store)
            .field(
                "credentials_passphrase",
                &redacted(&self.credentials_passphrase),
            )
            .field("log", &self.log)
            .field("base_url", &self.base_url)
            .field("no_color", &self.no_color)
            .field("editor", &self.editor)
            .field("xdg_config_home", &self.xdg_config_home)
            .field("xdg_state_home", &self.xdg_state_home)
            .field("xdg_cache_home", &self.xdg_cache_home)
            .finish()
    }
}

/// Names of the variables that hold secrets, for `config show --effective` masking.
pub const SECRET_VARS: &[&str] = &[
    "ZENDESK_CLIENT_SECRET",
    "ZENDESK_ACCESS_TOKEN",
    "ZENDESK_REFRESH_TOKEN",
    "ZENDESK_API_TOKEN",
    "ZENDESK_CREDENTIALS_PASSPHRASE",
];

impl EnvOverrides {
    /// Read the real process environment. Call once, early in `main`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()))
    }

    /// Build from explicit `(NAME, VALUE)` pairs — for tests and embedding.
    pub fn from_pairs<I, K, V>(vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let map: HashMap<String, String> = vars
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        Self::from_lookup(|key| map.get(key).cloned())
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let non_empty = |key: &str| get(key).filter(|v| !v.trim().is_empty());
        let secret = |key: &str| non_empty(key).map(SecretString::from);
        let flag = |key: &str| {
            get(key).is_some_and(|v| {
                let v = v.trim();
                !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false"))
            })
        };

        Self {
            subdomain: non_empty("ZENDESK_SUBDOMAIN"),
            client_id: non_empty("ZENDESK_CLIENT_ID"),
            client_secret: secret("ZENDESK_CLIENT_SECRET"),
            access_token: secret("ZENDESK_ACCESS_TOKEN"),
            refresh_token: secret("ZENDESK_REFRESH_TOKEN"),
            scopes: non_empty("ZENDESK_SCOPES").map(|s| split_scopes(&s)),
            email: non_empty("ZENDESK_EMAIL"),
            api_token: secret("ZENDESK_API_TOKEN"),
            profile: non_empty("ZENDESK_PROFILE"),
            config: non_empty("ZENDESK_CONFIG").map(PathBuf::from),
            output: non_empty("ZENDESK_OUTPUT"),
            page_size: non_empty("ZENDESK_PAGE_SIZE"),
            max_concurrency: non_empty("ZENDESK_MAX_CONCURRENCY"),
            rate_limit_strategy: non_empty("ZENDESK_RATE_LIMIT_STRATEGY"),
            no_cache: flag("ZENDESK_NO_CACHE"),
            credential_store: non_empty("ZENDESK_CREDENTIAL_STORE"),
            credentials_passphrase: secret("ZENDESK_CREDENTIALS_PASSPHRASE"),
            log: non_empty("ZENDESK_LOG"),
            base_url: non_empty("ZENDESK_BASE_URL"),
            // NO_COLOR's contract is "present and non-empty" (https://no-color.org).
            no_color: non_empty("NO_COLOR").is_some(),
            editor: non_empty("VISUAL").or_else(|| non_empty("EDITOR")),
            xdg_config_home: non_empty("XDG_CONFIG_HOME").map(PathBuf::from),
            xdg_state_home: non_empty("XDG_STATE_HOME").map(PathBuf::from),
            xdg_cache_home: non_empty("XDG_CACHE_HOME").map(PathBuf::from),
        }
    }

    /// Which secret-bearing variables are set (names only, never values).
    #[must_use]
    pub fn secrets_present(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.client_secret.is_some() {
            out.push("ZENDESK_CLIENT_SECRET");
        }
        if self.access_token.is_some() {
            out.push("ZENDESK_ACCESS_TOKEN");
        }
        if self.refresh_token.is_some() {
            out.push("ZENDESK_REFRESH_TOKEN");
        }
        if self.api_token.is_some() {
            out.push("ZENDESK_API_TOKEN");
        }
        if self.credentials_passphrase.is_some() {
            out.push("ZENDESK_CREDENTIALS_PASSPHRASE");
        }
        out
    }

    /// The secret value for a variable name, if set (for `--reveal-secrets`).
    #[must_use]
    pub fn secret(&self, name: &str) -> Option<&SecretString> {
        match name {
            "ZENDESK_CLIENT_SECRET" => self.client_secret.as_ref(),
            "ZENDESK_ACCESS_TOKEN" => self.access_token.as_ref(),
            "ZENDESK_REFRESH_TOKEN" => self.refresh_token.as_ref(),
            "ZENDESK_API_TOKEN" => self.api_token.as_ref(),
            "ZENDESK_CREDENTIALS_PASSPHRASE" => self.credentials_passphrase.as_ref(),
            _ => None,
        }
    }
}

/// `ZENDESK_SCOPES` accepts commas or whitespace (Zendesk itself uses spaces).
fn split_scopes(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn reads_every_documented_variable() {
        let env = EnvOverrides::from_pairs([
            ("ZENDESK_SUBDOMAIN", "acme"),
            ("ZENDESK_CLIENT_ID", "zdk"),
            ("ZENDESK_CLIENT_SECRET", "s3cret"),
            ("ZENDESK_ACCESS_TOKEN", "tok"),
            ("ZENDESK_REFRESH_TOKEN", "ref"),
            ("ZENDESK_SCOPES", "tickets:read, users:read hc:read"),
            ("ZENDESK_EMAIL", "a@b.c"),
            ("ZENDESK_API_TOKEN", "api"),
            ("ZENDESK_PROFILE", "prod"),
            ("ZENDESK_CONFIG", "/tmp/c.toml"),
            ("ZENDESK_OUTPUT", "json"),
            ("ZENDESK_PAGE_SIZE", "50"),
            ("ZENDESK_MAX_CONCURRENCY", "2"),
            ("ZENDESK_RATE_LIMIT_STRATEGY", "fail"),
            ("ZENDESK_NO_CACHE", "1"),
            ("ZENDESK_CREDENTIAL_STORE", "file"),
            ("ZENDESK_CREDENTIALS_PASSPHRASE", "pass"),
            ("ZENDESK_LOG", "zdk=debug"),
            ("ZENDESK_BASE_URL", "http://127.0.0.1:1"),
            ("NO_COLOR", "1"),
            ("EDITOR", "vi"),
            ("VISUAL", "code -w"),
            ("XDG_CONFIG_HOME", "/x/config"),
            ("XDG_STATE_HOME", "/x/state"),
            ("XDG_CACHE_HOME", "/x/cache"),
        ]);
        assert_eq!(env.subdomain.as_deref(), Some("acme"));
        assert_eq!(env.client_id.as_deref(), Some("zdk"));
        assert_eq!(
            env.client_secret.as_ref().unwrap().expose_secret(),
            "s3cret"
        );
        assert_eq!(env.access_token.as_ref().unwrap().expose_secret(), "tok");
        assert_eq!(env.refresh_token.as_ref().unwrap().expose_secret(), "ref");
        assert_eq!(
            env.scopes.as_deref(),
            Some(
                &[
                    "tickets:read".to_string(),
                    "users:read".into(),
                    "hc:read".into()
                ][..]
            )
        );
        assert_eq!(env.email.as_deref(), Some("a@b.c"));
        assert_eq!(env.api_token.as_ref().unwrap().expose_secret(), "api");
        assert_eq!(env.profile.as_deref(), Some("prod"));
        assert_eq!(
            env.config.as_deref(),
            Some(std::path::Path::new("/tmp/c.toml"))
        );
        assert_eq!(env.output.as_deref(), Some("json"));
        assert_eq!(env.page_size.as_deref(), Some("50"));
        assert_eq!(env.max_concurrency.as_deref(), Some("2"));
        assert_eq!(env.rate_limit_strategy.as_deref(), Some("fail"));
        assert!(env.no_cache);
        assert_eq!(env.credential_store.as_deref(), Some("file"));
        assert_eq!(
            env.credentials_passphrase.as_ref().unwrap().expose_secret(),
            "pass"
        );
        assert_eq!(env.log.as_deref(), Some("zdk=debug"));
        assert_eq!(env.base_url.as_deref(), Some("http://127.0.0.1:1"));
        assert!(env.no_color);
        assert_eq!(
            env.editor.as_deref(),
            Some("code -w"),
            "VISUAL wins over EDITOR"
        );
        assert_eq!(
            env.xdg_config_home.as_deref(),
            Some(std::path::Path::new("/x/config"))
        );
        assert_eq!(
            env.xdg_state_home.as_deref(),
            Some(std::path::Path::new("/x/state"))
        );
        assert_eq!(
            env.xdg_cache_home.as_deref(),
            Some(std::path::Path::new("/x/cache"))
        );
        assert_eq!(env.secrets_present().len(), 5);
    }

    #[test]
    fn empty_values_are_treated_as_unset_and_flags_understand_false() {
        let env = EnvOverrides::from_pairs([
            ("ZENDESK_SUBDOMAIN", "  "),
            ("NO_COLOR", ""),
            ("ZENDESK_NO_CACHE", "false"),
        ]);
        assert!(env.subdomain.is_none());
        assert!(!env.no_color);
        assert!(!env.no_cache);
        assert!(env.secrets_present().is_empty());
    }

    #[test]
    fn debug_never_prints_secret_values() {
        let env = EnvOverrides::from_pairs([
            ("ZENDESK_ACCESS_TOKEN", "hunter2"),
            ("ZENDESK_API_TOKEN", "hunter3"),
        ]);
        let dbg = format!("{env:?}");
        assert!(!dbg.contains("hunter"), "{dbg}");
        assert!(dbg.contains("[redacted]"));
    }
}
