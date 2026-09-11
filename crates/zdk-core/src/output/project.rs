//! Dot-path projection for `--fields` / `--exclude` (`assignee.name`, `tags.0`, `custom.Environment`).

use std::fmt;

use serde_json::{Map, Value};

use crate::{Result, ZdkError};

/// One step of a dot path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Index(usize),
}

/// A parsed dot path such as `assignee.name` or `tags.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotPath(pub Vec<Segment>);

impl DotPath {
    /// `a.b.0` → `[Key(a), Key(b), Index(0)]`. Numeric segments are indices but also match
    /// object keys that happen to be numeric strings (custom field ids).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        Self(
            s.split('.')
                .filter(|p| !p.is_empty())
                .map(|p| {
                    p.parse::<usize>()
                        .map_or_else(|_| Segment::Key(p.to_string()), Segment::Index)
                })
                .collect(),
        )
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for DotPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, seg) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            match seg {
                Segment::Key(k) => f.write_str(k)?,
                Segment::Index(n) => write!(f, "{n}")?,
            }
        }
        Ok(())
    }
}

/// Parse a `--fields`/`--exclude` list (each entry may itself be comma-separated).
pub fn parse_paths(list: &[String]) -> Result<Vec<DotPath>> {
    let mut out = Vec::new();
    for entry in list {
        for part in entry.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let path = DotPath::parse(part);
            if path.is_empty() {
                return Err(ZdkError::Usage(format!("invalid field path '{part}'")));
            }
            out.push(path);
        }
    }
    Ok(out)
}

/// Look a path up; `None` when any step is missing.
#[must_use]
pub fn get<'a>(value: &'a Value, path: &DotPath) -> Option<&'a Value> {
    let mut cur = value;
    for seg in &path.0 {
        cur = match (cur, seg) {
            (Value::Object(map), Segment::Key(k)) => map.get(k)?,
            (Value::Object(map), Segment::Index(n)) => map.get(&n.to_string())?,
            (Value::Array(items), Segment::Index(n)) => items.get(*n)?,
            (Value::Array(items), Segment::Key(k)) => items.get(k.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Keep only `paths`, preserving nesting (`a.b` → `{"a": {"b": …}}`); missing paths become `null`.
/// Arrays are projected element-wise.
#[must_use]
pub fn select(value: &Value, paths: &[DotPath]) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|v| select(v, paths)).collect()),
        other => {
            let mut out = Value::Object(Map::new());
            for path in paths {
                let found = get(other, path).cloned().unwrap_or(Value::Null);
                insert_at(&mut out, &path.0, found);
            }
            out
        }
    }
}

/// Remove `paths` (deep). Arrays are handled element-wise; an index segment removes that element.
#[must_use]
pub fn exclude(value: &Value, paths: &[DotPath]) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|v| exclude(v, paths)).collect()),
        other => {
            let mut out = other.clone();
            for path in paths {
                remove_at(&mut out, &path.0);
            }
            out
        }
    }
}

fn insert_at(target: &mut Value, segments: &[Segment], value: Value) {
    let Some((first, rest)) = segments.split_first() else {
        *target = value;
        return;
    };
    match first {
        Segment::Key(k) => {
            if !target.is_object() {
                *target = Value::Object(Map::new());
            }
            let map = target
                .as_object_mut()
                .unwrap_or_else(|| unreachable!("just made an object"));
            let slot = map.entry(k.clone()).or_insert(Value::Null);
            insert_at(slot, rest, value);
        }
        Segment::Index(_) => {
            // Selected array elements are packed in selection order rather than padded with nulls.
            if !target.is_array() {
                *target = Value::Array(Vec::new());
            }
            let items = target
                .as_array_mut()
                .unwrap_or_else(|| unreachable!("just made an array"));
            let mut slot = Value::Null;
            insert_at(&mut slot, rest, value);
            items.push(slot);
        }
    }
}

