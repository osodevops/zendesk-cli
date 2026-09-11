//! `--field k=v` (string) / `k:=json` (typed) with dotted keys, and `--custom-field id=v`.

use serde_json::{Map, Value};
use zdk_core::{Result, ZdkError};

/// `k=v` → string, `k:=json` → parsed JSON.
pub(crate) fn parse_field(f: &str) -> Result<(String, Value)> {
    if let Some((k, v)) = f.split_once(":=") {
        let value = serde_json::from_str(v)
            .map_err(|e| ZdkError::Usage(format!("--field {k}: '{v}' is not valid JSON: {e}")))?;
        return Ok((k.trim().to_string(), value));
    }
    if let Some((k, v)) = f.split_once('=') {
        return Ok((k.trim().to_string(), Value::String(v.to_string())));
    }
    Err(ZdkError::Usage(format!(
        "--field '{f}' must be key=value or key:=json"
    )))
}

/// `ticket.custom_fields.0` style keys build nested objects (numeric segments are object keys,
/// not array indexes — use `:=` with a JSON array for lists).
pub(crate) fn insert_dotted(root: &mut Value, dotted: &str, value: Value) -> Result<()> {
    let parts: Vec<&str> = dotted.split('.').collect();
    let mut cur = root;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            return Err(ZdkError::Usage(format!(
                "--field '{dotted}': empty key segment"
            )));
        }
        let obj = cur.as_object_mut().ok_or_else(|| {
            ZdkError::Usage(format!(
                "--field '{dotted}': '{}' is already set to a non-object value",
                parts[..i].join(".")
            ))
        })?;
        if i + 1 == parts.len() {
            obj.insert((*part).to_string(), value);
            return Ok(());
        }
        cur = obj
            .entry((*part).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    Ok(())
}

/// Apply every `--field` onto `root` (an object), in order — later fields win.
pub(crate) fn apply_fields(root: &mut Value, fields: &[String]) -> Result<()> {
    if !root.is_object() {
        *root = Value::Object(Map::new());
    }
    for f in fields {
        let (key, value) = parse_field(f)?;
        insert_dotted(root, &key, value)?;
    }
    Ok(())
}

/// `--custom-field id=value` for tickets: a numeric field id and a value (`id:=json` for
/// typed values such as `:=true` or `:=42`).
pub(crate) fn parse_ticket_custom_field(s: &str) -> Result<(u64, Value)> {
    let (key, value) = parse_field(s)
        .map_err(|_| ZdkError::Usage(format!("--custom-field '{s}' must be <field id>=<value>")))?;
    let id = key.parse::<u64>().map_err(|_| {
        ZdkError::Usage(format!(
            "--custom-field '{s}': '{key}' is not a numeric field id (name-based custom fields arrive with the v0.3 resolver; find ids with `zdk api GET /api/v2/ticket_fields`)"
        ))
    })?;
    Ok((id, value))
}

/// `--custom-field key=value` for users / organizations: the `user_fields` /
/// `organization_fields` key and its value.
pub(crate) fn parse_named_custom_field(s: &str) -> Result<(String, Value)> {
    let (key, value) = parse_field(s)
        .map_err(|_| ZdkError::Usage(format!("--custom-field '{s}' must be <key>=<value>")))?;
    if key.is_empty() {
        return Err(ZdkError::Usage(format!(
            "--custom-field '{s}': the field key is empty"
        )));
    }
    Ok((key, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_field_handles_strings_json_and_equals_in_values() {
        assert_eq!(
            parse_field("a=b=c").unwrap(),
            ("a".into(), Value::String("b=c".into()))
        );
        assert_eq!(parse_field("n:=3").unwrap(), ("n".into(), json!(3)));
        assert_eq!(
            parse_field(" tags :=[\"a\"]").unwrap(),
            ("tags".into(), json!(["a"]))
        );
        assert_eq!(
            parse_field("s:=\"quoted\"").unwrap(),
            ("s".into(), json!("quoted"))
        );
        assert_eq!(parse_field("novalue").unwrap_err().exit_code(), 2);
        assert_eq!(parse_field("a:={bad").unwrap_err().exit_code(), 2);
    }

    #[test]
    fn dotted_keys_nest_and_later_fields_win() {
        let mut root = json!({});
        apply_fields(
            &mut root,
            &[
                "ticket.subject=Hello".into(),
                "ticket.priority:=\"high\"".into(),
                "ticket.requester.name=Ada".into(),
                "ticket.subject=Replaced".into(),
                "count:=3".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            root,
            json!({
                "ticket": {"subject": "Replaced", "priority": "high", "requester": {"name": "Ada"}},
                "count": 3
            })
        );
        let mut scalar = json!(1);
        apply_fields(&mut scalar, &["a=1".into()]).unwrap();
        assert_eq!(scalar, json!({"a": "1"}));
        let err = apply_fields(&mut json!({}), &["a=1".into(), "a.b=2".into()]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("non-object"), "{err}");
        assert_eq!(
            insert_dotted(&mut json!({}), "a..b", json!(1))
                .unwrap_err()
                .exit_code(),
            2
        );
    }

    #[test]
    fn custom_fields_parse_ids_and_names() {
        assert_eq!(
            parse_ticket_custom_field("360000001=production").unwrap(),
            (360_000_001, json!("production"))
        );
        assert_eq!(
            parse_ticket_custom_field("7:=true").unwrap(),
            (7, json!(true))
        );
        let err = parse_ticket_custom_field("Environment=prod").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("numeric field id"), "{err}");
        assert_eq!(
            parse_ticket_custom_field("nope").unwrap_err().exit_code(),
            2
        );
        assert_eq!(
            parse_named_custom_field("tier=gold").unwrap(),
            ("tier".into(), json!("gold"))
        );
        assert_eq!(parse_named_custom_field("=x").unwrap_err().exit_code(), 2);
        assert_eq!(parse_named_custom_field("x").unwrap_err().exit_code(), 2);
    }
}
