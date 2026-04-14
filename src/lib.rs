//! Run cargo commands for all feature combinations across a workspace.
//!
//! This crate powers the `cargo-fc` and `cargo-feature-combinations` binaries.
//! The main entry point for consumers is [`run`], which parses CLI arguments
//! and dispatches the requested command.

/// Evaluate Cargo-style `cfg(...)` expressions against a concrete target.
pub mod cfg_eval;
/// CLI argument parsing, options, and help text.
pub mod cli;
/// Configuration types and resolution logic for feature combination generation.
pub mod config;
/// Diagnostics-only output mode (JSON parsing and deduplication).
pub mod diagnostics_only;
/// Feature implication graph and redundant-combination pruning.
pub mod implication;
/// Package-level configuration, feature combination generation, and error types.
pub mod package;
/// Cargo command execution, output parsing, summary printing, and matrix output.
pub mod runner;
/// Target triple handling and host/flag based detection.
pub mod target;
/// IO utilities.
pub mod tee;
/// Workspace-level configuration and package discovery.
pub mod workspace;

pub use cli::{ArgumentParser, Command, Options, parse_arguments};
pub use package::{FeatureCombinationError, Package};
pub use runner::{
    ExitCode, MatrixOptions, color_spec, error_counts, print_feature_matrix_for_target,
    print_summary, run_cargo_command_for_target, warning_counts,
};
pub use workspace::Workspace;

use cfg_eval::RustcCfgEvaluator;
use cli::cargo_subcommand;
use color_eyre::eyre;
use runner::print_feature_combination_error;
use std::process;
use target::{RustcTargetDetector, TargetDetector};

/// Yellow+bold color spec used by the [`print_warning!`] macro.
static WARNING_COLOR: std::sync::LazyLock<termcolor::ColorSpec> = std::sync::LazyLock::new(|| {
    let mut spec = termcolor::ColorSpec::new();
    spec.set_fg(Some(termcolor::Color::Yellow));
    spec.set_bold(true);
    spec
});

/// Print a colored warning to stderr.
///
/// Formats as `warning: <message>` with the `warning:` prefix in yellow.
/// Accepts the same arguments as [`format!`].
macro_rules! print_warning {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        use termcolor::WriteColor as _;
        let mut stderr = termcolor::StandardStream::stderr(termcolor::ColorChoice::Auto);
        let _ = stderr.set_color(&$crate::WARNING_COLOR);
        let _ = write!(&mut stderr, "warning");
        let _ = stderr.reset();
        let _ = writeln!(&mut stderr, ": {}", format_args!($($arg)*));
    }};
}
pub(crate) use print_warning;

/// Whether to warn when the cargo subcommand is not one of the known commands
/// (`build`, `test`, `run`, `check`, `doc`, `clippy`). Disabled by default
/// because cargo aliases (e.g. `cargo lint`) are common and the tool handles
/// unknown subcommands gracefully via best-effort output parsing.
const WARN_UNKNOWN_SUBCOMMAND: bool = false;

/// Expands to the default metadata key literal.
macro_rules! default_metadata_key {
    () => {
        "cargo-fc"
    };
}

/// All recognized metadata key aliases, tried in order during lookup.
///
/// Longest (most explicit) keys come first so that when a manifest
/// contains more than one alias the most specific one wins.
pub(crate) const METADATA_KEYS: &[&str] = &[
    "cargo-feature-combinations",
    "feature-combinations",
    "cargo-fc",
    "fc",
];

/// Default metadata key used in hints and help text when no existing
/// usage is detected.
pub(crate) const DEFAULT_METADATA_KEY: &str = default_metadata_key!();

/// Default TOML section header for per-package configuration.
pub(crate) const DEFAULT_PKG_METADATA_SECTION: &str =
    concat!("[package.metadata.", default_metadata_key!(), "]");

/// Look up configuration from any recognized metadata key alias.
///
/// Returns the first matching value and the alias that matched, or
/// `None` if none of the aliases are present.
pub(crate) fn find_metadata_value(
    metadata: &serde_json::Value,
) -> Option<(&serde_json::Value, &'static str)> {
    for &key in METADATA_KEYS {
        if let Some(value) = metadata.get(key) {
            return Some((value, key));
        }
    }
    None
}

/// Format a `[package.metadata.<key>]` TOML section header.
pub(crate) fn pkg_metadata_section(key: &str) -> String {
    format!("[package.metadata.{key}]")
}

/// Format a `[workspace.metadata.<key>]` TOML section header.
pub(crate) fn ws_metadata_section(key: &str) -> String {
    format!("[workspace.metadata.{key}]")
}

