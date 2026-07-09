//! Renderer with an optional GPU backend and SIMD kernels.

/// Render a frame.
#[must_use]
pub fn render() -> u32 {
    0
}

#[cfg(feature = "gpu")]
pub mod gpu {}

#[cfg(feature = "simd")]
pub mod simd {}
