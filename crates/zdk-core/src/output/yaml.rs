//! YAML output (buffered — a YAML document needs its whole sequence).

use serde_json::Value;

use super::{OutputSink, write_stdout};
use crate::{Result, ZdkError};

#[derive(Debug, Default)]
pub struct YamlSink {
    items: Vec<Value>,
}

impl OutputSink for YamlSink {
    fn begin(&mut self) -> Result<()> {
        Ok(())
    }

    fn item(&mut self, value: Value) -> Result<()> {
        self.items.push(value);
        Ok(())
    }

    fn end(&mut self) -> Result<()> {
        let items = std::mem::take(&mut self.items);
        write_stdout(to_string(&Value::Array(items))?.as_bytes());
        Ok(())
    }
}

/// One value as a YAML document.
pub fn render_single(value: &Value) -> Result<()> {
    write_stdout(to_string(value)?.as_bytes());
    Ok(())
}

pub fn to_string(value: &Value) -> Result<String> {
    serde_yaml_ng::to_string(value).map_err(|e| ZdkError::Other(format!("YAML error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_sequences_and_nested_maps() {
        let v = serde_json::json!([{"id": 1, "tags": ["a", "b"]}, {"id": 2, "org": {"name": "x"}}]);
        let y = to_string(&v).unwrap();
        assert!(y.starts_with("- id: 1\n"), "{y}");
        assert!(y.contains("  org:\n    name: x\n"), "{y}");
    }
}
