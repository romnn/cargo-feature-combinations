//! Tiny HTTP client with two mutually-exclusive TLS backends.

// Enabling both backends is a real conflict — the excluded combination would
// otherwise fail to compile here.
#[cfg(all(feature = "native-tls", feature = "rustls"))]
compile_error!("enable only one TLS backend: `native-tls` or `rustls`");

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
