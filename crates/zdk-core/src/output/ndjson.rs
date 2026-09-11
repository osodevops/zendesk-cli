//! Newline-delimited JSON: one compact object per line, streamed as records arrive.

use serde_json::Value;

use super::{OutputSink, write_stdout_line};
use crate::Result;

#[derive(Debug, Default)]
pub struct NdjsonSink;

impl OutputSink for NdjsonSink {
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
