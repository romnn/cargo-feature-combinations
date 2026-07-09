//! A store with optional storage backends and an optional compression feature.

/// Open the store.
#[must_use]
pub fn open() -> &'static str {
    "store"
}

#[cfg(feature = "compression")]
pub mod compression {}
