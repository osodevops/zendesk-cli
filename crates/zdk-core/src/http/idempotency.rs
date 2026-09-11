//! `Idempotency-Key` handling (PRD §11): every POST/PUT/PATCH carries one, fixed for the
//! lifetime of a request so a retry after a timeout cannot double-create.

use crate::api::Method;

/// The header name Zendesk recognises.
pub const HEADER: &str = "Idempotency-Key";

/// Methods that get a key when the caller did not supply one.
#[must_use]
pub const fn needs_key(method: Method) -> bool {
    matches!(method, Method::Post | Method::Put | Method::Patch)
}

/// A fresh UUID v4 key.
#[must_use]
pub fn generate() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_need_keys_reads_do_not() {
        assert!(needs_key(Method::Post));
        assert!(needs_key(Method::Put));
        assert!(needs_key(Method::Patch));
        assert!(!needs_key(Method::Get));
        assert!(!needs_key(Method::Delete));
        let a = generate();
        let b = generate();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
    }
}
