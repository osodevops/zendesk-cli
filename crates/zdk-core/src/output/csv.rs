//! CSV and TSV: header from `--fields` or the union of keys; nested values are JSON-encoded.

use serde_json::Value;

use super::project::{DotPath, get};
use super::{OutputSink, RenderOptions, write_stdout};
use crate::{Result, ZdkError};

/// Buffers rows so the header can be the union of every record's keys.
#[derive(Debug)]
pub struct CsvSink {
    delimiter: u8,
    fields: Vec<String>,
    rows: Vec<Value>,
}

impl CsvSink {
    #[must_use]
    pub fn new(delimiter: u8, opts: &RenderOptions) -> Self {
        Self {
            delimiter,
            fields: opts.fields.clone(),
            rows: Vec::new(),
        }
    }
}

impl OutputSink for CsvSink {
    fn begin(&mut self) -> Result<()> {
        Ok(())
    }

    fn item(&mut self, value: Value) -> Result<()> {
        self.rows.push(value);
        Ok(())
    }

    fn end(&mut self) -> Result<()> {
        let rows = std::mem::take(&mut self.rows);
        let out = to_bytes(&rows, &self.fields, self.delimiter)?;
        write_stdout(&out);
        Ok(())
    }
}

/// A single object as a one-row CSV/TSV.
pub fn render_single(value: &Value, delimiter: u8, opts: &RenderOptions) -> Result<()> {
    let out = to_bytes(std::slice::from_ref(value), &opts.fields, delimiter)?;
    write_stdout(&out);
    Ok(())
}

/// Column headers: explicit fields, else the union of top-level keys in first-seen order.
#[must_use]
pub fn headers(rows: &[Value], fields: &[String]) -> Vec<String> {
    if !fields.is_empty() {
        return fields.to_vec();
    }
    let mut out: Vec<String> = Vec::new();
    let mut any_object = false;
    for row in rows {
        if let Value::Object(map) = row {
            any_object = true;
            for key in map.keys() {
                if !out.iter().any(|k| k == key) {
                    out.push(key.clone());
                }
            }
        }
    }
    if !any_object && !rows.is_empty() {
        out.push("value".into());
    }
    out
}

/// A cell: scalars verbatim, `null` empty, arrays/objects as compact JSON.
#[must_use]
pub fn cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other @ (Value::Array(_) | Value::Object(_))) => other.to_string(),
    }
}

fn to_bytes(rows: &[Value], fields: &[String], delimiter: u8) -> Result<Vec<u8>> {
    let headers = headers(rows, fields);
    let paths: Vec<DotPath> = headers.iter().map(|h| DotPath::parse(h)).collect();
    let mut w = ::csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(Vec::new());
    let io = |e: ::csv::Error| ZdkError::Other(format!("CSV error: {e}"));
    w.write_record(&headers).map_err(io)?;
    for row in rows {
        let record: Vec<String> = match row {
            Value::Object(_) => paths.iter().map(|p| cell(get(row, p))).collect(),
            other => vec![cell(Some(other))],
        };
        w.write_record(&record).map_err(io)?;
    }
    w.into_inner()
        .map_err(|e| ZdkError::Other(format!("CSV error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_the_union_of_keys_in_first_seen_order() {
        let rows = vec![
            serde_json::json!({"id": 1, "b": 2}),
            serde_json::json!({"id": 2, "a": 3}),
        ];
        assert_eq!(headers(&rows, &[]), vec!["id", "b", "a"]);
        assert_eq!(headers(&rows, &["x".to_string()]), vec!["x"]);
    }

    #[test]
    fn escapes_delimiters_quotes_and_newlines_and_encodes_nested_values() {
        let rows = vec![serde_json::json!({
            "id": 1,
            "subject": "a, \"quoted\" line\nsecond",
            "tags": ["x", "y"],
            "org": {"name": "n"},
            "gone": null,
            "ok": true
        })];
        let out = String::from_utf8(to_bytes(&rows, &[], b',').unwrap()).unwrap();
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), "id,subject,tags,org,gone,ok");
        let body = out.split_once('\n').unwrap().1;
        assert_eq!(
            body,
            "1,\"a, \"\"quoted\"\" line\nsecond\",\"[\"\"x\"\",\"\"y\"\"]\",\"{\"\"name\"\":\"\"n\"\"}\",,true\n"
        );
    }

    #[test]
    fn tsv_uses_tabs_and_dot_paths_resolve_nested_fields() {
        let rows = vec![serde_json::json!({"id": 7, "assignee": {"name": "Ada"}})];
        let out = String::from_utf8(
            to_bytes(
                &rows,
                &["id".into(), "assignee.name".into(), "missing".into()],
                b'\t',
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(out, "id\tassignee.name\tmissing\n7\tAda\t\n");
    }

    #[test]
    fn scalars_get_a_value_column() {
        let rows = vec![serde_json::json!(1), serde_json::json!("two")];
        let out = String::from_utf8(to_bytes(&rows, &[], b',').unwrap()).unwrap();
        assert_eq!(out, "value\n1\ntwo\n");
    }
}
