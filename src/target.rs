use color_eyre::eyre::{self, WrapErr};
use std::process::Command;

/// A Rust target triple.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TargetTriple(pub String);

impl TargetTriple {
    /// Borrow this target triple as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a package's effective target came from in the precedence chain.
///
/// Carried per package-target assignment so that injection and output
/// decisions stay local to the assignment instead of relying on plan-wide
/// boolean drift (`is_configured`, `should_inject`, `is_multi_target`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetSource {
    /// Explicit Cargo CLI `--target <triple>` (already forwarded to Cargo).
    Cli,
    /// Package-level `targets` list (cargo-fc must inject `--target`).
    PackageConfig,
    /// Workspace-level `targets` list (cargo-fc must inject `--target`).
    WorkspaceConfig,
    /// `CARGO_BUILD_TARGET` (Cargo sees the env var; no injection needed).
    CargoBuildTargetEnv,
    /// Host target from `rustc -vV` (keep current no-`--target` behavior).
    Host,
}

impl TargetSource {
    /// Whether cargo-fc must inject a `--target <triple>` flag for this source.
    #[must_use]
    pub fn should_inject_target_arg(self) -> bool {
        matches!(self, Self::PackageConfig | Self::WorkspaceConfig)
    }

    /// Whether this assignment came from configured target metadata.
    #[must_use]
    pub fn is_configured(self) -> bool {
        matches!(self, Self::PackageConfig | Self::WorkspaceConfig)
    }
}

/// A concrete target together with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveTarget {
    /// The concrete target triple.
    pub triple: TargetTriple,
    /// Where this target was selected from.
    pub source: TargetSource,
}

/// Adapter providing the ambient target environment for planning.
///
/// Abstracted behind a trait so that target planning is unit-testable without
/// reading the real environment or invoking `rustc`.
pub trait TargetEnvironment {
    /// The `CARGO_BUILD_TARGET` triple, if set and non-empty.
    fn cargo_build_target(&self) -> Option<String>;
    /// The host target triple from `rustc -vV`.
    ///
    /// # Errors
    ///
    /// Returns an error if the host target cannot be determined.
    fn host_target(&self) -> eyre::Result<TargetTriple>;
}

/// Production [`TargetEnvironment`] backed by the process environment and
/// `rustc -vV`.
#[derive(Debug, Clone)]
pub struct RustcTargetEnvironment<E = ProcessEnv> {
    env: E,
}

impl Default for RustcTargetEnvironment {
    fn default() -> Self {
        Self { env: ProcessEnv }
    }
}

impl<E: Env> RustcTargetEnvironment<E> {
    /// Create an environment adapter over a custom [`Env`].
    pub fn with_env(env: E) -> Self {
        Self { env }
    }
}

impl<E: Env> TargetEnvironment for RustcTargetEnvironment<E> {
    fn cargo_build_target(&self) -> Option<String> {
        let triple = self.env.var("CARGO_BUILD_TARGET")?;
        let triple = triple.trim();
        if triple.is_empty() {
            None
        } else {
            Some(triple.to_string())
        }
    }

    fn host_target(&self) -> eyre::Result<TargetTriple> {
        host_triple()
    }
}

/// Detect the host target triple via `rustc -vV`.
///
/// # Errors
///
/// Returns an error if `rustc` cannot be invoked or its output cannot be
/// parsed.
pub fn host_triple() -> eyre::Result<TargetTriple> {
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

/// Parse an explicit Cargo `--target <triple>` / `--target=<triple>` flag from
/// forwarded args, considering only arguments **before** `--`.
///
/// Arguments after `--` belong to the test binary or run target and must not
/// affect cargo-fc target planning, e.g. `cargo fc run -- --target value`.
#[must_use]
pub fn parse_cli_target(cargo_args: &[String]) -> Option<String> {
    let mut it = cargo_args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            break;
        }
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

    fn parse_target_flag(cargo_args: &[String]) -> Option<String> {
        parse_cli_target(cargo_args)
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
        host_triple()
    }
}

#[cfg(test)]
mod test {
    use super::{Env, RustcTargetDetector, TargetDetector, parse_cli_target};
    use color_eyre::eyre;
    use std::collections::HashMap;

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(String::from).collect()
    }

    #[test]
    fn parse_cli_target_separate_value() {
        assert_eq!(
            parse_cli_target(&owned(&["check", "--target", "wasm32-unknown-unknown"])),
            Some("wasm32-unknown-unknown".to_string())
        );
    }

    #[test]
    fn parse_cli_target_equals_form() {
        assert_eq!(
            parse_cli_target(&owned(&["check", "--target=wasm32-unknown-unknown"])),
            Some("wasm32-unknown-unknown".to_string())
        );
    }

    #[test]
    fn parse_cli_target_ignores_value_after_double_dash() {
        // `cargo fc run -- --target value-for-the-binary` must not be treated as
        // cargo's target triple.
        assert_eq!(
            parse_cli_target(&owned(&["run", "--", "--target", "binary-arg"])),
            None
        );
    }

    #[test]
    fn parse_cli_target_before_double_dash_still_parsed() {
        assert_eq!(
            parse_cli_target(&owned(&[
                "run",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--",
                "--target",
                "binary-arg",
            ])),
            Some("x86_64-unknown-linux-gnu".to_string())
        );
    }

    #[test]
    fn parse_cli_target_absent() {
        assert_eq!(parse_cli_target(&owned(&["check", "--all-features"])), None);
    }

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
    fn detector_ignores_target_flag_after_double_dash() -> eyre::Result<()> {
        let env = TestEnv {
            vars: HashMap::from([(
                "CARGO_BUILD_TARGET".to_string(),
                "aarch64-unknown-linux-gnu".to_string(),
            )]),
        };
        let d = RustcTargetDetector::with_env(env);
        let args = vec![
            "run".to_string(),
            "--".to_string(),
            "--target".to_string(),
            "binary-arg".to_string(),
        ];
        let triple = d.detect_target(&args)?;
        assert_eq!(triple.as_str(), "aarch64-unknown-linux-gnu");
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