fn remove_at(target: &mut Value, segments: &[Segment]) {
    let Some((first, rest)) = segments.split_first() else {
        return;
    };
    if rest.is_empty() {
        match (target, first) {
            (Value::Object(map), Segment::Key(k)) => {
                map.shift_remove(k);
            }
            (Value::Object(map), Segment::Index(n)) => {
                map.shift_remove(&n.to_string());
            }
            (Value::Array(items), Segment::Index(n)) if *n < items.len() => {
                items.remove(*n);
            }
            _ => {}
        }
        return;
    }
    match (target, first) {
        (Value::Object(map), Segment::Key(k)) => {
            if let Some(child) = map.get_mut(k) {
                remove_at(child, rest);
            }
        }
        (Value::Object(map), Segment::Index(n)) => {
            if let Some(child) = map.get_mut(&n.to_string()) {
                remove_at(child, rest);
            }
        }
        (Value::Array(items), Segment::Index(n)) => {
            if let Some(child) = items.get_mut(*n) {
                remove_at(child, rest);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ticket() -> Value {
        json!({
            "id": 42,
            "subject": "Printer on fire",
            "assignee": {"id": 7, "name": "Ada", "email": "ada@x"},
            "tags": ["a", "b", "c"],
            "custom_fields": [{"id": 100, "value": "prod"}],
            "1234": "numeric key"
        })
    }

    #[test]
    fn get_walks_objects_arrays_and_numeric_keys() {
        let t = ticket();
        assert_eq!(
            get(&t, &DotPath::parse("assignee.name")),
            Some(&json!("Ada"))
        );
        assert_eq!(get(&t, &DotPath::parse("tags.1")), Some(&json!("b")));
        assert_eq!(
            get(&t, &DotPath::parse("custom_fields.0.value")),
            Some(&json!("prod"))
        );
        assert_eq!(
            get(&t, &DotPath::parse("1234")),
            Some(&json!("numeric key"))
        );
        assert_eq!(get(&t, &DotPath::parse("assignee.phone")), None);
        assert_eq!(get(&t, &DotPath::parse("tags.9")), None);
        assert_eq!(get(&t, &DotPath::parse("subject.length")), None);
    }

    #[test]
    fn select_keeps_nesting_packs_indices_and_nulls_missing() {
        let t = ticket();
        let paths = parse_paths(&[
            "id,assignee.name".into(),
            "tags.0".into(),
            "tags.2".into(),
            "nope.deep".into(),
        ])
        .unwrap();
        let out = select(&t, &paths);
        assert_eq!(
            out,
            json!({"id": 42, "assignee": {"name": "Ada"}, "tags": ["a", "c"], "nope": {"deep": null}})
        );
    }

    #[test]
    fn select_and_exclude_apply_to_each_array_element() {
        let list = json!([ticket(), ticket()]);
        let out = select(&list, &[DotPath::parse("id")]);
        assert_eq!(out, json!([{"id": 42}, {"id": 42}]));
        let out = exclude(&list, &[DotPath::parse("assignee")]);
        assert!(out[0].get("assignee").is_none());
        assert_eq!(out[1]["id"], 42);
    }

    #[test]
    fn exclude_removes_deep_keys_and_array_elements() {
        let t = ticket();
        let out = exclude(
            &t,
            &[
                DotPath::parse("assignee.email"),
                DotPath::parse("tags.1"),
                DotPath::parse("missing.x"),
            ],
        );
        assert_eq!(out["assignee"], json!({"id": 7, "name": "Ada"}));
        assert_eq!(out["tags"], json!(["a", "c"]));
        assert_eq!(out["id"], 42);
    }

    #[test]
    fn select_then_exclude_is_stable() {
        let t = ticket();
        let sel = select(&t, &[DotPath::parse("id"), DotPath::parse("assignee")]);
        let once = exclude(&sel, &[DotPath::parse("assignee.email")]);
        let twice = exclude(&once, &[DotPath::parse("assignee.email")]);
        assert_eq!(once, twice);
        assert_eq!(
            select(&once, &[DotPath::parse("id"), DotPath::parse("assignee")]),
            once
        );
    }

    #[test]
    fn paths_display_round_trip() {
        for p in ["a.b.c", "tags.0", "custom.Environment"] {
            assert_eq!(DotPath::parse(p).to_string(), p);
        }
        assert!(parse_paths(&[".".into()]).is_err());
    }
}
