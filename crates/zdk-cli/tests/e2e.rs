//! Live end-to-end run against a real Zendesk sandbox (plan A11). Self-skipping: it does
//! nothing unless `ZDK_E2E_SUBDOMAIN`, `ZDK_E2E_CLIENT_ID` and `ZDK_E2E_CLIENT_SECRET` are all
//! set, so the default suite never needs credentials. Run it alone:
//!
//! ```text
//! ZDK_E2E_SUBDOMAIN=… ZDK_E2E_CLIENT_ID=… ZDK_E2E_CLIENT_SECRET=… \
//!   cargo test -p zendesk-cli --test e2e -- --test-threads 1 --nocapture
//! ```
//!
//! Flow: client-credentials login → whoami → ticket create / note / update / solve / delete /
//! restore / permanently-delete → user and organization lifecycle → `search` vs
//! `search export` consistency on a uniquely tagged ticket. Everything it creates carries the
//! tag `zdk-e2e` and is removed at the end (best effort: a failing assertion leaves the rest).

// The gate variables are read here on purpose; nothing else in the workspace reads the
// environment directly.
#![allow(clippy::disallowed_methods)]

mod common;

use std::time::Duration;

use assert_cmd::Command;
use common::{Harness, json};

const TAG: &str = "zdk-e2e";
const SCOPES: &str =
    "tickets:read,tickets:write,users:read,users:write,organizations:read,organizations:write,read";

struct Live {
    h: Harness,
    subdomain: String,
}

impl Live {
    fn from_env() -> Option<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let subdomain = get("ZDK_E2E_SUBDOMAIN")?;
        get("ZDK_E2E_CLIENT_ID")?;
        get("ZDK_E2E_CLIENT_SECRET")?;
        Some(Self {
            h: Harness::new(),
            subdomain,
        })
    }

    /// `zdk` with the encrypted file store (a login has to survive across processes) and the
    /// sandbox subdomain. JSON output unless a test passes `-o`.
    fn zdk(&self) -> Command {
        let mut cmd = self.h.zdk();
        cmd.env("ZENDESK_CREDENTIAL_STORE", "file")
            .env("ZENDESK_CREDENTIALS_PASSPHRASE", "zdk-e2e-passphrase")
            .env("ZENDESK_SUBDOMAIN", &self.subdomain)
            .env("ZENDESK_OUTPUT", "json")
            .timeout(Duration::from_secs(120));
        cmd
    }

    fn run(&self, args: &[&str]) -> serde_json::Value {
        let out = self.zdk().args(args).output().expect("run zdk");
        assert!(
            out.status.success(),
            "zdk {} failed ({}):\n{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        if out.stdout.iter().all(u8::is_ascii_whitespace) {
            return serde_json::Value::Null;
        }
        json(&out.stdout)
    }

    fn run_ok(&self, args: &[&str]) {
        let _ = self.run(args);
    }
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{prefix}-{nanos}")
}

