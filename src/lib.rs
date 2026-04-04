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
    ExitCode, color_spec, error_counts, print_feature_matrix, print_feature_matrix_for_target,
    print_summary, run_cargo_command, run_cargo_command_for_target, warning_counts,
};
pub use workspace::Workspace;

use crate::cfg_eval::RustcCfgEvaluator;
use crate::cli::cargo_subcommand;
use crate::runner::print_feature_combination_error;
use crate::target::{RustcTargetDetector, TargetDetector};

use color_eyre::eyre;
use std::process;

/// Key used to look up this tool's configuration in Cargo metadata.
pub(crate) const METADATA_KEY: &str = "cargo-feature-combinations";

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
        Some(Command::FeatureMatrix { pretty }) => print_feature_matrix_for_target(
            &packages,
            pretty,
            options.packages_only,
            &target,
            &mut evaluator,
        ),
        None => {
            if cargo_subcommand(cargo_args.as_slice()) == cli::CargoSubcommand::Other {
                eprintln!(
                    "warning: `cargo {bin_name}` only supports cargo's `build`, `test`, `run`, `check`, `doc`, and `clippy` subcommands",
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
