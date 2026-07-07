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
#[derive(Debug, Default, Clone, Copy)]
pub struct RustcTargetEnvironment;

impl TargetEnvironment for RustcTargetEnvironment {
    fn cargo_build_target(&self) -> Option<String> {
        let triple = std::env::var("CARGO_BUILD_TARGET").ok()?;
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
/// # Errors
///
/// Returns an error when more than one explicit target is present. Cargo
/// accepts repeated targets for some commands, but cargo-fc currently plans
/// one explicit target; silently choosing one would make summaries misleading.
pub fn parse_cli_target(cargo_args: &[String]) -> eyre::Result<Option<String>> {
    let mut target = None;
    let mut it = cargo_args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            break;
        }
        if arg == "--target"
            && let Some(v) = it.next()
        {
            if v == "--" {
                break;
            }
            set_cli_target(&mut target, v.clone())?;
            continue;
        }
        if let Some(v) = arg.strip_prefix("--target=")
            && !v.is_empty()
        {
            set_cli_target(&mut target, v.to_string())?;
        }
    }
    Ok(target)
}

fn set_cli_target(target: &mut Option<String>, value: String) -> eyre::Result<()> {
    if let Some(existing) = target {
        eyre::bail!(
            "cargo-fc supports only one explicit --target at a time; got `{existing}` and `{value}`"
        );
    }
    *target = Some(value);
    Ok(())
}

#[cfg(test)]
mod test {
    use super::parse_cli_target;

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(String::from).collect()
    }

    #[test]
    fn parse_cli_target_separate_value() {
        assert_eq!(
            parse_cli_target(&owned(&["check", "--target", "wasm32-unknown-unknown"])).unwrap(),
            Some("wasm32-unknown-unknown".to_string())
        );
    }

    #[test]
    fn parse_cli_target_equals_form() {
        assert_eq!(
            parse_cli_target(&owned(&["check", "--target=wasm32-unknown-unknown"])).unwrap(),
            Some("wasm32-unknown-unknown".to_string())
        );
    }

    #[test]
    fn parse_cli_target_ignores_value_after_double_dash() {
        // `cargo fc run -- --target value-for-the-binary` must not be treated as
        // cargo's target triple.
        assert_eq!(
            parse_cli_target(&owned(&["run", "--", "--target", "binary-arg"])).unwrap(),
            None
        );
    }

    #[test]
    fn parse_cli_target_does_not_consume_double_dash_as_value() {
        assert_eq!(
            parse_cli_target(&owned(&["check", "--target", "--"])).unwrap(),
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
            ]))
            .unwrap(),
            Some("x86_64-unknown-linux-gnu".to_string())
        );
    }

    #[test]
    fn parse_cli_target_absent() {
        assert_eq!(
            parse_cli_target(&owned(&["check", "--all-features"])).unwrap(),
            None
        );
    }

    #[test]
    fn parse_cli_target_rejects_repeated_targets() {
        let err = parse_cli_target(&owned(&["check", "--target=a", "--target", "b"]))
            .expect_err("repeated targets should fail");

        assert!(err.to_string().contains("only one explicit --target"));
    }
}
