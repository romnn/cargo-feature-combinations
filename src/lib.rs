//! Run cargo commands for all feature combinations across a workspace.
//!
//! This crate powers the `cargo-fc` and `cargo-feature-combinations` binaries.
//! The main entry point for consumers is [`run`], which parses CLI arguments
//! and dispatches the requested command.
//!
//! The Rust API is an implementation detail of the CLI and has no stability
//! guarantees; the command-line interface is the supported interface.

/// Resolve cargo command aliases from the `.cargo/config.toml` hierarchy.
mod cargo_alias;
/// Evaluate Cargo-style `cfg(...)` expressions against a concrete target.
pub mod cfg_eval;
/// CLI argument parsing, options, and help text.
mod cli;
/// Configuration types and resolution logic for feature combination generation.
pub mod config;
/// Diagnostics-only output mode (JSON parsing and deduplication).
mod diagnostics_only;
/// Build-driver finalization.
mod driver;
/// User-facing hints and no-op warnings.
mod hints;
/// Feature implication graph and redundant-combination pruning.
pub mod implication;
/// Forwarded Cargo argument splitting and generated-argument placement.
mod invocation_args;
/// JSON matrix output from resolved execution plans.
pub mod matrix;
/// Package-level configuration, feature combination generation, and error types.
pub mod package;
/// Planning stages that prepare target and execution plans before Cargo runs.
pub mod plan;
/// Cargo command execution, output parsing, summary printing, and matrix output.
mod runner;
/// Target triple handling and host/flag based detection.
pub mod target;
/// Optional Rust target installation.
mod target_install;
/// IO utilities.
mod tee;
/// Workspace-level configuration and package discovery.
pub mod workspace;

pub use cfg_eval::CfgEvaluator;
pub use config::ResolvedFeatures;
pub use config::resolve::resolve_config;
pub use package::Package;
pub use target::TargetTriple;

use cfg_eval::RustcCfgEvaluator;
use cli::{Command, Options, cargo_subcommand, parse_arguments};
use color_eyre::eyre;
use invocation_args::GeneratedArgPlacement;
use itertools::Itertools;
use package::FeatureCombinationError;
use runner::{ExitCode, print_feature_combination_error};
use std::process;
use target::RustcTargetEnvironment;
use workspace::Workspace;

/// Yellow+bold color spec used by the [`print_warning!`] macro.
static WARNING_COLOR: std::sync::LazyLock<termcolor::ColorSpec> = std::sync::LazyLock::new(|| {
    let mut spec = termcolor::ColorSpec::new();
    spec.set_fg(Some(termcolor::Color::Yellow));
    spec.set_bold(true);
    spec
});

/// Cyan+bold color spec used by the [`print_note!`] macro.
static NOTE_COLOR: std::sync::LazyLock<termcolor::ColorSpec> = std::sync::LazyLock::new(|| {
    let mut spec = termcolor::ColorSpec::new();
    spec.set_fg(Some(termcolor::Color::Cyan));
    spec.set_bold(true);
    spec
});

macro_rules! print_labeled {
    ($label:literal, $color:expr, $($arg:tt)*) => {{
        use std::io::Write as _;
        use termcolor::WriteColor as _;
        let mut stderr = termcolor::StandardStream::stderr(termcolor::ColorChoice::Auto);
        let _ = stderr.set_color(&$color);
        let _ = write!(&mut stderr, $label);
        let _ = stderr.reset();
        let _ = writeln!(&mut stderr, ": {}", format_args!($($arg)*));
    }};
}
pub(crate) use print_labeled;

/// Print a colored warning to stderr.
///
/// Formats as `warning: <message>` with the `warning:` prefix in yellow.
/// Accepts the same arguments as [`format!`].
macro_rules! print_warning {
    ($($arg:tt)*) => {
        $crate::print_labeled!("warning", $crate::WARNING_COLOR, $($arg)*)
    };
}
pub(crate) use print_warning;

/// Print a colored informational note to stderr.
///
/// Formats as `note: <message>` with the `note:` prefix in cyan. Used for
/// non-fatal mode fallbacks/no-ops such as `--aggregate-targets` adjustments.
macro_rules! print_note {
    ($($arg:tt)*) => {
        $crate::print_labeled!("note", $crate::NOTE_COLOR, $($arg)*)
    };
}
pub(crate) use print_note;

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

/// Default dotted `package.metadata.<key>` path for per-package configuration
/// (no brackets; callers wrap it in `[...]`).
pub(crate) const DEFAULT_PKG_METADATA_SECTION: &str =
    concat!("package.metadata.", default_metadata_key!());

