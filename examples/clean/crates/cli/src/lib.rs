//! Demo CLI crate built on top of `engine`.

use engine::run;

/// Print the engine status.
#[must_use]
pub fn status() -> &'static str {
    run()
}

/// Colored terminal output.
#[cfg(feature = "color")]
pub mod color {
    /// Wrap text in (stand-in) color markers.
    #[must_use]
    pub fn paint(text: &str) -> String {
        format!("<{text}>")
    }
}
