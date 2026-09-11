//! Zendesk OAuth scopes: the granular catalogue, presets, the command → scope map, and the
//! implication rules used for pre-flight checks (PRD §6.5).
//!
//! Implications: `write` ⊇ everything, `read` ⊇ every `*:read`, `X:write` ⊇ `X:read`.
//! An empty scope request is refused by construction — Zendesk treats it as full read+write.

use serde::Serialize;

use crate::{Result, ZdkError};

/// Read or write half of a scope family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
}

/// One entry of the scope catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ScopeDef {
    /// Exact scope string sent to Zendesk (`tickets:read`).
    pub name: &'static str,
    /// The resource family (`tickets`); `*` for the legacy global scopes.
    pub family: &'static str,
    pub access: Access,
    pub description: &'static str,
}

const fn read(name: &'static str, family: &'static str, description: &'static str) -> ScopeDef {
    ScopeDef {
        name,
        family,
        access: Access::Read,
        description,
    }
}

const fn write(name: &'static str, family: &'static str, description: &'static str) -> ScopeDef {
    ScopeDef {
        name,
        family,
        access: Access::Write,
        description,
    }
}

/// Every scope Zendesk accepts (52), legacy global scopes first.
pub static CATALOGUE: &[ScopeDef] = &[
    read("read", "*", "Legacy: read access to every resource"),
    write(
        "write",
        "*",
        "Legacy: read and write access to every resource",
    ),
    read(
        "tickets:read",
        "tickets",
        "Read tickets, comments, audits and metrics",
    ),
    write(
        "tickets:write",
        "tickets",
        "Create, update, merge and delete tickets and comments",
    ),
    read(
        "users:read",
        "users",
        "Read users, identities and related records",
    ),
    write(
        "users:write",
        "users",
        "Create, update and delete users and identities",
    ),
    read(
        "organizations:read",
        "organizations",
        "Read organizations and memberships",
    ),
    write(
        "organizations:write",
        "organizations",
        "Create, update and delete organizations",
    ),
    read("auditlogs:read", "auditlogs", "Read the account audit log"),
    read(
        "hc:read",
        "hc",
        "Read Help Center articles, sections, categories and community content",
    ),
    write(
        "hc:write",
        "hc",
        "Create, update and delete Help Center content",
    ),
    read("apps:read", "apps", "Read app installations and settings"),
    write("apps:write", "apps", "Install, update and remove apps"),
    read("automations:read", "automations", "Read automations"),
    write(
        "automations:write",
        "automations",
        "Create, update and delete automations",
    ),
    read("targets:read", "targets", "Read notification targets"),
    write(
        "targets:write",
        "targets",
        "Create, update and delete notification targets",
    ),
    read(
        "triggers:read",
        "triggers",
        "Read triggers and trigger categories",
    ),
    write(
        "triggers:write",
        "triggers",
        "Create, update and delete triggers",
    ),
    read("macros:read", "macros", "Read macros"),
    write("macros:write", "macros", "Create, update and delete macros"),
    read("requests:read", "requests", "Read end-user requests"),
    write(
        "requests:write",
        "requests",
        "Create and update end-user requests",
    ),
    read(
        "satisfaction_ratings:read",
        "satisfaction_ratings",
        "Read satisfaction ratings",
    ),
    write(
        "satisfaction_ratings:write",
        "satisfaction_ratings",
        "Create satisfaction ratings",
    ),
    read(
        "dynamic_content:read",
        "dynamic_content",
        "Read dynamic content items",
    ),
    write(
        "dynamic_content:write",
        "dynamic_content",
        "Create, update and delete dynamic content",
    ),
    read("themes:read", "themes", "Read Help Center themes"),
    write(
        "themes:write",
        "themes",
        "Import, update and publish Help Center themes",
    ),
    read(
        "zis:read",
        "zis",
        "Read Zendesk Integration Services flows and links",
    ),
    write(
        "zis:write",
        "zis",
        "Create and update Zendesk Integration Services resources",
    ),
    read("webhooks:read", "webhooks", "Read webhooks and invocations"),
    write(
        "webhooks:write",
        "webhooks",
        "Create, update, test and delete webhooks",
    ),
    read(
        "security:read",
        "security",
        "Read security settings and sessions",
    ),
    write(
        "any_channel:write",
        "any_channel",
        "Push messages through the Channel framework",
    ),
    write(
        "web_widget:write",
        "web_widget",
        "Update Web Widget settings",
    ),
    read(
        "account_settings:read",
        "account_settings",
        "Read account settings",
    ),
    write(
        "account_settings:write",
        "account_settings",
        "Update account settings",
    ),
    read("brands:read", "brands", "Read brands"),
    write("brands:write", "brands", "Create, update and delete brands"),
    read(
        "custom_objects:read",
        "custom_objects",
        "Read custom objects and records",
    ),
    write(
        "custom_objects:write",
        "custom_objects",
        "Create, update and delete custom objects and records",
    ),
    read(
        "deletion_schedules:read",
        "deletion_schedules",
        "Read data deletion schedules",
    ),
    write(
        "deletion_schedules:write",
        "deletion_schedules",
        "Create, update and delete deletion schedules",
    ),
    read("groups:read", "groups", "Read groups and group memberships"),
    write(
        "groups:write",
        "groups",
        "Create, update and delete groups and memberships",
    ),
    read("sla_policies:read", "sla_policies", "Read SLA policies"),
    write(
        "sla_policies:write",
        "sla_policies",
        "Create, update and delete SLA policies",
    ),
    read(
        "ticket_attachments:read",
        "ticket_attachments",
        "Download ticket attachments",
    ),
    write(
        "ticket_attachments:write",
        "ticket_attachments",
        "Upload, redact and delete attachments",
    ),
    read(
        "ticket_views:read",
        "ticket_views",
        "Read views and execute them",
    ),
    write(
        "ticket_views:write",
        "ticket_views",
        "Create, update and delete views",
    ),
];

