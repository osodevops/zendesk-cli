//! The ticket filter flags shared by `tickets list`, `tickets count` and `search explain`,
//! and their conversion into a [`TicketQuery`] (validated values, human times resolved).

use chrono::{DateTime, Utc};
use clap::{ArgAction, Args};
use zdk_core::api::curated::search::TicketQuery;
use zdk_core::api::curated::tickets::{PRIORITIES, STATUSES, TYPES};
use zdk_core::util::time::parse_human_time;
use zdk_core::{Result, ZdkError};

use super::field::parse_ticket_custom_field;
use super::ids::UserRef;

/// Any of these turns the list into a compiled search query.
#[derive(Debug, Args, Clone, Default)]
#[command(next_help_heading = "Filters (any of these compiles to a search query)")]
pub struct TicketFilters {
    /// Status (comma-separated or repeated): new, open, pending, hold, solved, closed
    #[arg(long, value_name = "STATUS", value_delimiter = ',', action = ArgAction::Append)]
    pub status: Vec<String>,

    /// Assignee: me, a user id, or an email
    #[arg(long, value_name = "ME|ID|EMAIL")]
    pub assignee: Option<String>,

    /// Requester: me, a user id, or an email
    #[arg(long, value_name = "ME|ID|EMAIL")]
    pub requester: Option<String>,

    /// Group id
    #[arg(long, value_name = "ID")]
    pub group_id: Option<u64>,

    /// Organization id
    #[arg(long, value_name = "ID")]
    pub organization_id: Option<u64>,

    /// Brand id
    #[arg(long, value_name = "ID")]
    pub brand_id: Option<u64>,

    /// Ticket form id
    #[arg(long, value_name = "ID")]
    pub form_id: Option<u64>,

    /// Tag (repeatable; every tag must be present)
    #[arg(long, value_name = "TAG", action = ArgAction::Append)]
    pub tag: Vec<String>,

    /// Priority: low, normal, high, urgent
    #[arg(long, value_name = "PRIORITY")]
    pub priority: Option<String>,

    /// Type: problem, incident, question, task
    #[arg(long = "type", value_name = "TYPE")]
    pub ticket_type: Option<String>,

    /// Created after (RFC 3339, YYYY-MM-DD, 24h, "2 hours ago", yesterday)
    #[arg(long, value_name = "TIME")]
    pub created_after: Option<String>,

    /// Created before
    #[arg(long, value_name = "TIME")]
    pub created_before: Option<String>,

    /// Updated after
    #[arg(long, value_name = "TIME")]
    pub updated_after: Option<String>,

    /// Updated before
    #[arg(long, value_name = "TIME")]
    pub updated_before: Option<String>,

    /// Custom field filter <field id>=<value> (repeatable)
    #[arg(long, value_name = "ID=VALUE", action = ArgAction::Append)]
    pub custom_field: Vec<String>,

    /// No assignee (assignee:none)
    #[arg(long)]
    pub unassigned: bool,

    /// Created earlier than this long ago, e.g. 24h, 7d (created<…)
    #[arg(long, value_name = "DURATION")]
    pub older_than: Option<String>,

    /// Waiting on the customer (status:pending)
    #[arg(long)]
    pub awaiting_customer: bool,
}

impl TicketFilters {
    /// Whether any filter was given.
    pub(crate) fn is_active(&self) -> bool {
        !self.status.is_empty()
            || self.assignee.is_some()
            || self.requester.is_some()
            || self.group_id.is_some()
            || self.organization_id.is_some()
            || self.brand_id.is_some()
            || self.form_id.is_some()
            || !self.tag.is_empty()
            || self.priority.is_some()
            || self.ticket_type.is_some()
            || self.created_after.is_some()
            || self.created_before.is_some()
            || self.updated_after.is_some()
            || self.updated_before.is_some()
            || !self.custom_field.is_empty()
            || self.unassigned
            || self.older_than.is_some()
            || self.awaiting_customer
    }

