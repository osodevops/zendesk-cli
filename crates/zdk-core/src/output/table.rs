//! Human tables (comfy-table): per-resource column presets, width-aware layout, local-time
//! timestamps, right-aligned numbers, and status/priority colour when colour is on.

use comfy_table::{
    Attribute, Cell, CellAlignment, Color, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED,
};
use serde_json::Value;

use super::project::{DotPath, get};
use super::{OutputSink, RenderOptions, write_stdout};
use crate::Result;
use crate::util::time::{format_local, looks_like_timestamp, parse_timestamp};

/// Longest string shown in a `subject`/`body`/`description` column before `…`.
pub const TRUNCATE_AT: usize = 80;
/// Columns shown for a resource without a preset.
pub const MAX_AUTO_COLUMNS: usize = 8;
/// Width used when stdout is not a terminal and none was given.
pub const NON_TTY_WIDTH: u16 = 120;

/// One table column: a header and the dot paths tried in order (first hit wins).
#[derive(Debug, Clone, Copy)]
pub struct Column {
    pub header: &'static str,
    pub paths: &'static [&'static str],
}

/// The default columns for a resource.
#[derive(Debug, Clone, Copy)]
pub struct TablePreset {
    pub resource: &'static str,
    pub columns: &'static [Column],
}

/// Built-in presets (plan A8).
pub mod presets {
    use super::{Column, TablePreset};

    const fn col(header: &'static str, paths: &'static [&'static str]) -> Column {
        Column { header, paths }
    }

    pub static TICKETS: TablePreset = TablePreset {
        resource: "tickets",
        columns: &[
            col("ID", &["id"]),
            col("STATUS", &["status"]),
            col("PRIORITY", &["priority"]),
            col("SUBJECT", &["subject"]),
            col(
                "REQUESTER",
                &["requester.name", "requester.email", "requester_id"],
            ),
            col(
                "ASSIGNEE",
                &["assignee.name", "assignee.email", "assignee_id"],
            ),
            col("UPDATED", &["updated_at"]),
        ],
    };

    pub static USERS: TablePreset = TablePreset {
        resource: "users",
        columns: &[
            col("ID", &["id"]),
            col("NAME", &["name"]),
            col("EMAIL", &["email"]),
            col("ROLE", &["role"]),
            col("ACTIVE", &["active"]),
            col("UPDATED", &["updated_at"]),
        ],
    };

    pub static ORGANIZATIONS: TablePreset = TablePreset {
        resource: "organizations",
        columns: &[
            col("ID", &["id"]),
            col("NAME", &["name"]),
            col("DOMAINS", &["domain_names"]),
            col("TAGS", &["tags"]),
            col("UPDATED", &["updated_at"]),
        ],
    };

    pub static COMMENTS: TablePreset = TablePreset {
        resource: "comments",
        columns: &[
            col("ID", &["id"]),
            col("AUTHOR", &["author.name", "author.email", "author_id"]),
            col("PUBLIC", &["public"]),
            col("CREATED", &["created_at"]),
            col("BODY", &["plain_body", "body"]),
        ],
    };

    pub static SEARCH: TablePreset = TablePreset {
        resource: "search",
        columns: &[
            col("ID", &["id"]),
            col("TYPE", &["result_type"]),
            col("TITLE", &["subject", "title", "name"]),
            col("STATUS", &["status"]),
            col("UPDATED", &["updated_at"]),
        ],
    };

    pub static OPS: TablePreset = TablePreset {
        resource: "ops",
        columns: &[
            col("ID", &["id"]),
            col("METHOD", &["method"]),
            col("PATH", &["path"]),
            col("PAGINATION", &["pagination"]),
            col("SCOPE", &["scope"]),
        ],
    };

    pub static PROFILES: TablePreset = TablePreset {
        resource: "profiles",
        columns: &[
            col("PROFILE", &["profile"]),
            col("SUBDOMAIN", &["subdomain"]),
            col("AUTH", &["auth"]),
            col("PLAN", &["plan"]),
        ],
    };

    static ALL: [&TablePreset; 7] = [
        &TICKETS,
        &USERS,
        &ORGANIZATIONS,
        &COMMENTS,
        &SEARCH,
        &OPS,
        &PROFILES,
    ];

    /// Look a preset up by resource name (`orgs` is an alias for `organizations`).
    #[must_use]
    pub fn for_resource(name: &str) -> Option<&'static TablePreset> {
        let name = match name {
            "orgs" | "organization" => "organizations",
            "ticket" => "tickets",
            "user" => "users",
            "comment" => "comments",
            "profile" => "profiles",
            "operations" | "op" => "ops",
            other => other,
        };
        ALL.iter().copied().find(|p| p.resource == name)
    }