/// A named bundle of scopes (`zdk auth login --preset agent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    pub scopes: &'static [&'static str],
}

/// Every `*:read` scope except the legacy global `read`.
const ALL_READ: &[&str] = &[
    "tickets:read",
    "users:read",
    "organizations:read",
    "auditlogs:read",
    "hc:read",
    "apps:read",
    "automations:read",
    "targets:read",
    "triggers:read",
    "macros:read",
    "requests:read",
    "satisfaction_ratings:read",
    "dynamic_content:read",
    "themes:read",
    "zis:read",
    "webhooks:read",
    "security:read",
    "account_settings:read",
    "brands:read",
    "custom_objects:read",
    "deletion_schedules:read",
    "groups:read",
    "sla_policies:read",
    "ticket_attachments:read",
    "ticket_views:read",
];

/// Presets from PRD §6.5 / plan A4.
pub static PRESETS: &[Preset] = &[
    Preset {
        name: "agent",
        description: "Day-to-day agent work: read (search) + tickets write, users/orgs/Help Center read",
        scopes: &[
            "read", // Zendesk search endpoints require the global read scope
            "tickets:read",
            "tickets:write",
            "users:read",
            "organizations:read",
            "hc:read",
        ],
    },
    Preset {
        name: "admin",
        description: "Agent preset plus business-rule and webhook administration",
        scopes: &[
            "read", // Zendesk search endpoints require the global read scope
            "tickets:read",
            "tickets:write",
            "users:read",
            "organizations:read",
            "hc:read",
            "triggers:write",
            "automations:write",
            "macros:write",
            "webhooks:write",
        ],
    },
    Preset {
        name: "readonly",
        description: "Every granular read scope",
        scopes: ALL_READ,
    },
    Preset {
        name: "exporter",
        description: "Bulk export: tickets, users, organizations and the audit log",
        scopes: &[
            "read", // Zendesk search endpoints require the global read scope
            "tickets:read",
            "users:read",
            "organizations:read",
            "auditlogs:read",
        ],
    },
];

/// Look a scope up by exact name.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static ScopeDef> {
    CATALOGUE.iter().find(|s| s.name == name)
}

