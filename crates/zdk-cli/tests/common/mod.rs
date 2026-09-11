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
