//! Black-box harness: every test gets a private HOME/XDG tree under `CARGO_TARGET_TMPDIR`,
//! the memory credential store, a fixed subdomain and no colour — and never sees the
//! developer's real `ZENDESK_*` environment.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;

pub struct Harness {
    _tmp: tempfile::TempDir,
    pub home: PathBuf,
}

impl Harness {
    pub fn new() -> Self {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(root).expect("target tmpdir");
        let tmp = tempfile::Builder::new()
            .prefix("zdk-")
            .tempdir_in(root)
            .expect("tempdir");
        let home = tmp.path().to_path_buf();
        Self { _tmp: tmp, home }
    }

    /// Where the binary will look for `config.toml` under this harness.
    pub fn config_path(&self) -> PathBuf {
        self.home
            .join("config")
            .join("zendesk-cli")
            .join("config.toml")
    }

    /// A `zdk` command with the isolated environment applied.
    pub fn zdk(&self) -> Command {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("zdk"));
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("ZENDESK_")
                || matches!(
                    name.as_ref(),
                    "VISUAL"
                        | "EDITOR"
                        | "NO_COLOR"
                        | "XDG_CONFIG_HOME"
                        | "XDG_STATE_HOME"
                        | "XDG_CACHE_HOME"
                )
            {
                cmd.env_remove(key);
            }
        }
        cmd.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("ZENDESK_CREDENTIAL_STORE", "none")
            .env("ZENDESK_SUBDOMAIN", "test")
            .env("NO_COLOR", "1")
            .env("COLUMNS", "100");
        cmd
    }

    /// Write `toml` as the config file (parents created).
    pub fn write_config(&self, toml: &str) -> PathBuf {
        let path = self.config_path();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, toml).expect("write config");
        path
    }

    pub fn read_config(&self) -> String {
        std::fs::read_to_string(self.config_path()).expect("read config")
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a command's stdout as JSON.
pub fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}):\n{}",
            String::from_utf8_lossy(bytes)
        )
    })
}

/// Config used by the mock-server tests: no backoff, no local rate-limit buckets.
pub const FAST_CONFIG: &str =
    "[retry]\nmax_attempts = 3\nbase_ms = 0\nmax_ms = 0\n\n[rate_limit]\nstrategy = \"burst\"\n";

impl Harness {
    /// A `zdk` command pointed at a mock Zendesk (`ZENDESK_BASE_URL`) with a static bearer
    /// token (`ZENDESK_ACCESS_TOKEN=test-token`). Writes [`FAST_CONFIG`] unless a config exists.
    pub fn zdk_api(&self, base_url: &str) -> Command {
        if !self.config_path().exists() {
            self.write_config(FAST_CONFIG);
        }
        let mut cmd = self.zdk();
        cmd.env("ZENDESK_BASE_URL", base_url)
            .env("ZENDESK_ACCESS_TOKEN", "test-token");
        cmd
    }
}

/// The JSON error line the binary prints on stderr in machine formats.
pub fn stderr_error(stderr: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON error line on stderr:\n{text}"));
    serde_json::from_str(line).unwrap_or_else(|e| panic!("stderr line is not JSON ({e}): {line}"))
}
