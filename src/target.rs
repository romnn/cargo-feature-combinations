use color_eyre::eyre::{self, WrapErr};
use std::process::Command;

/// A Rust target triple.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetTriple(pub String);

impl TargetTriple {
    /// Borrow this target triple as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Detect the effective compilation target for this invocation.
///
/// Resolution order:
/// 1. `--target <triple>` CLI flag (authoritative)
/// 2. `CARGO_BUILD_TARGET` environment variable
/// 3. Host target via `rustc -vV`
pub trait TargetDetector {
    /// Determine the effective target triple.
    ///
    /// # Errors
    ///
    /// Returns an error if the target triple cannot be determined.
    fn detect_target(&self, cargo_args: &[String]) -> eyre::Result<TargetTriple>;
}

/// Detect the host target by invoking `rustc -vV`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RustcTargetDetector;

impl RustcTargetDetector {
    fn host_triple() -> eyre::Result<TargetTriple> {
        let output = Command::new("rustc")
            .arg("-vV")
            .output()
            .wrap_err("failed to invoke rustc to detect host target")?;

        if !output.status.success() {
            eyre::bail!("rustc -vV failed with exit code {:?}", output.status.code());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(host) = line.strip_prefix("host: ") {
                let host = host.trim();
                if host.is_empty() {
                    continue;
                }
                return Ok(TargetTriple(host.to_string()));
            }
        }

        eyre::bail!("could not parse host target triple from `rustc -vV`")
    }

    fn parse_target_flag(cargo_args: &[String]) -> Option<String> {
        // Support both `--target x` and `--target=x`.
        let mut it = cargo_args.iter();
        while let Some(arg) = it.next() {
            if arg == "--target"
                && let Some(v) = it.next()
            {
                return Some(v.clone());
            }
            if let Some(v) = arg.strip_prefix("--target=")
                && !v.is_empty()
            {
                return Some(v.to_string());
            }
        }
        None
    }
}

impl TargetDetector for RustcTargetDetector {
    fn detect_target(&self, cargo_args: &[String]) -> eyre::Result<TargetTriple> {
        if let Some(triple) = Self::parse_target_flag(cargo_args) {
            return Ok(TargetTriple(triple));
        }
        if let Ok(triple) = std::env::var("CARGO_BUILD_TARGET") {
            let triple = triple.trim();
            if !triple.is_empty() {
                return Ok(TargetTriple(triple.to_string()));
            }
        }
        Self::host_triple()
    }
}

#[cfg(test)]
mod test {
    use super::{RustcTargetDetector, TargetDetector};
    use color_eyre::eyre;

    #[test]
    fn parses_target_flag_separate_value() -> eyre::Result<()> {
        let d = RustcTargetDetector;
        let args = vec!["--target".to_string(), "wasm32-unknown-unknown".to_string()];
        let triple = d.detect_target(&args)?;
        assert_eq!(triple.as_str(), "wasm32-unknown-unknown");
        Ok(())
    }

    #[test]
    fn parses_target_flag_equals_form() -> eyre::Result<()> {
        let d = RustcTargetDetector;
        let args = vec!["--target=wasm32-unknown-unknown".to_string()];
        let triple = d.detect_target(&args)?;
        assert_eq!(triple.as_str(), "wasm32-unknown-unknown");
        Ok(())
    }

    #[test]
    #[allow(unsafe_code)]
    fn respects_cargo_build_target_env_var() -> eyre::Result<()> {
        let d = RustcTargetDetector;
        // SAFETY: test is single-threaded for this env var; cleaned up immediately.
        unsafe { std::env::set_var("CARGO_BUILD_TARGET", "aarch64-unknown-linux-gnu") };
        let result = d.detect_target(&Vec::new());
        unsafe { std::env::remove_var("CARGO_BUILD_TARGET") };
        assert_eq!(result?.as_str(), "aarch64-unknown-linux-gnu");
        Ok(())
    }

    #[test]
    #[allow(unsafe_code)]
    fn cli_target_takes_precedence_over_env_var() -> eyre::Result<()> {
        let d = RustcTargetDetector;
        // SAFETY: test is single-threaded for this env var; cleaned up immediately.
        unsafe { std::env::set_var("CARGO_BUILD_TARGET", "aarch64-unknown-linux-gnu") };
        let args = vec!["--target".to_string(), "wasm32-unknown-unknown".to_string()];
        let result = d.detect_target(&args);
        unsafe { std::env::remove_var("CARGO_BUILD_TARGET") };
        assert_eq!(result?.as_str(), "wasm32-unknown-unknown");
        Ok(())
    }
}
