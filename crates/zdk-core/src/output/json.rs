//! Streaming JSON: `[` item, item, … `]` — pretty by default, one line with `--compact`.

use serde_json::Value;

use super::{OutputSink, write_stdout};
use crate::Result;

/// Writes a JSON array incrementally so `--all` never buffers a whole export.
#[derive(Debug)]
pub struct JsonSink {
    compact: bool,
    count: usize,
}

impl JsonSink {
    #[must_use]
    pub fn new(compact: bool) -> Self {
        Self { compact, count: 0 }
    }
}

impl OutputSink for JsonSink {
    fn begin(&mut self) -> Result<()> {
        write_stdout(b"[");
        Ok(())
    }

    fn item(&mut self, value: Value) -> Result<()> {
        let mut buf = String::new();
        if self.compact {
            if self.count > 0 {
                buf.push(',');
            }
            buf.push_str(&value.to_string());
        } else {
            buf.push_str(if self.count > 0 { ",\n  " } else { "\n  " });
            buf.push_str(&indent(&serde_json::to_string_pretty(&value)?, "  "));
        }
        write_stdout(buf.as_bytes());
        self.count += 1;
        Ok(())
    }

    fn end(&mut self) -> Result<()> {
        if self.compact || self.count == 0 {
            write_stdout(b"]\n");
        } else {
            write_stdout(b"\n]\n");
        }
        Ok(())
    }
}

/// One value, followed by a newline.
pub fn write_value(value: &Value, compact: bool) {
    write_stdout(to_string(value, compact).as_bytes());
    write_stdout(b"\n");
}

/// Serialise (pretty unless `compact`). Serialisation of a `Value` cannot fail.
#[must_use]
pub fn to_string(value: &Value, compact: bool) -> String {
    if compact {
        value.to_string()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    }
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .collect::<Vec<_>>()
        .join(&format!("\n{prefix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_prefixes_every_line_but_the_first() {
        assert_eq!(indent("{\n  \"a\": 1\n}", "  "), "{\n    \"a\": 1\n  }");
    }

    #[test]
    fn to_string_honours_compact() {
        let v = serde_json::json!({"a": [1, 2]});
        assert_eq!(to_string(&v, true), "{\"a\":[1,2]}");
        assert!(to_string(&v, false).contains('\n'));
    }
}
