//! Planning stages before cargo-fc runs Cargo.

/// Resolve configured/CLI/environment targets into package assignments.
pub mod targets;

/// Resolve target assignments into runnable feature-combination plans.
pub mod execution;
