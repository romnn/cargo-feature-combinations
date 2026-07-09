//! Demo crate — clean across every feature combination on every configured target.

/// Run the app.
#[must_use]
pub fn run() -> &'static str {
    "ok"
}

/// Configuration loading, gated behind the `std` feature.
#[cfg(feature = "std")]
pub mod config {
    /// Return the default configuration name.
    #[must_use]
    pub fn default_name() -> String {
        String::from("app")
    }
}

/// A stub SIMD kernel, gated behind the `simd` feature.
#[cfg(feature = "simd")]
pub mod simd {
    /// Sum a slice of values.
    #[must_use]
    pub fn sum(values: &[u32]) -> u32 {
        values.iter().copied().sum()
    }
}