/// Look a preset up by name (case-insensitive).
#[must_use]
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// The scope(s) each v0.1.0 command needs, keyed by command path; longest prefix wins.
/// Empty = no scope check (local commands, or the registry-driven `api` escape hatch).
static COMMAND_SCOPES: &[(&[&str], &[&str])] = &[
    // tickets
    (&["tickets", "list"], &["tickets:read"]),
    (&["tickets", "get"], &["tickets:read"]),
    (&["tickets", "show"], &["tickets:read"]),
    (&["tickets", "count"], &["tickets:read"]),
    (&["tickets", "recent"], &["tickets:read"]),
    (&["tickets", "create"], &["tickets:write"]),
    (&["tickets", "update"], &["tickets:write"]),
    (&["tickets", "reply"], &["tickets:write"]),
    (&["tickets", "note"], &["tickets:write"]),
    (&["tickets", "solve"], &["tickets:write"]),
    (&["tickets", "close"], &["tickets:write"]),
    (&["tickets", "reopen"], &["tickets:write"]),
    (&["tickets", "assign"], &["tickets:write"]),
    (&["tickets", "delete"], &["tickets:write"]),
    (&["tickets", "restore"], &["tickets:write"]),
    (&["tickets", "permanently-delete"], &["tickets:write"]),
    // comments
    (&["comments", "list"], &["tickets:read"]),
    (&["comments", "get"], &["tickets:read"]),
    (&["comments", "count"], &["tickets:read"]),
    (&["comments", "make-private"], &["tickets:write"]),
    (&["comments", "redact"], &["tickets:write"]),
    // users
    (&["users", "list"], &["users:read"]),
    (&["users", "search"], &["users:read"]),
    (&["users", "autocomplete"], &["users:read"]),
    (&["users", "get"], &["users:read"]),
    (&["users", "me"], &["users:read"]),
    (&["users", "related"], &["users:read"]),
    (&["users", "create"], &["users:write"]),
    (&["users", "create-or-update"], &["users:write"]),
    (&["users", "update"], &["users:write"]),
    (&["users", "delete"], &["users:write"]),
    // organizations
    (&["orgs", "list"], &["organizations:read"]),
    (&["orgs", "get"], &["organizations:read"]),
    (&["orgs", "search"], &["organizations:read"]),
    (&["orgs", "autocomplete"], &["organizations:read"]),
    (&["orgs", "count"], &["organizations:read"]),
    (&["orgs", "related"], &["organizations:read"]),
    (&["orgs", "tickets"], &["tickets:read"]),
    (&["orgs", "users"], &["users:read"]),
    (&["orgs", "create"], &["organizations:write"]),
    (&["orgs", "update"], &["organizations:write"]),
    (&["orgs", "delete"], &["organizations:write"]),
    // search (unified search has no granular scope)
    (&["search"], &["read"]),
    (&["search", "explain"], &[]),
    // auth
    (&["auth"], &[]),
    (&["auth", "whoami"], &["users:read"]),
    (&["auth", "test"], &["users:read"]),
    // escape hatch: the registry decides per operation
    (&["api"], &[]),
    // local
    (&["config"], &[]),
    (&["completions"], &[]),
    (&["man"], &[]),
    (&["doctor"], &[]),
    (&["version"], &[]),
];

/// Scopes required by a command path (`["tickets", "update"]`), longest prefix match;
/// `organizations` is accepted as an alias of `orgs`. Unknown commands need nothing.
#[must_use]
pub fn required_for(command_path: &[&str]) -> &'static [&'static str] {
    let mut best: Option<(&'static [&'static str], usize)> = None;
    for (path, scopes) in COMMAND_SCOPES {
        if path.len() > command_path.len() {
            continue;
        }
        let matches = path
            .iter()
            .zip(command_path)
            .all(|(want, have)| want == have || (*want == "orgs" && *have == "organizations"));
        if matches && best.is_none_or(|(_, n)| path.len() > n) {
            best = Some((scopes, path.len()));
        }
    }
    best.map_or(&[], |(scopes, _)| scopes)
}

/// Every command path in the static map (for tests and `auth scopes check --list`).
#[must_use]
pub fn known_command_paths() -> Vec<&'static [&'static str]> {
    COMMAND_SCOPES.iter().map(|(p, _)| *p).collect()
}

