//! Request bodies: `--data @file|-|json` (escape hatch), `--file`/`--from-stdin` resource
//! documents, `--body`/`--body-file`/`--editor` text, and the merge order
//! `--file` < typed flags < `--field`.

use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value};
use zdk_core::api::curated::unwrap_key;
use zdk_core::{Result, ZdkError};

use super::field::{apply_fields, insert_dotted, parse_field};

/// `--data` (inline JSON, `@file`, `-`) or the `--field` set, never both.
pub(crate) fn build_body(data: Option<&str>, fields: &[String]) -> Result<Option<Value>> {
    if data.is_some() && !fields.is_empty() {
        return Err(ZdkError::Usage(
            "--data and --field cannot be combined; put everything in --data or use --field only"
                .into(),
        ));
    }
    if let Some(d) = data {
        let text = match d {
            "-" => read_stdin()?,
            file if file.starts_with('@') => {
                let path = &file[1..];
                std::fs::read_to_string(path)
                    .map_err(|e| ZdkError::Usage(format!("--data: cannot read {path}: {e}")))?
            }
            inline => inline.to_string(),
        };
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| ZdkError::Usage(format!("--data is not valid JSON: {e}")))?;
        return Ok(Some(value));
    }
    if fields.is_empty() {
        return Ok(None);
    }
    let mut root = Value::Object(Map::new());
    for f in fields {
        let (key, value) = parse_field(f)?;
        insert_dotted(&mut root, &key, value)?;
    }
    Ok(Some(root))
}

/// All of stdin as text.
pub(crate) fn read_stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

/// A JSON document from `--file PATH` (`-` = stdin) or `--from-stdin`.
pub(crate) fn read_json_source(file: Option<&Path>, from_stdin: bool) -> Result<Option<Value>> {
    let text = match (file, from_stdin) {
        (Some(_), true) => {
            return Err(ZdkError::Usage(
                "--file and --from-stdin cannot be combined".into(),
            ));
        }
        (Some(p), false) if p.as_os_str() == "-" => read_stdin()?,
        (Some(p), false) => std::fs::read_to_string(p)
            .map_err(|e| ZdkError::Usage(format!("--file: cannot read {}: {e}", p.display())))?,
        (None, true) => read_stdin()?,
        (None, false) => return Ok(None),
    };
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        ZdkError::Usage(format!(
            "{} is not valid JSON: {e}",
            if from_stdin { "stdin" } else { "--file" }
        ))
    })?;
    Ok(Some(value))
}

/// Merge `overlay` into `base`: objects recurse, everything else replaces. `Null` in the
/// overlay replaces too (so a flag can clear a value).
pub(crate) fn deep_merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) if slot.is_object() && v.is_object() => deep_merge(slot, v),
                    _ => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// The resource object for a create/update: `--file`/`--from-stdin` (either the bare object
/// or `{key: …}`) < `flags` (typed flags, `Null` values skipped) < `--field` (dotted keys).
pub(crate) fn resource_body(
    key: &str,
    file: Option<&Path>,
    from_stdin: bool,
    flags: Value,
    fields: &[String],
) -> Result<Value> {
    let mut body = match read_json_source(file, from_stdin)? {
        Some(doc) => {
            let inner = unwrap_key(doc, key);
            if !inner.is_object() {
                return Err(ZdkError::Usage(format!(
                    "the document must be a JSON object (optionally wrapped as {{\"{key}\": {{…}}}})"
                )));
            }
            inner
        }
        None => Value::Object(Map::new()),
    };
    deep_merge(&mut body, strip_nulls(flags));
    apply_fields(&mut body, fields)?;
    Ok(body)
}

/// Drop `Null` members of a flag object (an unset `Option` flag must not clear a file value).
fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).collect())
        }
        other => other,
    }
}

/// Free text from `--body TEXT` / `--body-file PATH` (`-` = stdin).
pub(crate) fn read_text_source(
    inline: Option<&str>,
    file: Option<&Path>,
) -> Result<Option<String>> {
    match (inline, file) {
        (Some(_), Some(_)) => Err(ZdkError::Usage(
            "give the text inline or with a file, not both".into(),
        )),
        (Some(t), None) => Ok(Some(t.to_string())),
        (None, Some(p)) if p.as_os_str() == "-" => Ok(Some(read_stdin()?)),
        (None, Some(p)) => std::fs::read_to_string(p)
            .map(Some)
            .map_err(|e| ZdkError::Usage(format!("cannot read {}: {e}", p.display()))),
        (None, None) => Ok(None),
    }
}