/// Run the cargo subcommand for all relevant feature combinations.
///
/// This is the main entry point used by the binaries in this crate.
///
/// # Errors
///
/// Returns an error if argument parsing fails or `cargo metadata` can not be
/// executed successfully.
pub fn run(bin_name: &str) -> eyre::Result<()> {
    color_eyre::install()?;

    let (options, cargo_args) = parse_arguments(bin_name)?;

    if let Some(Command::Help) = options.command {
        cli::print_help();
        return Ok(());
    }

    if let Some(Command::Version) = options.command {
        println!("cargo-{bin_name} v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Get metadata for cargo package
    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(ref manifest_path) = options.manifest_path {
        cmd.manifest_path(manifest_path);
    }
    let metadata = cmd.exec()?;
    let mut packages = metadata.packages_for_fc()?;

    // When `--manifest-path` points to a workspace member, `cargo metadata`
    // still returns the entire workspace. Unless the user explicitly selected
    // packages via `-p/--package`, default to only processing the root package
    // resolved by Cargo for the given manifest.
    if options.manifest_path.is_some()
        && options.packages.is_empty()
        && let Some(root) = metadata.root_package()
    {
        packages.retain(|p| p.id == root.id);
    }

    // Filter excluded packages via CLI arguments
    packages.retain(|p| !options.exclude_packages.contains(p.name.as_str()));

    if options.only_packages_with_lib_target {
        // Filter only packages with a library target
        packages.retain(|p| {
            p.targets
                .iter()
                .any(|t| t.kind.contains(&cargo_metadata::TargetKind::Lib))
        });
    }

    // Filter packages based on CLI options
    if !options.packages.is_empty() {
        packages.retain(|p| options.packages.contains(p.name.as_str()));
    }

    // Preserve the original String args for `--target` detection.
    let cargo_args_owned = cargo_args;
    let cargo_args: Vec<&str> = cargo_args_owned.iter().map(String::as_str).collect();

    let detector = RustcTargetDetector::default();
    let target = detector.detect_target(&cargo_args_owned)?;
    let mut evaluator = RustcCfgEvaluator::default();
    let result = match options.command {
        Some(Command::Help | Command::Version) => Ok(None),
        Some(Command::FeatureMatrix { pretty }) => {
            let matrix_opts = runner::MatrixOptions {
                pretty,
                packages_only: options.packages_only,
                no_prune_implied: options.no_prune_implied,
            };
            print_feature_matrix_for_target(&packages, &target, &mut evaluator, &matrix_opts)
        }
        None => {
            if WARN_UNKNOWN_SUBCOMMAND
                && cargo_subcommand(cargo_args.as_slice()) == cli::CargoSubcommand::Other
            {
                print_warning!(
                    "`cargo {bin_name}` only supports cargo's `build`, `test`, `run`, `check`, `doc`, and `clippy` subcommands"
                );
            }
            run_cargo_command_for_target(&packages, cargo_args, &options, &target, &mut evaluator)
        }
    };

    match result {
        Ok(Some(exit_code)) => process::exit(exit_code),
        Ok(None) => Ok(()),
        Err(err) => {
            if let Some(e) = err.downcast_ref::<FeatureCombinationError>() {
                print_feature_combination_error(e);
                process::exit(2);
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::eyre;
    use serde_json::json;

    #[test]
    fn find_metadata_value_returns_none_for_empty_object() {
        let meta = json!({});
        assert!(find_metadata_value(&meta).is_none());
    }

    #[test]
    fn find_metadata_value_returns_none_for_unrelated_keys() {
        let meta = json!({ "other-tool": { "key": "value" } });
        assert!(find_metadata_value(&meta).is_none());
    }

    #[test]
    fn find_metadata_value_finds_each_alias() -> eyre::Result<()> {
        for &alias in METADATA_KEYS {
            let meta = json!({ alias: { "exclude_features": ["default"] } });
            let (value, matched) =
                find_metadata_value(&meta).ok_or_else(|| eyre::eyre!("no match for {alias}"))?;
            assert_eq!(matched, alias);
            assert!(value.get("exclude_features").is_some());
        }
        Ok(())
    }

    #[test]
    fn find_metadata_value_prefers_longest_alias() -> eyre::Result<()> {
        let meta = json!({
            "cargo-feature-combinations": { "source": "long" },
            "fc": { "source": "short" },
        });
        let (value, matched) = find_metadata_value(&meta).ok_or_else(|| eyre::eyre!("no match"))?;
        assert_eq!(matched, "cargo-feature-combinations");
        assert_eq!(value["source"], "long");
        Ok(())
    }

    #[test]
    fn find_metadata_value_prefers_cargo_fc_over_fc() -> eyre::Result<()> {
        let meta = json!({
            "cargo-fc": { "source": "cargo-fc" },
            "fc": { "source": "fc" },
        });
        let (_, matched) = find_metadata_value(&meta).ok_or_else(|| eyre::eyre!("no match"))?;
        assert_eq!(matched, "cargo-fc");
        Ok(())
    }

    #[test]
    fn pkg_metadata_section_formats_correctly() {
        assert_eq!(
            pkg_metadata_section("cargo-fc"),
            "[package.metadata.cargo-fc]"
        );
        assert_eq!(pkg_metadata_section("fc"), "[package.metadata.fc]");
    }

    #[test]
    fn ws_metadata_section_formats_correctly() {
        assert_eq!(
            ws_metadata_section("cargo-fc"),
            "[workspace.metadata.cargo-fc]"
        );
    }

    #[test]
    fn default_metadata_key_is_cargo_fc() {
        assert_eq!(DEFAULT_METADATA_KEY, "cargo-fc");
    }

    #[test]
    fn default_pkg_metadata_section_uses_default_key() {
        assert_eq!(DEFAULT_PKG_METADATA_SECTION, "[package.metadata.cargo-fc]");
    }
}