    /// Every preset.
    #[must_use]
    pub fn all() -> &'static [&'static TablePreset] {
        &ALL
    }
}

/// Buffers rows, then renders one table.
#[derive(Debug)]
pub struct TableSink {
    opts: RenderOptions,
    rows: Vec<Value>,
}

impl TableSink {
    #[must_use]
    pub fn new(opts: &RenderOptions) -> Self {
        Self {
            opts: opts.clone(),
            rows: Vec::new(),
        }
    }
}

impl OutputSink for TableSink {
    fn begin(&mut self) -> Result<()> {
        Ok(())
    }

    fn item(&mut self, value: Value) -> Result<()> {
        self.rows.push(value);
        Ok(())
    }

    fn end(&mut self) -> Result<()> {
        let rows = std::mem::take(&mut self.rows);
        write_stdout(render_rows(&rows, &self.opts).as_bytes());
        write_stdout(b"\n");
        Ok(())
    }
}

/// A resolved column: header plus candidate paths.
#[derive(Debug, Clone)]
struct ResolvedColumn {
    header: String,
    paths: Vec<DotPath>,
}

/// Choose columns: `--fields` → preset for `resource` → union of top-level keys (max 8).
fn columns_for(rows: &[Value], opts: &RenderOptions) -> Vec<ResolvedColumn> {
    if !opts.fields.is_empty() {
        return opts
            .fields
            .iter()
            .flat_map(|f| {
                f.split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .map(|f| ResolvedColumn {
                header: f.to_ascii_uppercase(),
                paths: vec![DotPath::parse(&f)],
            })
            .collect();
    }
    if let Some(preset) = opts.resource.as_deref().and_then(presets::for_resource) {
        return preset
            .columns
            .iter()
            .map(|c| ResolvedColumn {
                header: c.header.to_string(),
                paths: c.paths.iter().map(|p| DotPath::parse(p)).collect(),
            })
            .collect();
    }
    let mut keys: Vec<String> = Vec::new();
    for row in rows {
        if let Value::Object(map) = row {
            for k in map.keys() {
                if !keys.iter().any(|x| x == k) {
                    keys.push(k.clone());
                }
            }
        }
    }
    keys.truncate(MAX_AUTO_COLUMNS);
    if keys.is_empty() {
        keys.push("value".into());
    }
    keys.into_iter()
        .map(|k| ResolvedColumn {
            header: k.to_ascii_uppercase(),
            paths: vec![DotPath::parse(&k)],
        })
        .collect()
}

/// Render a list of records.
#[must_use]
pub fn render_rows(rows: &[Value], opts: &RenderOptions) -> String {
    let columns = columns_for(rows, opts);
    let mut table = new_table(opts);
    table.set_header(columns.iter().map(|c| header_cell(&c.header, opts.color)));
    for row in rows {
        let cells = columns.iter().map(|c| {
            let value = match row {
                Value::Object(_) => c.paths.iter().find_map(|p| get(row, p)),
                other if c.header == "VALUE" => Some(other),
                _ => None,
            };
            data_cell(value, &c.header, opts.color)
        });
        table.add_row(cells);
    }
    table.to_string()
}

/// Render one record as a FIELD / VALUE table (nested values as compact JSON, untruncated).
pub fn render_single(value: &Value, opts: &RenderOptions) -> Result<()> {
    let mut table = new_table(opts);
    table.set_header([
        header_cell("FIELD", opts.color),
        header_cell("VALUE", opts.color),
    ]);
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                table.add_row([Cell::new(k), data_cell_untruncated(Some(v), k, opts.color)]);
            }
        }
        other => {
            table.add_row([
                Cell::new("value"),
                data_cell_untruncated(Some(other), "value", opts.color),
            ]);
        }
    }
    write_stdout(table.to_string().as_bytes());
    write_stdout(b"\n");
    Ok(())
}

