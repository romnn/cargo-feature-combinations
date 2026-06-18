//! Demo loader crate with intentional lint issues.

/// Load the input into memory.
#[must_use]
pub fn load() -> Vec<u8> {
    Vec::new()
}

// Compiled in *every* feature combination and never used: the resulting
// `dead_code` warning repeats identically across the whole matrix, which is
// exactly what `cargo fc --dedupe` folds into a single copy.
fn checksum(bytes: &[u8]) -> usize {
    bytes.len()
}

/// Input validation.
#[cfg(feature = "validation")]
pub mod validation {
    /// Whether the input is non-empty.
    #[must_use]
    pub fn is_valid(input: &str) -> bool {
        !input.is_empty()
    }
}

/// An experimental entry point that does not compile yet.
#[cfg(feature = "experimental")]
#[must_use]
pub fn experimental() -> u32 {
    // Intentional compile error: this function does not exist. Feature
    // combinations that enable `experimental` therefore fail.
    missing_helper()
}
