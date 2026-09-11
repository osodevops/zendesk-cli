//! `-o raw`: the API body untouched. Given structured values instead (curated commands that
//! never had raw bytes), it falls back to one compact JSON value per line.

use serde_json::Value;

use super::{OutputSink, write_stdout, write_stdout_line};
use crate::Result;

/// Pass response bytes straight through, adding a trailing newline if the body lacks one.
pub fn write(bytes: &[u8]) {
    write_stdout(bytes);
    if !bytes.ends_with(b"\n") {
        write_stdout(b"\n");
    }
}

#[derive(Debug, Default)]
pub struct RawSink;

impl OutputSink for RawSink {
    fn begin(&mut self) -> Result<()> {
        Ok(())
    }

    fn item(&mut self, value: Value) -> Result<()> {
        write_stdout_line(&value.to_string());
        Ok(())
    }

    fn end(&mut self) -> Result<()> {
        Ok(())
    }
}
