//! `specs/SPEC_VERSIONS.toml` (the source of truth for upstream URLs and snapshot metadata),
//! downloads, and `cargo xtask spec-refresh`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use toml_edit::{DocumentMut, value};

use crate::openapi::SpecName;
use crate::openapi::walk::Document;

pub const SPEC_VERSIONS_PATH: &str = "specs/SPEC_VERSIONS.toml";
const DOWNLOAD_ATTEMPTS: u32 = 3;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_SPEC_BYTES: u64 = 64 << 20;

/// One `[spec]` table of `SPEC_VERSIONS.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct SpecEntry {
    pub url: String,
    pub file: String,
    #[serde(default)]
    pub openapi: String,
    #[serde(default)]
    pub info_version: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub fetched_at: String,
    #[serde(default)]
    pub paths: usize,
    #[serde(default)]
    pub operations: usize,
}

/// All entries in canonical order (`support`, `help_center`, `voice`).
#[derive(Debug, Clone)]
pub struct SpecVersions {
    pub entries: Vec<(SpecName, SpecEntry)>,
}

/// The repository root (the parent of `xtask/`).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

pub fn load(root: &Path) -> Result<SpecVersions> {
    let path = root.join(SPEC_VERSIONS_PATH);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn parse(text: &str) -> Result<SpecVersions> {
    let table: toml::Table = toml::from_str(text)?;
    for key in table.keys() {
        if SpecName::parse(key).is_none() {
            bail!("unknown spec table [{key}] (expected one of support, help_center, voice)");
        }
    }
    let mut entries = Vec::new();
    for spec in SpecName::ALL {
        let item = table
            .get(spec.as_str())
            .ok_or_else(|| anyhow!("missing [{spec}] table"))?;
        let entry: SpecEntry = item
            .clone()
            .try_into()
            .with_context(|| format!("[{spec}]"))?;
        entries.push((spec, entry));
    }
    Ok(SpecVersions { entries })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// GET `url` with a global timeout, retrying transient failures.
pub fn download(url: &str) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .user_agent("zendesk-cli-xtask (+https://github.com/osodevops/zendesk-cli)")
        .build()
        .into();
    let mut last_err = None;
    for attempt in 1..=DOWNLOAD_ATTEMPTS {
        let result = agent
            .get(url)
            .call()
            .map_err(anyhow::Error::from)
            .and_then(|mut resp| {
                resp.body_mut()
                    .with_config()
                    .limit(MAX_SPEC_BYTES)
                    .read_to_vec()
                    .map_err(anyhow::Error::from)
            });
        match result {
            Ok(bytes) if bytes.is_empty() => last_err = Some(anyhow!("empty response body")),
            Ok(bytes) => return Ok(bytes),
            Err(e) => last_err = Some(e),
        }
        if attempt < DOWNLOAD_ATTEMPTS {
            let wait = Duration::from_secs(u64::from(attempt) * 2);
            eprintln!(
                "warning: download of {url} failed (attempt {attempt}/{DOWNLOAD_ATTEMPTS}), retrying in {wait:?}"
            );
            std::thread::sleep(wait);
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("download failed")))
        .with_context(|| format!("downloading {url} ({DOWNLOAD_ATTEMPTS} attempts)"))
}

/// A freshly downloaded upstream document that has passed the walker.
#[derive(Debug)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub doc: Document,
}

/// Download `entry.url` and refuse anything the walker cannot parse.
pub fn fetch(spec: SpecName, entry: &SpecEntry) -> Result<Fetched> {
    let bytes = download(&entry.url)?;
    let text = String::from_utf8(bytes.clone())
        .with_context(|| format!("{spec}: upstream document is not UTF-8"))?;
    let doc = Document::parse(spec, &text)
        .with_context(|| format!("{spec}: upstream document rejected by the walker"))?;
    let sha256 = sha256_hex(&bytes);
    Ok(Fetched { bytes, sha256, doc })
}

