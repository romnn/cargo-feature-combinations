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

/// Read-only access to environment variables.
///
/// This trait abstracts environment variable lookups so that production code
/// uses the real process environment while tests can supply deterministic
/// values without data races.
pub trait Env {
    /// Look up an environment variable by name.
    fn var(&self, key: &str) -> Option<String>;
}

/// The real process environment (delegates to [`std::env::var`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Detect the effective compilation target for this invocation.
pub trait TargetDetector {
    /// Determine the effective target triple.
    ///
    /// # Errors
    ///
    /// Returns an error if the target triple cannot be determined.
    fn detect_target(&self, cargo_args: &[String]) -> eyre::Result<TargetTriple>;
}

/// Detect the compilation target using CLI flags, environment, and `rustc`.
///
/// Resolution order:
/// 1. `--target <triple>` CLI flag (authoritative)
/// 2. `CARGO_BUILD_TARGET` environment variable
/// 3. Host target via `rustc -vV`
#[derive(Debug, Clone)]
pub struct RustcTargetDetector<E = ProcessEnv> {
    env: E,
}

impl Default for RustcTargetDetector {
    fn default() -> Self {
        Self { env: ProcessEnv }
    }
}

impl<E: Env> RustcTargetDetector<E> {
    /// Create a detector with a custom environment.
    pub fn with_env(env: E) -> Self {
        Self { env }
    }

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

impl<E: Env> TargetDetector for RustcTargetDetector<E> {
    fn detect_target(&self, cargo_args: &[String]) -> eyre::Result<TargetTriple> {
        if let Some(triple) = Self::parse_target_flag(cargo_args) {
            return Ok(TargetTriple(triple));
        }
        if let Some(triple) = self.env.var("CARGO_BUILD_TARGET") {
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
    use super::{Env, RustcTargetDetector, TargetDetector};
    use color_eyre::eyre;
    use std::collections::HashMap;

    #[derive(Default)]
    struct TestEnv {
        vars: HashMap<String, String>,
    }

    impl Env for TestEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
    }

    #[test]
    fn parses_target_flag_separate_value() -> eyre::Result<()> {
        let d = RustcTargetDetector::default();
        let args = vec!["--target".to_string(), "wasm32-unknown-unknown".to_string()];
        let triple = d.detect_target(&args)?;
        assert_eq!(triple.as_str(), "wasm32-unknown-unknown");
        Ok(())
    }

    #[test]
    fn parses_target_flag_equals_form() -> eyre::Result<()> {
        let d = RustcTargetDetector::default();
        let args = vec!["--target=wasm32-unknown-unknown".to_string()];
        let triple = d.detect_target(&args)?;
        assert_eq!(triple.as_str(), "wasm32-unknown-unknown");
        Ok(())
    }

    #[test]
    fn respects_cargo_build_target_env_var() -> eyre::Result<()> {
        let env = TestEnv {
            vars: HashMap::from([(
                "CARGO_BUILD_TARGET".to_string(),
                "aarch64-unknown-linux-gnu".to_string(),
            )]),
        };
        let d = RustcTargetDetector::with_env(env);
        let triple = d.detect_target(&Vec::new())?;
        assert_eq!(triple.as_str(), "aarch64-unknown-linux-gnu");
        Ok(())
    }

    #[test]
    fn cli_target_takes_precedence_over_env_var() -> eyre::Result<()> {
        let env = TestEnv {
            vars: HashMap::from([(
                "CARGO_BUILD_TARGET".to_string(),
                "aarch64-unknown-linux-gnu".to_string(),
            )]),
        };
        let d = RustcTargetDetector::with_env(env);
        let args = vec!["--target".to_string(), "wasm32-unknown-unknown".to_string()];
        let triple = d.detect_target(&args)?;
        assert_eq!(triple.as_str(), "wasm32-unknown-unknown");
        Ok(())
    }

    #[test]
    fn empty_env_var_falls_through_to_host() -> eyre::Result<()> {
        let env = TestEnv {
            vars: HashMap::from([("CARGO_BUILD_TARGET".to_string(), "  ".to_string())]),
        };
        let d = RustcTargetDetector::with_env(env);
        // No --target flag, empty env var → should fall through to host triple.
        let triple = d.detect_target(&Vec::new())?;
        assert!(!triple.as_str().is_empty());
        Ok(())
    }

    #[test]
    fn missing_env_var_falls_through_to_host() -> eyre::Result<()> {
        let env = TestEnv::default();
        let d = RustcTargetDetector::with_env(env);
        let triple = d.detect_target(&Vec::new())?;
        assert!(!triple.as_str().is_empty());
        Ok(())
    }
}