#[test]
fn e2e_lifecycle() {
    let Some(live) = Live::from_env() else {
        zdk_core::output::error_line(
            "skipped: set ZDK_E2E_SUBDOMAIN, ZDK_E2E_CLIENT_ID and ZDK_E2E_CLIENT_SECRET to run the live suite",
        );
        return;
    };
    let client_id = std::env::var("ZDK_E2E_CLIENT_ID").expect("gated above");
    let client_secret = std::env::var("ZDK_E2E_CLIENT_SECRET").expect("gated above");

    // --- login + whoami --------------------------------------------------------------------
    live.run_ok(&[
        "auth",
        "login",
        "--client-credentials",
        "--subdomain",
        &live.subdomain,
        "--client-id",
        &client_id,
        "--client-secret",
        &client_secret,
        "--scopes",
        SCOPES,
    ]);
    let me = live.run(&["auth", "whoami"]);
    let me_id = me["id"].as_u64().expect("whoami returns the user id");
    let me2 = live.run(&["users", "me"]);
    assert_eq!(me2["id"].as_u64(), Some(me_id));

    // --- ticket lifecycle ------------------------------------------------------------------
    let marker = unique("zdk-e2e");
    let ticket = live.run(&[
        "tickets",
        "create",
        "--subject",
        &format!("zdk e2e {marker}"),
        "--comment",
        "created by the zendesk-cli e2e suite",
        "--requester",
        "me",
        "--priority",
        "low",
        "--tag",
        TAG,
        "--tag",
        &marker,
    ]);
    let ticket_id = ticket["id"].as_u64().expect("created ticket id");
    let id = ticket_id.to_string();
    assert_eq!(ticket["priority"], "low");

    let noted = live.run(&["tickets", "note", &id, "--body", "internal note from e2e"]);
    assert_eq!(noted["id"].as_u64(), Some(ticket_id));
    let comments = live.run(&["comments", "list", &id]);
    let comments = comments.as_array().expect("comments array");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[1]["public"], false);
    let count = live.run(&["comments", "count", &id]);
    assert_eq!(count["count"], 2);

    let updated = live.run(&[
        "tickets",
        "update",
        &id,
        "--priority",
        "normal",
        "--add-tag",
        "e2e-updated",
    ]);
    assert_eq!(updated["priority"], "normal");
    assert!(
        updated["tags"]
            .as_array()
            .is_some_and(|t| t.iter().any(|x| x == "e2e-updated")),
        "{updated}"
    );
    let shown = live.run(&["tickets", "get", &id, "--with-comments"]);
    assert_eq!(shown["comments"].as_array().map(Vec::len), Some(2));
    let assigned = live.run(&["tickets", "assign", &id, "--to", "me"]);
    assert_eq!(assigned["assignee_id"].as_u64(), Some(me_id));
    let solved = live.run(&["tickets", "solve", &id, "--body", "resolved by e2e"]);
    assert_eq!(solved["status"], "solved");

    // --- search vs export consistency (the index lags; poll briefly) ---------------------
    let query = format!("type:ticket tags:{marker}");
    let mut found = 0;
    for _ in 0..12 {
        let c = live.run(&["search", "count", &query]);
        found = c["count"].as_u64().unwrap_or(0);
        if found >= 1 {
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    if found >= 1 {
        let listed = live.run(&["tickets", "list", "--tag", &marker]);
        let via_search: Vec<u64> = listed
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t["id"].as_u64())
            .collect();
        let exported = live.run(&["search", "export", &query, "--filter-type", "ticket"]);
        let via_export: Vec<u64> = exported
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t["id"].as_u64())
            .collect();
        assert_eq!(via_search, vec![ticket_id]);
        assert_eq!(via_export, vec![ticket_id]);
        let explained = live.run(&["search", "explain", "--tag", &marker]);
        assert_eq!(explained["query"], query);
    } else {
        zdk_core::output::error_line(
            "note: search index did not catch up within 60s; skipping the search/export comparison",
        );
    }

    // --- delete / restore / permanently delete --------------------------------------------
    live.run_ok(&["tickets", "delete", &id, "--yes"]);
    live.run_ok(&["tickets", "restore", &id]);
    let restored = live.run(&["tickets", "get", &id]);
    assert_eq!(restored["id"].as_u64(), Some(ticket_id));
    live.run_ok(&["tickets", "delete", &id, "--yes"]);
    live.run_ok(&["tickets", "permanently-delete", &id, "--yes"]);
    let gone = live
        .zdk()
        .args(["tickets", "get", &id])
        .output()
        .expect("run");
    assert_eq!(gone.status.code(), Some(5), "a purged ticket is not found");

    // --- user lifecycle ------------------------------------------------------------------
    let email = format!("{}@example.com", unique("zdk-e2e-user"));
    let user = live.run(&[
        "users",
        "create",
        "--name",
        "ZDK E2E User",
        "--email",
        &email,
        "--role",
        "end-user",
        "--verified",
    ]);
    let user_id = user["id"].as_u64().expect("user id").to_string();
    let by_email = live.run(&["users", "get", &email]);
    assert_eq!(by_email["id"], user["id"]);
    let renamed = live.run(&["users", "update", &user_id, "--name", "ZDK E2E Renamed"]);
    assert_eq!(renamed["name"], "ZDK E2E Renamed");
    live.run_ok(&["users", "delete", &user_id, "--yes"]);

    // --- organization lifecycle ----------------------------------------------------------
    let org_name = unique("ZDK E2E Org");
    let org = live.run(&["orgs", "create", "--name", &org_name, "--tag", TAG]);
    let org_id = org["id"].as_u64().expect("org id").to_string();
    let found = live.run(&["orgs", "search", "--name", &org_name]);
    assert_eq!(found[0]["id"], org["id"]);
    let tagged = live.run(&["orgs", "update", &org_id, "--add-tag", "e2e-updated"]);
    assert!(
        tagged["tags"]
            .as_array()
            .is_some_and(|t| t.iter().any(|x| x == "e2e-updated")),
        "{tagged}"
    );
    live.run_ok(&["orgs", "delete", &org_id, "--yes"]);

    live.run_ok(&["auth", "logout"]);
}