fn new_table(opts: &RenderOptions) -> Table {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    if !opts.stdout_is_tty {
        table.force_no_tty();
    }
    match opts.width {
        Some(w) => {
            table.set_width(w);
        }
        None if !opts.stdout_is_tty => {
            table.set_width(NON_TTY_WIDTH);
        }
        None => {}
    }
    if opts.color {
        table.enforce_styling();
    }
    table
}

fn header_cell(text: &str, color: bool) -> Cell {
    let cell = Cell::new(text);
    if color {
        cell.add_attribute(Attribute::Bold)
    } else {
        cell
    }
}

fn data_cell(value: Option<&Value>, header: &str, color: bool) -> Cell {
    let text = cell_text(value, header, true);
    style(Cell::new(text.clone()), value, header, &text, color)
}

fn data_cell_untruncated(value: Option<&Value>, header: &str, color: bool) -> Cell {
    let text = cell_text(value, header, false);
    style(Cell::new(text.clone()), value, header, &text, color)
}

fn style(cell: Cell, value: Option<&Value>, header: &str, text: &str, color: bool) -> Cell {
    let cell = if matches!(value, Some(Value::Number(_))) {
        cell.set_alignment(CellAlignment::Right)
    } else {
        cell
    };
    if !color {
        return cell;
    }
    match header.to_ascii_lowercase().as_str() {
        "status" => match text {
            "new" => cell.fg(Color::Yellow),
            "open" => cell.fg(Color::Red),
            "pending" => cell.fg(Color::Blue),
            "hold" => cell.fg(Color::Magenta),
            "solved" => cell.fg(Color::Green),
            "closed" => cell.fg(Color::DarkGrey),
            _ => cell,
        },
        "priority" => match text {
            "urgent" => cell.fg(Color::Red).add_attribute(Attribute::Bold),
            "high" => cell.fg(Color::Red),
            "low" => cell.fg(Color::DarkGrey),
            _ => cell,
        },
        _ => cell,
    }
}

/// Text for a cell. Timestamps become local `YYYY-MM-DD HH:MM`; long text columns are truncated.
#[must_use]
pub fn cell_text(value: Option<&Value>, header: &str, truncate_long: bool) -> String {
    let text = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => {
            if looks_like_timestamp(s) {
                parse_timestamp(s).map_or_else(|| s.clone(), |t| format_local(&t))
            } else {
                collapse_whitespace(s)
            }
        }
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Array(items)) if items.iter().all(|i| !i.is_array() && !i.is_object()) => items
            .iter()
            .map(|i| match i {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => other.to_string(),
    };
    if truncate_long && is_long_text_column(header) {
        truncate(&text, TRUNCATE_AT)
    } else {
        text
    }
}

fn is_long_text_column(header: &str) -> bool {
    let h = header.to_ascii_lowercase();
    ["subject", "body", "description", "title", "comment"]
        .iter()
        .any(|w| h.contains(w))
}

fn collapse_whitespace(s: &str) -> String {
    if s.contains(['\n', '\r', '\t']) {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        s.to_string()
    }
}