/// Lines starting with `#` are instructions; the rest, trimmed, is the message.
pub(crate) fn strip_comment_lines(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Open `$VISUAL` / `$EDITOR` on a temp file seeded with `template` and return what was
/// written (comment lines stripped). Empty → usage error, like `git commit`.
pub(crate) fn edit_text(editor: Option<&str>, template: &str) -> Result<String> {
    let editor = editor.ok_or_else(|| {
        ZdkError::Usage(
            "--editor needs $VISUAL or $EDITOR (e.g. `export EDITOR=vim`); or pass --body / --body-file".into(),
        )
    })?;
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| ZdkError::Usage("$EDITOR is empty".into()))?;
    let path = std::env::temp_dir().join(format!("zdk-{}.md", uuid::Uuid::new_v4()));
    std::fs::write(&path, template)?;
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .map_err(|e| ZdkError::Other(format!("cannot run editor '{editor}': {e}")));
    let text = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    let status = status?;
    if !status.success() {
        return Err(ZdkError::Other(format!(
            "editor '{editor}' exited with {status}; nothing sent"
        )));
    }
    let body = strip_comment_lines(&text?);
    if body.is_empty() {
        return Err(ZdkError::Usage(
            "empty message from the editor; nothing sent".into(),
        ));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fields_build_nested_objects_and_raw_json() {
        let body = build_body(
            None,
            &[
                "ticket.subject=Hello".into(),
                "ticket.priority:=\"high\"".into(),
                "ticket.tags:=[\"a\",\"b\"]".into(),
                "ticket.requester.name=Ada".into(),
                "count:=3".into(),
            ],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            body,
            json!({
                "ticket": {"subject": "Hello", "priority": "high", "tags": ["a", "b"], "requester": {"name": "Ada"}},
                "count": 3
            })
        );
        let err = build_body(None, &["a=1".into(), "a.b=2".into()]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("non-object"), "{err}");
    }

    #[test]
    fn data_and_field_conflict_and_inline_data_parses() {
        assert_eq!(
            build_body(Some("{}"), &["a=b".into()])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            build_body(Some(r#"{"a":1}"#), &[]).unwrap().unwrap()["a"],
            1
        );
        assert_eq!(build_body(Some("nope"), &[]).unwrap_err().exit_code(), 2);
        assert_eq!(
            build_body(Some("@/definitely/missing.json"), &[])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert!(build_body(None, &[]).unwrap().is_none());
    }

    #[test]
    fn deep_merge_recurses_and_replaces_scalars() {
        let mut base = json!({"a": {"b": 1, "c": 2}, "d": [1], "e": "x"});
        deep_merge(
            &mut base,
            json!({"a": {"b": 9, "z": 0}, "d": [2, 3], "e": null, "f": true}),
        );
        assert_eq!(
            base,
            json!({"a": {"b": 9, "c": 2, "z": 0}, "d": [2, 3], "e": null, "f": true})
        );
        let mut scalar = json!(1);
        deep_merge(&mut scalar, json!({"k": 1}));
        assert_eq!(scalar, json!({"k": 1}));
    }

    #[test]
    fn resource_body_precedence_is_file_then_flags_then_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        std::fs::write(
            &path,
            r#"{"ticket": {"subject": "from file", "priority": "low", "tags": ["file"], "custom": {"a": 1}}}"#,
        )
        .unwrap();
        let body = resource_body(
            "ticket",
            Some(&path),
            false,
            json!({"subject": "from flag", "status": "open", "assignee_id": null}),
            &["subject=from field".into(), "custom.b:=2".into()],
        )
        .unwrap();
        assert_eq!(
            body["subject"], "from field",
            "--field beats flags beats file"
        );
        assert_eq!(body["priority"], "low", "file values survive");
        assert_eq!(body["status"], "open", "flags apply");
        assert!(
            body.get("assignee_id").is_none(),
            "unset flags do not clear file values"
        );
        assert_eq!(
            body["custom"],
            json!({"a": 1, "b": 2}),
            "fields nest into file objects"
        );
        assert_eq!(body["tags"], json!(["file"]));

        // A bare object (no envelope) works the same.
        std::fs::write(&path, r#"{"subject": "bare"}"#).unwrap();
        let body = resource_body("ticket", Some(&path), false, json!({}), &[]).unwrap();
        assert_eq!(body["subject"], "bare");
        // Nothing at all → {} with the flags.
        let body = resource_body("ticket", None, false, json!({"x": 1}), &[]).unwrap();
        assert_eq!(body, json!({"x": 1}));
        // Not an object → usage.
        std::fs::write(&path, "[1]").unwrap();
        assert_eq!(
            resource_body("ticket", Some(&path), false, json!({}), &[])
                .unwrap_err()
                .exit_code(),
            2
        );
        std::fs::write(&path, "nope").unwrap();
        assert_eq!(
            resource_body("ticket", Some(&path), false, json!({}), &[])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            read_json_source(Some(Path::new("/definitely/missing.json")), false)
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            read_json_source(Some(&path), true).unwrap_err().exit_code(),
            2
        );
    }

    #[test]
    fn text_sources_and_comment_stripping() {
        assert_eq!(
            read_text_source(Some("hi"), None).unwrap().as_deref(),
            Some("hi")
        );
        assert!(read_text_source(None, None).unwrap().is_none());
        assert_eq!(
            read_text_source(Some("a"), Some(Path::new("b")))
                .unwrap_err()
                .exit_code(),
            2
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body.md");
        std::fs::write(&path, "from file\n").unwrap();
        assert_eq!(
            read_text_source(None, Some(&path)).unwrap().as_deref(),
            Some("from file\n")
        );
        assert_eq!(
            read_text_source(None, Some(Path::new("/definitely/missing")))
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            strip_comment_lines("# instructions\n\nhello\n  # not a comment? it is\nworld\n"),
            "hello\nworld"
        );
        assert_eq!(strip_comment_lines("# only\n#comments"), "");
    }

    #[test]
    fn editor_errors_are_usage_or_generic() {
        assert_eq!(edit_text(None, "").unwrap_err().exit_code(), 2);
        assert_eq!(edit_text(Some("   "), "").unwrap_err().exit_code(), 2);
        // `true` leaves the template untouched: comments only → empty → usage.
        assert_eq!(
            edit_text(Some("true"), "# comment\n")
                .unwrap_err()
                .exit_code(),
            2
        );
        // `false` fails → generic error.
        assert_eq!(edit_text(Some("false"), "x").unwrap_err().exit_code(), 1);
        assert_eq!(
            edit_text(Some("/definitely/not/an/editor"), "x")
                .unwrap_err()
                .exit_code(),
            1
        );
        // `true` with a non-comment template returns it.
        assert_eq!(edit_text(Some("true"), "# hint\nkept\n").unwrap(), "kept");
    }
}
