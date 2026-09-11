//! Output of `cargo xtask codegen`. Do not edit by hand; regenerate from `specs/`
//! (`cargo xtask codegen`; CI verifies freshness with `cargo xtask codegen --check`).

pub mod registry;

pub use registry::{OPERATIONS, SPEC_VERSIONS};

/// gzip-compressed JSON `{ "<spec>.<operationId>": { description, parameters, request_body,
/// responses } }` — descriptions and flattened schemas for every operation. Inflated lazily by
/// [`crate::api::detail`].
pub static DETAIL_GZ: &[u8] = include_bytes!("detail.json.gz");
