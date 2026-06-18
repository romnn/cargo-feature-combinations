//! Demo app crate whose features imply one another.

/// Run the app.
#[must_use]
pub fn run() -> &'static str {
    "ok"
}

/// JSON support.
#[cfg(feature = "json")]
pub mod json {
    /// Parse a JSON document.
    #[must_use]
    pub fn parse() -> bool {
        true
    }
}

/// YAML support.
#[cfg(feature = "yaml")]
pub mod yaml {
    /// Parse a YAML document.
    #[must_use]
    pub fn parse() -> bool {
        true
    }
}
