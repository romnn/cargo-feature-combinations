//! Stub redis storage backend.

/// Backend name.
#[must_use]
pub fn name() -> &'static str {
    "redis"
}
