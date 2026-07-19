//! Tiny HTTP client with three mutually exclusive TLS backends.

// Enabling multiple backends is a real conflict that the feature group keeps
// out of the generated matrix.
#[cfg(any(
    all(feature = "native-tls", feature = "rustls"),
    all(feature = "native-tls", feature = "boring"),
    all(feature = "rustls", feature = "boring"),
))]
compile_error!("enable only one TLS backend: `native-tls`, `rustls`, or `boring`");

/// Connect to a URL using the selected TLS backend.
#[must_use]
pub fn connect(url: &str) -> &str {
    url
}

#[cfg(feature = "native-tls")]
pub mod native_tls_backend {
    /// Name of the active TLS backend.
    #[must_use]
    pub fn backend() -> &'static str {
        "native-tls"
    }
}

#[cfg(feature = "rustls")]
pub mod rustls_backend {
    /// Name of the active TLS backend.
    #[must_use]
    pub fn backend() -> &'static str {
        "rustls"
    }
}

#[cfg(feature = "boring")]
pub mod boring_backend {
    /// Name of the active TLS backend.
    #[must_use]
    pub fn backend() -> &'static str {
        "boring"
    }
}