/// `cargo xtask spec-refresh [--spec name]`.
pub fn refresh(root: &Path, only: Option<&str>) -> Result<()> {
    let versions = load(root)?;
    let only = match only {
        Some(name) => Some(
            SpecName::parse(name)
                .ok_or_else(|| anyhow!("unknown spec {name:?} (support | help_center | voice)"))?,
        ),
        None => None,
    };
    let toml_path = root.join(SPEC_VERSIONS_PATH);
    let toml_text = std::fs::read_to_string(&toml_path)?;
    let mut doc: DocumentMut = toml_text
        .parse()
        .with_context(|| format!("parsing {}", toml_path.display()))?;
    let mut changed = 0;

    for (spec, entry) in &versions.entries {
        if only.is_some_and(|o| o != *spec) {
            continue;
        }
        println!("{spec}: fetching {}", entry.url);
        let fetched = fetch(*spec, entry)?;
        let ops = fetched.doc.operations.len();
        let paths = fetched.doc.path_count;
        if fetched.sha256 == entry.sha256 {
            println!(
                "{spec}: unchanged (sha256 {}, {paths} paths, {ops} operations)",
                &fetched.sha256[..12]
            );
            continue;
        }
        let target = root.join("specs").join(&entry.file);
        std::fs::write(&target, &fetched.bytes)
            .with_context(|| format!("writing {}", target.display()))?;
        let table = doc
            .get_mut(spec.as_str())
            .and_then(toml_edit::Item::as_table_mut)
            .ok_or_else(|| anyhow!("[{spec}] table missing from SPEC_VERSIONS.toml"))?;
        table["openapi"] = value(fetched.doc.openapi.as_str());
        table["info_version"] = value(fetched.doc.info_version.as_str());
        table["sha256"] = value(fetched.sha256.as_str());
        table["fetched_at"] = value(now_rfc3339());
        table["paths"] = value(i64::try_from(paths).unwrap_or(i64::MAX));
        table["operations"] = value(i64::try_from(ops).unwrap_or(i64::MAX));
        changed += 1;
        println!(
            "{spec}: updated {} ({} bytes, sha256 {}, openapi {}, version {}, {paths} paths, {ops} operations; was {} paths, {} operations)",
            entry.file,
            fetched.bytes.len(),
            &fetched.sha256[..12],
            fetched.doc.openapi,
            fetched.doc.info_version,
            entry.paths,
            entry.operations
        );
    }

    if changed > 0 {
        std::fs::write(&toml_path, doc.to_string())
            .with_context(|| format!("writing {}", toml_path.display()))?;
        println!("updated {SPEC_VERSIONS_PATH} ({changed} spec(s)); now run `cargo xtask codegen`");
    } else {
        println!("all specs unchanged");
    }
    Ok(())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_versions_in_canonical_order() {
        let v = load(&workspace_root()).expect("load");
        let names: Vec<SpecName> = v.entries.iter().map(|(s, _)| *s).collect();
        assert_eq!(names, SpecName::ALL.to_vec());
        for (spec, e) in &v.entries {
            assert!(
                e.url.starts_with("https://developer.zendesk.com/"),
                "{spec}"
            );
            assert_eq!(
                e.sha256.len(),
                64,
                "{spec}: sha256 must be filled by spec-refresh"
            );
            assert!(
                e.fetched_at.ends_with('Z'),
                "{spec}: fetched_at must be RFC3339 UTC"
            );
        }
        assert_eq!(v.entries[0].1.file, "support.yaml");
    }

    #[test]
    fn committed_sha256_matches_files() {
        let root = workspace_root();
        for (spec, e) in load(&root).expect("load").entries {
            let bytes = std::fs::read(root.join("specs").join(&e.file)).expect("spec file");
            assert_eq!(
                sha256_hex(&bytes),
                e.sha256,
                "{spec}: run `cargo xtask spec-refresh`"
            );
        }
    }

    #[test]
    fn rejects_unknown_tables() {
        assert!(parse("[support]\nurl='u'\nfile='f'\n[help_center]\nurl='u'\nfile='f'\n[voice]\nurl='u'\nfile='f'\n").is_ok());
        assert!(parse("[chat]\nurl='u'\nfile='f'\n").is_err());
        assert!(
            parse("[support]\nurl='u'\nfile='f'\n").is_err(),
            "all three tables are required"
        );
    }

    #[test]
    fn sha256_hex_is_lowercase_hex() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