#[derive(Clone, Copy)]
struct CommandTokens<'a> {
    raw: Option<&'a str>,
    resolved: Option<&'a str>,
}

struct PreparedCargoCommand {
    args: Vec<String>,
    raw_token: Option<String>,
    resolved_token: Option<String>,
    cli_target: Option<String>,
    generated_arg_placement: GeneratedArgPlacement,
}

impl PreparedCargoCommand {
    fn tokens(&self) -> CommandTokens<'_> {
        CommandTokens {
            raw: self.raw_token.as_deref(),
            resolved: self.resolved_token.as_deref(),
        }
    }
}

struct CargoCommandDispatch<'a> {
    target_plans: &'a plan::targets::TargetPlans<'a>,
    options: &'a Options,
    cargo_args: Vec<&'a str>,
    tokens: CommandTokens<'a>,
    generated_arg_placement: GeneratedArgPlacement,
    workspace_config: &'a config::WorkspaceConfig,
    workspace_key: &'a str,
}

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

/// Format the dotted `package.metadata.<key>` path (no brackets).
///
/// Callers wrap it in `[...]` and may append a sub-key, e.g.
/// `[{pkg_metadata_section(key)}.target.'cfg(...)']`.
pub(crate) fn pkg_metadata_section(key: &str) -> String {
    format!("package.metadata.{key}")
}