/// Does a single granted scope cover `required`, applying the implication rules?
fn implies(granted: &str, required: &str) -> bool {
    if granted == required || granted == "write" {
        return true;
    }
    let (req_family, req_access) = split(required);
    if granted == "read" {
        return req_access == Some("read");
    }
    let (g_family, g_access) = split(granted);
    g_family == req_family && g_access == Some("write") && req_access == Some("read")
}

fn split(scope: &str) -> (&str, Option<&str>) {
    match scope.split_once(':') {
        Some((f, a)) => (f, Some(a)),
        None => (scope, None),
    }
}

/// Is one scope covered by the granted set?
pub fn covers<G: AsRef<str>>(granted: &[G], required: &str) -> bool {
    granted.iter().any(|g| implies(g.as_ref(), required))
}

/// Are all `required` scopes covered by `granted`?
pub fn satisfies<G: AsRef<str>, R: AsRef<str>>(granted: &[G], required: &[R]) -> bool {
    required.iter().all(|r| covers(granted, r.as_ref()))
}

/// Fail before the HTTP call when a required scope is missing (exit 4, `SCOPE_MISSING`).
/// An empty `granted` set means "unknown" and passes — the server is the authority then.
pub fn preflight<G: AsRef<str>, R: AsRef<str>>(granted: &[G], required: &[R]) -> Result<()> {
    if granted.is_empty() {
        return Ok(());
    }
    for r in required {
        let r = r.as_ref();
        if !covers(granted, r) {
            return Err(ZdkError::Forbidden {
                message: format!(
                    "this command requires the '{r}' scope, which the current token does not grant"
                ),
                required_scope: Some(r.to_string()),
                granted: granted.iter().map(|g| g.as_ref().to_string()).collect(),
                request_id: None,
            });
        }
    }
    Ok(())
}

/// Reject an empty request (would grant full read+write) and unknown names (exit 2).
pub fn validate_requested<S: AsRef<str>>(scopes: &[S]) -> Result<()> {
    if scopes.is_empty() {
        return Err(ZdkError::Usage(
            "no scopes requested: an empty scope would grant full read+write. Pass --scopes \
             (e.g. tickets:read,users:read), --preset agent|admin|readonly|exporter, or set \
             the profile's scopes"
                .into(),
        ));
    }
    let unknown: Vec<&str> = scopes
        .iter()
        .map(AsRef::as_ref)
        .filter(|s| lookup(s).is_none())
        .collect();
    if !unknown.is_empty() {
        return Err(ZdkError::Usage(format!(
            "unknown scope(s): {}. Run `zdk auth scopes list` for the catalogue",
            unknown.join(", ")
        )));
    }
    Ok(())
}