    /// Validate values and resolve times relative to `now`.
    pub(crate) fn to_query(&self, now: DateTime<Utc>) -> Result<TicketQuery> {
        let mut q = TicketQuery::default();
        for s in &self.status {
            q.status.push(one_of("--status", s, STATUSES)?);
        }
        if let Some(a) = &self.assignee {
            q.assignee = Some(user_term(a)?);
        }
        if let Some(r) = &self.requester {
            q.requester = Some(user_term(r)?);
        }
        q.group_id = self.group_id;
        q.organization_id = self.organization_id;
        q.brand_id = self.brand_id;
        q.form_id = self.form_id;
        q.tags = self
            .tag
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if let Some(p) = &self.priority {
            q.priority = Some(one_of("--priority", p, PRIORITIES)?);
        }
        if let Some(t) = &self.ticket_type {
            q.ticket_type = Some(one_of("--type", t, TYPES)?);
        }
        q.created_after = time("--created-after", self.created_after.as_deref(), now)?;
        q.created_before = time("--created-before", self.created_before.as_deref(), now)?;
        q.updated_after = time("--updated-after", self.updated_after.as_deref(), now)?;
        q.updated_before = time("--updated-before", self.updated_before.as_deref(), now)?;
        for cf in &self.custom_field {
            let (id, value) = parse_ticket_custom_field(cf)?;
            let text = match value {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            q.custom_fields.push((id, text));
        }
        q.unassigned = self.unassigned;
        if let Some(older) = time("--older-than", self.older_than.as_deref(), now)? {
            q.older_than(older);
        }
        if self.awaiting_customer {
            q.awaiting_customer();
        }
        Ok(q)
    }
}

fn one_of(flag: &str, value: &str, allowed: &[&str]) -> Result<String> {
    let v = value.trim().to_ascii_lowercase();
    if allowed.contains(&v.as_str()) {
        Ok(v)
    } else {
        Err(ZdkError::Usage(format!(
            "{flag} '{value}' is not one of {}",
            allowed.join(", ")
        )))
    }
}

/// `me` stays `me`, ids stay ids, emails pass through — Zendesk resolves them in the query.
fn user_term(value: &str) -> Result<String> {
    Ok(match UserRef::parse(value)? {
        UserRef::Me => "me".into(),
        UserRef::Id(id) => id.to_string(),
        UserRef::Email(e) => e,
    })
}

fn time(flag: &str, value: Option<&str>, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|v| parse_human_time(v, now).map_err(|e| ZdkError::Usage(format!("{flag}: {e}"))))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Probe {
        #[command(flatten)]
        filters: TicketFilters,
    }

    fn parse(args: &[&str]) -> TicketFilters {
        let mut cmdline = vec!["probe"];
        cmdline.extend_from_slice(args);
        Probe::try_parse_from(cmdline).expect("parses").filters
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 11, 12, 0, 0).unwrap()
    }

    #[test]
    fn no_flags_is_inactive_and_each_flag_activates() {
        assert!(!parse(&[]).is_active());
        for flags in [
            vec!["--status", "open"],
            vec!["--assignee", "me"],
            vec!["--requester", "1"],
            vec!["--group-id", "1"],
            vec!["--organization-id", "1"],
            vec!["--brand-id", "1"],
            vec!["--form-id", "1"],
            vec!["--tag", "x"],
            vec!["--priority", "high"],
            vec!["--type", "task"],
            vec!["--created-after", "24h"],
            vec!["--created-before", "24h"],
            vec!["--updated-after", "24h"],
            vec!["--updated-before", "24h"],
            vec!["--custom-field", "1=x"],
            vec!["--unassigned"],
            vec!["--older-than", "24h"],
            vec!["--awaiting-customer"],
        ] {
            assert!(parse(&flags).is_active(), "{flags:?}");
        }
    }

    #[test]
    fn every_flag_compiles_to_its_term() {
        let f = parse(&[
            "--status",
            "open,Pending",
            "--assignee",
            "42",
            "--requester",
            "grace@example.com",
            "--group-id",
            "501",
            "--organization-id",
            "1001",
            "--brand-id",
            "9001",
            "--form-id",
            "7001",
            "--tag",
            "urgent",
            "--tag",
            "vip",
            "--priority",
            "HIGH",
            "--type",
            "incident",
            "--created-after",
            "2026-09-01",
            "--created-before",
            "2026-09-10",
            "--updated-after",
            "2 hours ago",
            "--updated-before",
            "now",
            "--custom-field",
            "360000001=production",
            "--custom-field",
            "7:=true",
        ]);
        let q = f.to_query(now()).unwrap();
        assert_eq!(
            q.compile(),
            "type:ticket status:open status:pending assignee:42 requester:grace@example.com \
             group:501 organization:1001 brand:9001 ticket_form:7001 tags:urgent tags:vip \
             priority:high ticket_type:incident created>2026-09-01T00:00:00Z \
             created<2026-09-10T00:00:00Z updated>2026-09-11T10:00:00Z \
             updated<2026-09-11T12:00:00Z custom_field_360000001:production custom_field_7:true"
        );
    }

    #[test]
    fn composite_flags() {
        let q = parse(&["--unassigned", "--older-than", "24h", "--awaiting-customer"])
            .to_query(now())
            .unwrap();
        assert_eq!(
            q.compile(),
            "type:ticket status:pending assignee:none created<2026-09-10T12:00:00Z"
        );
        let q = parse(&[
            "--assignee",
            "me",
            "--older-than",
            "7d",
            "--created-before",
            "1d",
        ])
        .to_query(now())
        .unwrap();
        assert_eq!(
            q.compile(),
            "type:ticket assignee:me created<2026-09-04T12:00:00Z",
            "the earlier bound wins"
        );
    }

    #[test]
    fn invalid_values_are_usage_errors() {
        for flags in [
            vec!["--status", "done"],
            vec!["--priority", "critical"],
            vec!["--type", "bug"],
            vec!["--assignee", "Ada Lovelace"],
            vec!["--created-after", "last tuesday-ish"],
            vec!["--custom-field", "Environment=prod"],
            vec!["--older-than", "soon"],
        ] {
            let err = parse(&flags).to_query(now()).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{flags:?}");
        }
    }
}
