//! A codec crate with independent format and compression axes.

/// Encode a value.
#[must_use]
pub fn encode(bytes: &[u8]) -> usize {
    bytes.len()
}

#[cfg(feature = "json")]
pub mod json {}
#[cfg(feature = "yaml")]
pub mod yaml {}
#[cfg(feature = "msgpack")]
pub mod msgpack {}
#[cfg(feature = "gzip")]
pub mod gzip {}
#[cfg(feature = "zstd")]
pub mod zstd {}
#[cfg(feature = "brotli")]
pub mod brotli {}