/// Split `"a,b c"` on commas and whitespace, trimming and de-duplicating in order.
#[must_use]
pub fn parse_list(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in input.split([',', ' ', '\t', '\n']) {
        let p = part.trim();
        if !p.is_empty() && !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// Expand `--preset` names and explicit scopes into one de-duplicated list.
pub fn expand(presets: &[String], explicit: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for name in presets {
        let p = preset(name).ok_or_else(|| {
            ZdkError::Usage(format!(
                "unknown preset '{name}': expected one of {}",
                PRESETS
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        for s in p.scopes {
            if !out.iter().any(|o| o == s) {
                out.push((*s).to_string());
            }
        }
    }
    for s in explicit {
        if !out.iter().any(|o| o == s) {
            out.push(s.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn catalogue_has_52_unique_well_formed_entries() {
        assert_eq!(CATALOGUE.len(), 52);
        let mut names: Vec<&str> = CATALOGUE.iter().map(|s| s.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 52, "duplicate scope names");
        for def in CATALOGUE {
            match def.family {
                "*" => assert!(matches!(def.name, "read" | "write")),
                fam => {
                    let (f, a) = split(def.name);
                    assert_eq!(f, fam, "{}", def.name);
                    let expected = match def.access {
                        Access::Read => "read",
                        Access::Write => "write",
                    };
                    assert_eq!(a, Some(expected), "{}", def.name);
                }
            }
            assert!(!def.description.is_empty());
        }
    }

    #[test]
    fn presets_are_subsets_of_the_catalogue() {
        for p in PRESETS {
            assert!(!p.scopes.is_empty(), "{}", p.name);
            for sc in p.scopes {
                assert!(lookup(sc).is_some(), "{}: {sc}", p.name);
            }
            assert!(preset(p.name).is_some());
        }
        let every_read: Vec<&str> = CATALOGUE
            .iter()
            .filter(|d| d.access == Access::Read && d.family != "*")
            .map(|d| d.name)
            .collect();
        assert_eq!(preset("readonly").unwrap().scopes, every_read.as_slice());
        assert!(!preset("readonly").unwrap().scopes.contains(&"read"));
        assert!(preset("AGENT").is_some(), "case-insensitive");
        assert!(preset("root").is_none());
    }

    #[test]
    fn every_required_for_entry_names_catalogue_scopes() {
        for path in known_command_paths() {
            for sc in required_for(path) {
                assert!(lookup(sc).is_some(), "{path:?}: {sc}");
            }
        }
        assert_eq!(required_for(&["tickets", "list"]), ["tickets:read"]);
        assert_eq!(required_for(&["tickets", "update"]), ["tickets:write"]);
        assert_eq!(required_for(&["search"]), ["read"]);
        assert_eq!(required_for(&["search", "count"]), ["read"], "prefix match");
        assert!(required_for(&["search", "explain"]).is_empty());
        assert!(required_for(&["api", "GET", "/api/v2/tickets"]).is_empty());
        assert_eq!(required_for(&["users", "me"]), ["users:read"]);
        assert_eq!(required_for(&["auth", "whoami"]), ["users:read"]);
        assert!(required_for(&["auth", "login"]).is_empty());
        assert_eq!(
            required_for(&["organizations", "list"]),
            ["organizations:read"]
        );
        assert!(required_for(&["nope"]).is_empty());
        assert!(required_for(&[]).is_empty());
    }

    #[test]
    fn implication_rules() {
        assert!(satisfies(
            &s(&["write"]),
            &["tickets:write", "hc:read", "read"]
        ));
        assert!(satisfies(&s(&["read"]), &["tickets:read", "users:read"]));
        assert!(!satisfies(&s(&["read"]), &["tickets:write"]));
        assert!(satisfies(&s(&["tickets:write"]), &["tickets:read"]));
        assert!(!satisfies(&s(&["tickets:write"]), &["users:read"]));
        assert!(!satisfies(&s(&["tickets:read"]), &["tickets:write"]));
        assert!(satisfies(&s(&["tickets:read"]), &["tickets:read"]));
        assert!(
            !satisfies(&s(&["tickets:read"]), &["read"]),
            "granular never implies global"
        );
        assert!(satisfies(&s(&["hc:read"]), &Vec::<String>::new()));
    }

    #[test]
    fn preflight_reports_the_first_missing_scope() {
        assert!(preflight(&s(&["tickets:read"]), &["tickets:read"]).is_ok());
        assert!(
            preflight(&Vec::<String>::new(), &["tickets:write"]).is_ok(),
            "unknown grants pass"
        );
        let err = preflight(&s(&["tickets:read"]), &["tickets:write"]).unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert_eq!(err.error_code(), "SCOPE_MISSING");
        match err {
            ZdkError::Forbidden {
                required_scope,
                granted,
                ..
            } => {
                assert_eq!(required_scope.as_deref(), Some("tickets:write"));
                assert_eq!(granted, vec!["tickets:read"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn validate_requested_rejects_empty_and_unknown() {
        let err = validate_requested::<String>(&[]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("full read+write"));
        let err = validate_requested(&s(&["tickets:read", "ticket:read", "foo"])).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("ticket:read, foo"), "{err}");
        assert!(validate_requested(&s(&["tickets:read", "write"])).is_ok());
    }

    #[test]
    fn parse_list_and_expand() {
        assert_eq!(parse_list("a,b c,, d\ta"), vec!["a", "b", "c", "d"]);
        assert!(parse_list("  ").is_empty());
        let out = expand(&s(&["exporter"]), &s(&["hc:read", "tickets:read"])).unwrap();
        assert_eq!(
            out,
            vec![
                "tickets:read",
                "users:read",
                "organizations:read",
                "auditlogs:read",
                "hc:read"
            ]
        );
        assert_eq!(expand(&s(&["nope"]), &[]).unwrap_err().exit_code(), 2);
    }
}
