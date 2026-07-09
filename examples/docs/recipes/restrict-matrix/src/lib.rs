//! Web app rendered in one of several modes (only hydrate/ssr are shipped).

/// Render the app.
#[must_use]
pub fn render() -> &'static str {
    "<app/>"
}

#[cfg(feature = "hydrate")]
pub mod hydrate {}

#[cfg(feature = "ssr")]
pub mod ssr {}

#[cfg(feature = "csr")]
pub mod csr {}
