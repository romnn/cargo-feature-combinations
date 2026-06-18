//! Demo engine crate — clean across every feature combination.

/// Run the engine.
#[must_use]
pub fn run() -> &'static str {
    "ok"
}

/// Metrics collection.
#[cfg(feature = "metrics")]
pub mod metrics {
    /// Number of collected samples.
    #[must_use]
    pub fn samples() -> usize {
        0
    }
}

/// Structured tracing.
#[cfg(feature = "tracing")]
pub mod tracing {
    /// Open a span with the given name.
    #[must_use]
    pub fn span(name: &str) -> &str {
        name
    }
}