/// Cut at `max` characters (not bytes) and append `…`.
#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opts(resource: &str) -> RenderOptions {
        RenderOptions {
            resource: Some(resource.into()),
            width: Some(160),
            ..Default::default()
        }
    }

    #[test]
    fn every_preset_resolves_by_name_and_alias() {
        for p in presets::all() {
            assert!(std::ptr::eq(presets::for_resource(p.resource).unwrap(), *p));
            assert!(!p.columns.is_empty());
        }
        assert_eq!(
            presets::for_resource("orgs").unwrap().resource,
            "organizations"
        );
        assert!(presets::for_resource("widgets").is_none());
        let headers: Vec<&str> = presets::TICKETS.columns.iter().map(|c| c.header).collect();
        assert_eq!(
            headers,
            [
                "ID",
                "STATUS",
                "PRIORITY",
                "SUBJECT",
                "REQUESTER",
                "ASSIGNEE",
                "UPDATED"
            ]
        );
        let headers: Vec<&str> = presets::PROFILES.columns.iter().map(|c| c.header).collect();
        assert_eq!(headers, ["PROFILE", "SUBDOMAIN", "AUTH", "PLAN"]);
    }

    #[test]
    fn ticket_preset_uses_sideloaded_names_with_id_fallback() {
        let rows = vec![
            json!({"id": 1, "status": "open", "priority": "high", "subject": "S", "assignee": {"name": "Ada"}, "requester_id": 9, "updated_at": "2026-09-11T06:34:57Z"}),
            json!({"id": 2, "status": "solved", "subject": "T"}),
        ];
        let out = render_rows(&rows, &opts("tickets"));
        assert!(out.contains("ID"), "{out}");
        assert!(out.contains("Ada"), "{out}");
        assert!(out.contains(" 9 "), "{out}");
        assert!(out.contains("2026-09-11 "), "local-time rendering: {out}");
        assert!(!out.contains("T06:34:57Z"), "{out}");
    }

    #[test]
    fn long_subjects_are_truncated_with_an_ellipsis_but_ids_are_not() {
        let long = "x".repeat(200);
        let rows = vec![json!({"id": 123_456_789, "subject": long, "description": "line1\nline2"})];
        let out = render_rows(
            &rows,
            &RenderOptions {
                width: Some(400),
                ..Default::default()
            },
        );
        assert!(out.contains(&format!("{}…", "x".repeat(79))), "{out}");
        assert!(!out.contains(&"x".repeat(81)), "{out}");
        assert!(out.contains("123456789"), "{out}");
        assert!(out.contains("line1 line2"), "{out}");
    }

    #[test]
    fn auto_columns_are_the_first_eight_keys_and_fields_override_them() {
        let row = json!({"a": 1, "b": 2, "c": 3, "d": 4, "e": 5, "f": 6, "g": 7, "h": 8, "i": 9});
        let out = render_rows(
            std::slice::from_ref(&row),
            &RenderOptions {
                width: Some(200),
                ..Default::default()
            },
        );
        assert!(out.contains(" H "), "{out}");
        assert!(!out.contains(" I "), "{out}");
        let out = render_rows(
            &[row],
            &RenderOptions {
                fields: vec!["i,a".into()],
                width: Some(200),
                ..Default::default()
            },
        );
        assert!(out.contains(" I "), "{out}");
        assert!(!out.contains(" H "), "{out}");
    }

    #[test]
    fn scalars_and_arrays_render_sensibly() {
        assert_eq!(cell_text(Some(&json!(["a", "b"])), "TAGS", true), "a, b");
        assert_eq!(cell_text(Some(&json!({"k": 1})), "X", true), "{\"k\":1}");
        assert_eq!(cell_text(Some(&json!(true)), "ACTIVE", true), "true");
        assert_eq!(cell_text(None, "X", true), "");
        assert_eq!(cell_text(Some(&json!(null)), "X", true), "");
        assert_eq!(truncate("héllo wörld", 5), "héll…");
        assert_eq!(truncate("short", 80), "short");
    }

    #[test]
    fn colour_is_only_applied_when_enabled() {
        let rows = vec![json!({"id": 1, "status": "open", "priority": "urgent"})];
        let plain = render_rows(&rows, &opts("tickets"));
        assert!(!plain.contains("\u{1b}["), "{plain}");
        let coloured = render_rows(
            &rows,
            &RenderOptions {
                color: true,
                ..opts("tickets")
            },
        );
        assert!(coloured.contains("\u{1b}["), "{coloured}");
    }
}