/// Format the dotted `workspace.metadata.<key>` path (no brackets).
///
/// Callers wrap it in `[...]` and may append a sub-key, e.g.
/// `[{ws_metadata_section(key)}.subcommands.<token>]`.
pub(crate) fn ws_metadata_section(key: &str) -> String {
    format!("workspace.metadata.{key}")
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

    if options.command.is_none() && cargo_args.is_empty() {
        cli::print_help();
        return Ok(());
    }

    // Get metadata for cargo package
    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(ref manifest_path) = options.manifest_path {
        cmd.manifest_path(manifest_path);
    }
    let metadata = cmd.exec()?;

    let ws_config = metadata.workspace_config()?;
    // Discover candidate packages without applying workspace exclusions; those
    // (and their target-specific patches) are applied per target during
    // planning.
    let packages = select_candidate_packages(&metadata, &options)?;

    // Cache each selected package's base config once so planning and execution
    // never re-read the manifest (which would duplicate deprecation warnings).
    let configs: Vec<config::Config> = packages
        .iter()
        .map(|package| package.config())
        .collect::<eyre::Result<Vec<_>>>()?;

    let prepared = prepare_cargo_command(cargo_args, metadata.workspace_root.as_std_path())?;
    let tokens = prepared.tokens();
    let selected =
        selected_packages_for_target_planning(&packages, &configs, &options, &ws_config, tokens)?;

    // Echo the user's own metadata alias in capability hints/warnings.
    let ws_key = find_metadata_value(&metadata.workspace_metadata)
        .map_or(DEFAULT_METADATA_KEY, |(_, key)| key);
    hints::warn_if_configured_targets_ignored(
        &options,
        tokens.raw,
        tokens.resolved,
        &ws_config,
        ws_key,
        &selected,
    );

    let env = RustcTargetEnvironment;
    let mut evaluator = RustcCfgEvaluator::default();
    let base_exclude = metadata.base_workspace_exclude_packages()?;
    hints::warn_unmatched_config_exclude_packages(&base_exclude, &metadata);

    let expansion = match prepared.cli_target.as_deref() {
        Some(cli) => plan::targets::TargetExpansion::Explicit(cli),
        None if selected
            .iter()
            .any(|package| !package.ignore_configured_targets) =>
        {
            plan::targets::TargetExpansion::Configured
        }
        None => plan::targets::TargetExpansion::Denied,
    };
    let target_plans = plan::targets::build_target_plans(
        &selected,
        &ws_config,
        &base_exclude,
        plan::targets::TargetPlanRequest {
            expansion,
            raw_command: tokens.raw,
            resolved_command: tokens.resolved,
        },
        &env,
        &mut evaluator,
    )?;

    let result = if let Some(Command::FeatureMatrix { pretty }) = options.command {
        print_matrix_command(
            &target_plans,
            &options,
            &ws_config,
            tokens,
            pretty,
            &mut evaluator,
        )
    } else {
        run_cargo_command(
            CargoCommandDispatch {
                target_plans: &target_plans,
                options: &options,
                cargo_args: prepared.args.iter().map(String::as_str).collect(),
                tokens,
                generated_arg_placement: prepared.generated_arg_placement,
                workspace_config: &ws_config,
                workspace_key: ws_key,
            },
            &env,
            &mut evaluator,
        )
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

fn prepare_cargo_command(
    args: Vec<String>,
    workspace_root: &std::path::Path,
) -> eyre::Result<PreparedCargoCommand> {
    let raw_token = cli::cargo_subcommand_token(&args);
    let raw_cli_target = target::parse_cli_target(&args)?;
    // Resolve cargo command aliases so target policy and build-driver dispatch
    // see the underlying built-in subcommand when one is configured.
    let alias_expansion = cargo_alias::expand_aliases_with_info(args, workspace_root);
    let expanded_args = alias_expansion.args;
    let resolved_token = cli::cargo_subcommand_token(&expanded_args);
    // Expanded `run` aliases keep generated args after `--` only when the alias
    // body supplied that separator: `lint = "run --package wrapper -- lint"`.
    // A normal run alias plus user args (`serve = "run --package app"`,
    // invoked as `cargo fc serve -- arg`) still needs Cargo-side matrix args.
    let generated_arg_placement = if alias_expansion.expanded
        && matches!(
            cargo_subcommand(expanded_args.as_slice()),
            cli::CargoSubcommand::Run
        )
        && alias_expansion.alias_provided_double_dash
    {
        GeneratedArgPlacement::AliasWrapper
    } else {
        GeneratedArgPlacement::CargoCommand
    };
    // Target precedence is parsed from the user's raw command first, then from
    // the expanded command when generated args still belong to Cargo. For
    // wrapper aliases, an expanded `--target` configures the wrapper package
    // (`cargo run --target host -- ...`) and must not collapse cargo-fc's
    // configured target matrix.
    let expanded_cli_target = match generated_arg_placement {
        GeneratedArgPlacement::CargoCommand => target::parse_cli_target(&expanded_args)?,
        GeneratedArgPlacement::AliasWrapper => None,
    };
    let cli_target = raw_cli_target.or(expanded_cli_target);

    Ok(PreparedCargoCommand {
        args: expanded_args,
        raw_token,
        resolved_token,
        cli_target,
        generated_arg_placement,
    })
}

fn selected_packages_for_target_planning<'a>(
    packages: &[&'a cargo_metadata::Package],
    configs: &'a [config::Config],
    options: &Options,
    ws_config: &config::WorkspaceConfig,
    tokens: CommandTokens<'_>,
) -> eyre::Result<Vec<plan::targets::SelectedPackage<'a>>> {
    let command_token = tokens.resolved.or(tokens.raw);
    let default_target_capability = matches!(options.command, Some(Command::FeatureMatrix { .. }))
        || cli::builtin_target_capability(command_token);
    let default_diagnostics_allowed = cli::builtin_diagnostics_safe(command_token);

    let mut selected = Vec::new();
    for (package, package_config) in packages.iter().zip(configs) {
        // This resolution only decides target-selection capability; the resolved
        // driver is discarded, so the driver inputs are left unset.
        let command_config =
            config::Chain::base(ws_config, Some(package_config), tokens.raw, tokens.resolved)
                .resolve(
                    config::resolve::CliOverlay {
                        flags: options.flags,
                        driver: None,
                    },
                    config::resolve::ResolvePolicy {
                        default_diagnostics_allowed,
                        default_targets_enabled: default_target_capability,
                    },
                )?;
        selected.push(plan::targets::SelectedPackage {
            package,
            config: package_config,
            ignore_configured_targets: command_config.flags.no_targets
                || !command_config.targets_enabled,
            target_decision_explicit: command_config.flags.no_targets
                || command_config.targets_explicit,
        });
    }
    Ok(selected)
}

fn print_matrix_command(
    target_plans: &plan::targets::TargetPlans<'_>,
    options: &Options,
    workspace: &config::WorkspaceConfig,
    tokens: CommandTokens<'_>,
    pretty: bool,
    evaluator: &mut impl cfg_eval::CfgEvaluator,
) -> eyre::Result<ExitCode> {
    let context = plan::execution::PlanBuildContext {
        workspace_config: workspace,
        raw_command: tokens.raw,
        resolved_command: tokens.resolved,
        cli_driver: None,
        default_diagnostics_allowed: false,
        matrix: true,
    };
    let plan_set =
        plan::execution::build_execution_plans(target_plans, options.flags, &context, evaluator)?;
    hints::note_matrix_noop_flags(options);
    matrix::print_matrix_for_execution_plans(&plan_set, pretty)?;
    Ok(None)
}

fn run_cargo_command(
    dispatch: CargoCommandDispatch<'_>,
    env: &impl target::TargetEnvironment,
    evaluator: &mut impl cfg_eval::CfgEvaluator,
) -> eyre::Result<ExitCode> {
    let options = dispatch.options;
    let default_diagnostics_allowed =
        cli::builtin_diagnostics_safe(dispatch.tokens.resolved.or(dispatch.tokens.raw));
    let context = plan::execution::PlanBuildContext {
        workspace_config: dispatch.workspace_config,
        raw_command: dispatch.tokens.raw,
        resolved_command: dispatch.tokens.resolved,
        cli_driver: options.driver.as_deref(),
        default_diagnostics_allowed,
        matrix: false,
    };
    let mut plan_set = plan::execution::build_execution_plans(
        dispatch.target_plans,
        options.flags,
        &context,
        evaluator,
    )?;
    driver::finalize_plan_drivers(&mut plan_set, env)?;
    maybe_install_missing_targets(&plan_set, env, &dispatch.cargo_args)?;
    let mode = runner::resolve_execution_mode(
        &dispatch.cargo_args,
        &plan_set,
        dispatch.generated_arg_placement,
    );
    hints::warn_ignored_diagnostics_config(
        options,
        dispatch.tokens.raw,
        dispatch.tokens.resolved,
        dispatch.workspace_key,
        &plan_set,
    );
    runner::run_execution_plans(
        &plan_set,
        dispatch.cargo_args,
        mode,
        dispatch.generated_arg_placement,
    )
}

/// Discover candidate workspace packages and apply CLI-level package filters.
///
/// Workspace `exclude_packages` (and its target-specific patches) are applied
/// later, per target, by the planner — not here.
fn select_candidate_packages<'a>(
    metadata: &'a cargo_metadata::Metadata,
    options: &Options,
) -> eyre::Result<Vec<&'a cargo_metadata::Package>> {
    let mut packages = metadata.candidate_packages_for_fc()?;
    let available = packages
        .iter()
        .map(|package| package.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();

    let unknown_packages = options
        .packages
        .iter()
        .filter(|package| !available.contains(package.as_str()))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if !unknown_packages.is_empty() {
        eyre::bail!(
            "unknown package{} `{}`; available packages: {}",
            if unknown_packages.len() == 1 { "" } else { "s" },
            unknown_packages.iter().join("`, `"),
            available.iter().join(", "),
        );
    }

    for package in options
        .exclude_packages
        .iter()
        .filter(|package| !available.contains(package.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
    {
        print_warning!("excluded package `{package}` did not match any workspace member");
    }

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

    // Filter packages based on CLI options
    if !options.packages.is_empty() {
        packages.retain(|p| options.packages.contains(p.name.as_str()));
    }

    Ok(packages)
}

fn maybe_install_missing_targets(
    plan_set: &plan::execution::ExecutionPlanSet<'_>,
    env: &impl target::TargetEnvironment,
    cargo_args: &[&str],
) -> eyre::Result<()> {
    if plan_set.plans.iter().any(|plan| {
        plan.package_plans
            .iter()
            .any(|package_plan| package_plan.flags.install_missing_targets)
    }) {
        let installer =
            target_install::RustupTargetInstaller::new(cli::rustup_toolchain(cargo_args));
        target_install::ensure_missing_targets_installed(plan_set, env, &installer)?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::package::test::package as test_package;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use color_eyre::eyre;
    use serde_json::json;

    fn workspace_with_aliases(body: &str) -> eyre::Result<TempDir> {
        let tmp = TempDir::new()?;
        tmp.child(".cargo").create_dir_all()?;
        tmp.child(".cargo/config.toml").write_str(body)?;
        Ok(tmp)
    }

    fn execution_plan_set_with_flags<'a>(
        targets: &[&str],
        show_pruned: bool,
        package: &'a cargo_metadata::Package,
        flags: config::ResolvedFlags,
    ) -> plan::execution::ExecutionPlanSet<'a> {
        plan::execution::ExecutionPlanSet {
            plans: targets
                .iter()
                .map(|target| {
                    let target = target::TargetTriple((*target).to_string());
                    plan::execution::ExecutionPlan {
                        target: target.clone(),
                        package_plans: vec![plan::execution::PackageExecutionPlan {
                            package,
                            target: target::EffectiveTarget {
                                triple: target,
                                source: target::TargetSource::WorkspaceConfig,
                            },
                            combinations: Vec::new(),
                            pruned: Vec::new(),
                            matrix: serde_json::Map::new(),
                            flags,
                            driver: None,
                            ignored_diagnostics_config: false,
                        }],
                    }
                })
                .collect(),
            show_pruned,
            show_target: targets.len() > 1,
        }
    }

    fn target_selection_state(
        options: &Options,
        ws: &config::WorkspaceConfig,
        raw: Option<&str>,
        resolved: Option<&str>,
    ) -> eyre::Result<(bool, bool)> {
        let package = test_package("a")?;
        let config = config::Config::default();
        let packages = [&package];
        let configs = [config];
        let selected = selected_packages_for_target_planning(
            &packages,
            &configs,
            options,
            ws,
            CommandTokens { raw, resolved },
        )?;
        let [selected] = selected.as_slice() else {
            eyre::bail!("expected one selected package, got {}", selected.len());
        };
        Ok((
            selected.ignore_configured_targets,
            selected.target_decision_explicit,
        ))
    }

    #[test]
    fn prepare_cargo_command_marks_nested_run_wrapper_aliases() -> eyre::Result<()> {
        let workspace = workspace_with_aliases(
            r#"
            [alias]
            clippy-wrapper = "run --package clippy-wrapper --"
            lint = "clippy-wrapper lint"
            "#,
        )?;

        let prepared = prepare_cargo_command(vec!["lint".to_string()], workspace.path())?;

        // Nested aliases still need wrapper placement once the final expansion
        // is `cargo run ... -- <wrapped-command>`.
        assert_eq!(prepared.raw_token.as_deref(), Some("lint"));
        assert_eq!(prepared.resolved_token.as_deref(), Some("run"));
        assert_eq!(
            prepared.generated_arg_placement,
            GeneratedArgPlacement::AliasWrapper
        );
        assert_eq!(
            prepared.args,
            vec!["run", "--package", "clippy-wrapper", "--", "lint"],
        );
        Ok(())
    }

    #[test]
    fn prepare_cargo_command_keeps_direct_run_args_on_cargo_side() -> eyre::Result<()> {
        let workspace = workspace_with_aliases("[alias]\n")?;

        let prepared = prepare_cargo_command(
            vec!["run".to_string(), "--".to_string(), "lint".to_string()],
            workspace.path(),
        )?;

        // A user-provided `--` for direct `cargo run` is program argv, not an
        // alias wrapper boundary.
        assert_eq!(prepared.raw_token.as_deref(), Some("run"));
        assert_eq!(prepared.resolved_token.as_deref(), Some("run"));
        assert_eq!(
            prepared.generated_arg_placement,
            GeneratedArgPlacement::CargoCommand
        );
        Ok(())
    }

    #[test]
    fn prepare_cargo_command_keeps_run_alias_user_args_on_cargo_side() -> eyre::Result<()> {
        let workspace = workspace_with_aliases(
            r#"
            [alias]
            serve = "run --package app"
            "#,
        )?;

        let prepared = prepare_cargo_command(
            vec!["serve".to_string(), "--".to_string(), "lint".to_string()],
            workspace.path(),
        )?;

        // The alias did not provide `--`; the user's trailing args remain the
        // app's argv, so cargo-fc generated args still belong before `--`.
        assert_eq!(prepared.raw_token.as_deref(), Some("serve"));
        assert_eq!(prepared.resolved_token.as_deref(), Some("run"));
        assert_eq!(
            prepared.generated_arg_placement,
            GeneratedArgPlacement::CargoCommand
        );
        assert_eq!(prepared.args, vec!["run", "--package", "app", "--", "lint"]);
        Ok(())
    }

    #[test]
    fn prepare_cargo_command_preserves_run_alias_argument_position() -> eyre::Result<()> {
        let workspace = workspace_with_aliases(
            r#"
            [alias]
            serve = "run --package app -- serve"
            "#,
        )?;

        let prepared = prepare_cargo_command(vec!["serve".to_string()], workspace.path())?;

        // A `--` inside the alias body owns the argument boundary; generated
        // args must preserve that alias-defined position.
        assert_eq!(prepared.raw_token.as_deref(), Some("serve"));
        assert_eq!(prepared.resolved_token.as_deref(), Some("run"));
        assert_eq!(
            prepared.generated_arg_placement,
            GeneratedArgPlacement::AliasWrapper
        );
        assert_eq!(
            prepared.args,
            vec!["run", "--package", "app", "--", "serve"],
        );
        Ok(())
    }

    #[test]
    fn prepare_cargo_command_preserves_cli_target_for_run_wrapper_aliases() -> eyre::Result<()> {
        let workspace = workspace_with_aliases(
            r#"
            [alias]
            clippy-wrapper = "run --package clippy-wrapper --"
            lint = "clippy-wrapper lint"
            "#,
        )?;

        let prepared = prepare_cargo_command(
            vec![
                "lint".to_string(),
                "--target".to_string(),
                "wasm32-unknown-unknown".to_string(),
            ],
            workspace.path(),
        )?;

        // The user's explicit `--target` still wins even when the alias expands
        // into a wrapper command.
        assert_eq!(
            prepared.cli_target,
            Some("wasm32-unknown-unknown".to_string())
        );
        assert_eq!(
            prepared.generated_arg_placement,
            GeneratedArgPlacement::AliasWrapper
        );
        assert_eq!(
            prepared.args,
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--",
                "lint",
                "--target",
                "wasm32-unknown-unknown",
            ],
        );
        Ok(())
    }

    #[test]
    fn prepare_cargo_command_ignores_wrapper_cargo_target_for_target_planning() -> eyre::Result<()>
    {
        let workspace = workspace_with_aliases(
            r#"
            [alias]
            lint = "run --package clippy-wrapper --target x86_64-unknown-linux-gnu -- lint"
            "#,
        )?;

        let prepared = prepare_cargo_command(vec!["lint".to_string()], workspace.path())?;

        // The expanded `--target` configures the wrapper package, not the
        // command behind `--`, so target planning must ignore it.
        assert_eq!(prepared.cli_target, None);
        assert_eq!(
            prepared.generated_arg_placement,
            GeneratedArgPlacement::AliasWrapper
        );
        assert_eq!(
            prepared.args,
            vec![
                "run",
                "--package",
                "clippy-wrapper",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--",
                "lint",
            ],
        );
        Ok(())
    }

    #[test]
    fn prepare_cargo_command_reads_cli_target_from_expanded_alias() -> eyre::Result<()> {
        let workspace = workspace_with_aliases(
            r#"
            [alias]
            wasm-check = "check --target wasm32-unknown-unknown"
            "#,
        )?;

        let prepared = prepare_cargo_command(vec!["wasm-check".to_string()], workspace.path())?;

        // Non-wrapper aliases still expose their expanded Cargo `--target` as
        // an explicit target override.
        assert_eq!(
            prepared.cli_target,
            Some("wasm32-unknown-unknown".to_string())
        );
        assert_eq!(
            prepared.generated_arg_placement,
            GeneratedArgPlacement::CargoCommand
        );
        assert_eq!(
            prepared.args,
            vec!["check", "--target", "wasm32-unknown-unknown"],
        );
        Ok(())
    }

    #[test]
    fn aggregate_execution_mode_selected_for_supported_multi_target_command() -> eyre::Result<()> {
        let package = test_package("a")?;
        let flags = config::ResolvedFlags {
            aggregate_targets: true,
            ..config::ResolvedFlags::default()
        };
        let plan_set = execution_plan_set_with_flags(&["t1", "t2"], false, &package, flags);

        assert_eq!(
            runner::resolve_execution_mode(
                &["check"],
                &plan_set,
                GeneratedArgPlacement::CargoCommand
            ),
            runner::TargetExecutionMode::Aggregate
        );
        Ok(())
    }

    #[test]
    fn aggregate_execution_mode_falls_back_when_driver_differs_across_targets() -> eyre::Result<()>
    {
        // One package, two targets, different resolved drivers: aggregation would
        // batch both targets into one Cargo invocation, which cannot honor two
        // drivers — so it must fall back to serial per-target execution.
        let package = test_package("a")?;
        let flags = config::ResolvedFlags {
            aggregate_targets: true,
            ..config::ResolvedFlags::default()
        };
        let plan = |triple: &str, driver: Option<&str>| plan::execution::ExecutionPlan {
            target: target::TargetTriple(triple.to_string()),
            package_plans: vec![plan::execution::PackageExecutionPlan {
                package: &package,
                target: target::EffectiveTarget {
                    triple: target::TargetTriple(triple.to_string()),
                    source: target::TargetSource::WorkspaceConfig,
                },
                combinations: Vec::new(),
                pruned: Vec::new(),
                matrix: serde_json::Map::new(),
                flags,
                driver: driver.map(ToString::to_string),
                ignored_diagnostics_config: false,
            }],
        };
        let plan_set = plan::execution::ExecutionPlanSet {
            plans: vec![plan("t1", Some("cargo-zigbuild")), plan("t2", None)],
            show_pruned: false,
            show_target: true,
        };

        assert_eq!(
            runner::resolve_execution_mode(
                &["check"],
                &plan_set,
                GeneratedArgPlacement::CargoCommand
            ),
            runner::TargetExecutionMode::SerialPerTarget
        );
        Ok(())
    }

    #[test]
    fn aggregate_execution_mode_falls_back_for_run() -> eyre::Result<()> {
        let package = test_package("a")?;
        let flags = config::ResolvedFlags {
            aggregate_targets: true,
            ..config::ResolvedFlags::default()
        };
        let plan_set = execution_plan_set_with_flags(&["t1", "t2"], false, &package, flags);

        assert_eq!(
            runner::resolve_execution_mode(
                &["run"],
                &plan_set,
                GeneratedArgPlacement::CargoCommand
            ),
            runner::TargetExecutionMode::SerialPerTarget
        );
        Ok(())
    }

    #[test]
    fn aggregate_execution_mode_allows_run_wrapper_aliases() -> eyre::Result<()> {
        let package = test_package("a")?;
        let flags = config::ResolvedFlags {
            aggregate_targets: true,
            ..config::ResolvedFlags::default()
        };
        let plan_set = execution_plan_set_with_flags(&["t1", "t2"], false, &package, flags);

        assert_eq!(
            runner::resolve_execution_mode(
                &["run", "--package", "clippy-wrapper", "--", "lint"],
                &plan_set,
                GeneratedArgPlacement::AliasWrapper,
            ),
            runner::TargetExecutionMode::Aggregate
        );
        Ok(())
    }

    #[test]
    fn aggregate_execution_mode_falls_back_for_pruned_summaries() -> eyre::Result<()> {
        let package = test_package("a")?;
        let flags = config::ResolvedFlags {
            aggregate_targets: true,
            ..config::ResolvedFlags::default()
        };
        let plan_set = execution_plan_set_with_flags(&["t1", "t2"], true, &package, flags);

        assert_eq!(
            runner::resolve_execution_mode(
                &["check"],
                &plan_set,
                GeneratedArgPlacement::CargoCommand
            ),
            runner::TargetExecutionMode::SerialPerTarget
        );
        Ok(())
    }

    #[test]
    fn aggregate_execution_mode_is_noop_for_single_target() -> eyre::Result<()> {
        let package = test_package("a")?;
        let flags = config::ResolvedFlags {
            aggregate_targets: true,
            ..config::ResolvedFlags::default()
        };
        let plan_set = execution_plan_set_with_flags(&["t1"], false, &package, flags);

        assert_eq!(
            runner::resolve_execution_mode(
                &["check"],
                &plan_set,
                GeneratedArgPlacement::CargoCommand
            ),
            runner::TargetExecutionMode::SerialPerTarget
        );
        Ok(())
    }

    #[test]
    fn no_targets_flag_disables_configured_targets() -> eyre::Result<()> {
        let options = Options {
            flags: config::FlagConfig {
                no_targets: Some(true),
                ..config::FlagConfig::default()
            },
            ..Options::default()
        };
        let ws = config::WorkspaceConfig::default();
        let (ignore_configured_targets, target_decision_explicit) =
            target_selection_state(&options, &ws, Some("check"), Some("check"))?;

        assert!(ignore_configured_targets);
        assert!(target_decision_explicit);
        Ok(())
    }

    #[test]
    fn builtin_command_allows_capability_without_no_targets() -> eyre::Result<()> {
        let options = Options::default();
        let ws = config::WorkspaceConfig::default();
        let (ignore_configured_targets, target_decision_explicit) =
            target_selection_state(&options, &ws, Some("check"), Some("check"))?;

        assert!(!ignore_configured_targets);
        assert!(!target_decision_explicit);
        Ok(())
    }

    #[test]
    fn builtin_command_can_be_disabled_by_workspace_policy() -> eyre::Result<()> {
        let options = Options::default();
        let mut ws = config::WorkspaceConfig::default();
        ws.base.subcommands.insert(
            "build".to_string(),
            config::CommandCapabilities {
                expand_targets: Some(false),
                ..config::CommandCapabilities::default()
            },
        );
        let (ignore_configured_targets, target_decision_explicit) =
            target_selection_state(&options, &ws, Some("build"), Some("build"))?;

        assert!(ignore_configured_targets);
        assert!(target_decision_explicit);
        Ok(())
    }

    #[test]
    fn package_subcommand_replace_resets_broader_expand_targets_policy() -> eyre::Result<()> {
        let options = Options::default();
        let mut ws = config::WorkspaceConfig::default();
        ws.base.subcommands.insert(
            "build".to_string(),
            config::CommandCapabilities {
                expand_targets: Some(false),
                ..config::CommandCapabilities::default()
            },
        );
        let mut config = config::Config::default();
        config.base.subcommands.insert(
            "build".to_string(),
            config::CommandCapabilities {
                replace: true,
                ..config::CommandCapabilities::default()
            },
        );
        let package = test_package("a")?;
        let packages = [&package];
        let configs = [config];
        let selected = selected_packages_for_target_planning(
            &packages,
            &configs,
            &options,
            &ws,
            CommandTokens {
                raw: Some("build"),
                resolved: Some("build"),
            },
        )?;
        let [selected] = selected.as_slice() else {
            eyre::bail!("expected one selected package, got {}", selected.len());
        };

        assert!(!selected.ignore_configured_targets);
        assert!(!selected.target_decision_explicit);
        Ok(())
    }

    #[test]
    fn resolved_alias_inherits_builtin_capability_by_default() -> eyre::Result<()> {
        let options = Options::default();
        let ws = config::WorkspaceConfig::default();
        let (ignore_configured_targets, target_decision_explicit) =
            target_selection_state(&options, &ws, Some("lint"), Some("clippy"))?;

        assert!(!ignore_configured_targets);
        assert!(!target_decision_explicit);
        Ok(())
    }

    #[test]
    fn explicit_alias_policy_wins_over_resolved_builtin_policy() -> eyre::Result<()> {
        let options = Options::default();
        let mut ws = config::WorkspaceConfig::default();
        ws.base.subcommands.insert(
            "lint".to_string(),
            config::CommandCapabilities {
                expand_targets: Some(false),
                ..config::CommandCapabilities::default()
            },
        );
        let (ignore_configured_targets, target_decision_explicit) =
            target_selection_state(&options, &ws, Some("lint"), Some("clippy"))?;

        assert!(ignore_configured_targets);
        assert!(target_decision_explicit);
        Ok(())
    }

    #[test]
    fn explicit_alias_policy_can_enable_unresolved_expanded_command() -> eyre::Result<()> {
        let options = Options::default();
        let mut ws = config::WorkspaceConfig::default();
        ws.base.subcommands.insert(
            "lint".to_string(),
            config::CommandCapabilities {
                expand_targets: Some(true),
                ..config::CommandCapabilities::default()
            },
        );
        let (ignore_configured_targets, target_decision_explicit) =
            target_selection_state(&options, &ws, Some("lint"), Some("custom-wrapper"))?;

        assert!(!ignore_configured_targets);
        assert!(target_decision_explicit);
        Ok(())
    }

    #[test]
    fn no_targets_flag_disables_configured_targets_for_matrix() -> eyre::Result<()> {
        let options = Options {
            command: Some(Command::FeatureMatrix { pretty: false }),
            flags: config::FlagConfig {
                no_targets: Some(true),
                ..config::FlagConfig::default()
            },
            ..Options::default()
        };
        let ws = config::WorkspaceConfig::default();
        let (ignore_configured_targets, target_decision_explicit) =
            target_selection_state(&options, &ws, None, None)?;

        assert!(ignore_configured_targets);
        assert!(target_decision_explicit);
        Ok(())
    }

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
            "package.metadata.cargo-fc"
        );
        assert_eq!(pkg_metadata_section("fc"), "package.metadata.fc");
    }

    #[test]
    fn ws_metadata_section_formats_correctly() {
        assert_eq!(
            ws_metadata_section("cargo-fc"),
            "workspace.metadata.cargo-fc"
        );
    }

    #[test]
    fn default_metadata_key_is_cargo_fc() {
        assert_eq!(DEFAULT_METADATA_KEY, "cargo-fc");
    }

    #[test]
    fn default_pkg_metadata_section_uses_default_key() {
        assert_eq!(DEFAULT_PKG_METADATA_SECTION, "package.metadata.cargo-fc");
    }
}
