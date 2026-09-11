//! Observers see every attempt the client makes. `--audit-log` is one (NDJSON, PRD §16);
//! tests use [`MemoryObserver`].

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::rate_limit::RateHeaders;

/// One attempt of one request.
#[derive(Debug, Clone, Serialize)]
pub struct RequestRecord {
    pub ts: DateTime<Utc>,
    pub profile: String,
    pub method: String,
    /// Path plus query (secrets redacted).
    pub path: String,
    /// `None` when no response arrived.
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub rate: RateHeaders,
    pub request_id: Option<String>,
    /// Response body size.
    pub bytes: u64,
    /// The invoking command line (secret flag values redacted).
    pub command: String,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Something that wants to see every attempt.
pub trait RequestObserver: Send + Sync + fmt::Debug {
    fn observe(&self, record: &RequestRecord);
}

/// Appends one JSON line per attempt to a file created with owner-only permissions.
#[derive(Debug)]
pub struct AuditLogObserver {
    path: PathBuf,
    lock: Mutex<()>,
    failed: AtomicBool,
}

impl AuditLogObserver {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
            failed: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            crate::util::fs::ensure_dir(dir)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let _guard = self
            .lock
            .lock()
            .map_err(|_| std::io::Error::other("audit log lock poisoned"))?;
        let mut file = options.open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")
    }
}

impl RequestObserver for AuditLogObserver {
    fn observe(&self, record: &RequestRecord) {
        let Ok(line) = serde_json::to_string(record) else {
            return;
        };
        if let Err(e) = self.append(&line) {
            // Warn once; an unwritable audit log must not fail the command.
            if !self.failed.swap(true, Ordering::Relaxed) {
                crate::output::warn(&format!(
                    "warning: cannot write audit log {}: {e}",
                    self.path.display()
                ));
            }
        }
    }
}

/// Collects records in memory (tests, `zdk doctor`).
#[derive(Debug, Default)]
pub struct MemoryObserver {
    records: Mutex<Vec<RequestRecord>>,
}

impl MemoryObserver {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn records(&self) -> Vec<RequestRecord> {
        self.records.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl RequestObserver for MemoryObserver {
    fn observe(&self, record: &RequestRecord) {
        if let Ok(mut r) = self.records.lock() {
            r.push(record.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> RequestRecord {
        RequestRecord {
            ts: Utc::now(),
            profile: "p".into(),
            method: "GET".into(),
            path: "/api/v2/users/me".into(),
            status: Some(200),
            duration_ms: 12,
            rate: RateHeaders {
                limit: Some(700),
                remaining: Some(699),
                ..Default::default()
            },
            request_id: Some("r1".into()),
            bytes: 42,
            command: "auth whoami".into(),
            attempt: 1,
            error: None,
        }
    }

    #[test]
    fn audit_log_appends_ndjson_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("audit.ndjson");
        let obs = AuditLogObserver::new(&path);
        obs.observe(&record());
        obs.observe(&record());
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["method"], "GET");
        assert_eq!(v["status"], 200);
        assert_eq!(v["rate"]["limit"], 700);
        assert_eq!(v["request_id"], "r1");
        assert_eq!(v["bytes"], 42);
        assert_eq!(v["command"], "auth whoami");
        assert!(v.get("error").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mem = MemoryObserver::new();
        mem.observe(&record());
        assert_eq!(mem.records().len(), 1);
    }
}
