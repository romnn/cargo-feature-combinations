//! Run cargo commands for all feature combinations across a workspace.
//!
//! This crate powers the `cargo-fc` and `cargo-feature-combinations` binaries.
//! The main entry point for consumers is [`run`], which parses CLI arguments
//! and dispatches the requested command.

/// Resolve cargo command aliases from the `.cargo/config.toml` hierarchy.
mod cargo_alias;
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
/// Optional Rust target installation.
mod target_install;
/// Target selection and target-plan construction.
pub mod target_plan;
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
use target::RustcTargetEnvironment;

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

/// Print a colored informational note to stderr.
///
/// Formats as `note: <message>` with the `note:` prefix in cyan. Used for
/// non-fatal mode fallbacks/no-ops such as `--aggregate-targets` adjustments.
macro_rules! print_note {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        use termcolor::WriteColor as _;
        let mut stderr = termcolor::StandardStream::stderr(termcolor::ColorChoice::Auto);
        let _ = stderr.set_color(&$crate::NOTE_COLOR);
        let _ = write!(&mut stderr, "note");
        let _ = stderr.reset();
        let _ = writeln!(&mut stderr, ": {}", format_args!($($arg)*));
    }};
}
pub(crate) use print_note;

/// Whether to warn when the cargo subcommand is not one of the known commands
/// (`build`, `test`, `run`, `check`, `doc`, `clippy`). Disabled by default
/// because cargo aliases and custom subcommands are common and the tool handles
/// unresolved commands gracefully via best-effort output parsing.
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

/// Default dotted `package.metadata.<key>` path for per-package configuration
/// (no brackets; callers wrap it in `[...]`).
pub(crate) const DEFAULT_PKG_METADATA_SECTION: &str =
    concat!("package.metadata.", default_metadata_key!());

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
    let selected: Vec<target_plan::SelectedPackage<'_>> = packages
        .iter()
        .zip(&configs)
        .map(|(package, config)| target_plan::SelectedPackage { package, config })
        .collect();

    let raw_subcommand_token = cli::cargo_subcommand_token(&cargo_args);
    // Resolve cargo command aliases (e.g. `lint` → `clippy --all-targets --no-deps`) so the
    // underlying built-in subcommand is visible to the target-capability registry and the build
    // driver. Keep the resolved String args for `--target` detection.
    let cargo_args_owned =
        cargo_alias::expand_aliases(cargo_args, metadata.workspace_root.as_std_path());
    let resolved_subcommand_token = cli::cargo_subcommand_token(&cargo_args_owned);
    let cargo_args: Vec<&str> = cargo_args_owned.iter().map(String::as_str).collect();

    // Parse an explicit `--target` only before `--`.
    let cli_target = target::parse_cli_target(&cargo_args_owned);

    // Echo the user's own metadata alias in capability hints/warnings.
    let ws_key = find_metadata_value(&metadata.workspace_metadata)
        .map_or(DEFAULT_METADATA_KEY, |(_, key)| key);
    let capability_allowed = resolve_capability_and_warn(
        &options,
        raw_subcommand_token.as_deref(),
        resolved_subcommand_token.as_deref(),
        &ws_config,
        ws_key,
        &selected,
    );

    let env = RustcTargetEnvironment;
    let mut evaluator = RustcCfgEvaluator::default();
    let base_exclude = metadata.base_workspace_exclude_packages()?;

    let target_plans = target_plan::build_target_plans(
        &selected,
        &ws_config,
        &base_exclude,
        cli_target.as_deref(),
        capability_allowed,
        &env,
        &mut evaluator,
    )?;

    let result = match options.command {
        Some(Command::Help | Command::Version) => Ok(None),
        Some(Command::FeatureMatrix { pretty }) => {
            let plan_set = runner::build_execution_plans(
                &target_plans,
                &options,
                options.packages_only,
                &mut evaluator,
            )?;
            note_matrix_noop_flags(&options);
            let matrix_opts = runner::MatrixOptions {
                pretty,
                packages_only: options.packages_only,
                no_prune_implied: options.no_prune_implied,
            };
            runner::print_matrix_for_execution_plans(&plan_set, &matrix_opts)
        }
        None => {
            if WARN_UNKNOWN_SUBCOMMAND
                && cargo_subcommand(cargo_args.as_slice()) == cli::CargoSubcommand::Other
            {
                print_warning!(
                    "`cargo {bin_name}` only supports cargo's `build`, `test`, `run`, `check`, `doc`, and `clippy` subcommands"
                );
            }
            let plan_set =
                runner::build_execution_plans(&target_plans, &options, false, &mut evaluator)?;
            maybe_install_missing_targets(&options, &ws_config, &plan_set, &env, &cargo_args)?;
            let mode = resolve_execution_mode(&options, &cargo_args, &plan_set);
            let driver = resolve_driver(&options, &ws_config, &plan_set, &env)?;
            runner::run_execution_plans(&plan_set, cargo_args, &options, mode, driver.as_deref())
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

/// Discover candidate workspace packages and apply CLI-level package filters.
///
/// Workspace `exclude_packages` (and its target-specific patches) are applied
/// later, per target, by the planner — not here.
fn select_candidate_packages<'a>(
    metadata: &'a cargo_metadata::Metadata,
    options: &Options,
) -> eyre::Result<Vec<&'a cargo_metadata::Package>> {
    let mut packages = metadata.candidate_packages_for_fc()?;

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

    Ok(packages)
}

fn maybe_install_missing_targets(
    options: &Options,
    ws_config: &config::WorkspaceConfig,
    plan_set: &runner::ExecutionPlanSet<'_>,
    env: &impl target::TargetEnvironment,
    cargo_args: &[&str],
) -> eyre::Result<()> {
    if options.install_missing_targets || ws_config.install_missing_targets {
        let installer =
            target_install::RustupTargetInstaller::new(cli::rustup_toolchain(cargo_args));
        target_install::ensure_missing_targets_installed(plan_set, env, &installer)?;
    }
    Ok(())
}

/// Resolve the selected command's target capability and warn (once) if
/// configured targets exist but the command can not accept them.
///
/// `matrix` is not a forwarded cargo command: it always uses configured target
/// planning. The warning is driven from raw config state, not from the planned
/// targets after capability filtering.
fn resolve_capability_and_warn(
    options: &Options,
    raw_token: Option<&str>,
    resolved_token: Option<&str>,
    ws_config: &config::WorkspaceConfig,
    ws_key: &str,
    selected: &[target_plan::SelectedPackage<'_>],
) -> bool {
    // `--no-targets` deliberately ignores configured target lists and falls back
    // to Cargo's default single target, so deny without warning.
    if options.no_targets {
        return false;
    }

    // `matrix` is not a forwarded cargo command: it always uses configured
    // target planning.
    if matches!(options.command, Some(Command::FeatureMatrix { .. })) {
        return true;
    }

    let raw_policy = cli::configured_target_policy(raw_token, &ws_config.subcommand_overrides);
    let (policy, warning_token) = if raw_policy.explicit {
        (raw_policy, raw_token)
    } else {
        (
            cli::configured_target_policy(resolved_token, &ws_config.subcommand_overrides),
            raw_token.or(resolved_token),
        )
    };
    if policy.enabled {
        return true;
    }

    // Capability denied: warn (once) only when the user actually configured
    // targets that we are now skipping.
    let has_raw_configured_targets = !ws_config.workspace_targets.is_empty()
        || selected.iter().any(|s| {
            s.config
                .package_targets
                .as_ref()
                .is_some_and(|t| !t.is_empty())
        });
    if has_raw_configured_targets
        && !policy.explicit
        && let Some(token) = warning_token.filter(|t| !t.is_empty())
    {
        print_warning!(
            "not passing configured targets to cargo command `{token}` because it has no targets capability"
        );
        eprintln!(
            "hint: add [{}.subcommands.{token}] targets = true if this command accepts --target",
            ws_metadata_section(ws_key),
        );
    }

    false
}

/// Emit one note per run-only flag that has no effect on `cargo fc matrix`
/// output, so the silent no-op is visible to the user.
fn note_matrix_noop_flags(options: &Options) {
    if options.install_missing_targets {
        print_note!(
            "--install-missing-targets has no effect for matrix output; matrix only prints planned targets"
        );
    }
    if options.aggregate_targets {
        print_note!(
            "--aggregate-targets has no effect for matrix output; matrix rows are always per target"
        );
    }
    if options.driver.is_some() {
        print_note!("--driver has no effect for matrix output; matrix only prints planned targets");
    }
}

/// Resolve the build driver used to spawn each combination.
///
/// An explicit `--driver` or `[workspace.metadata.cargo-fc].driver` always wins.
/// Otherwise cargo-fc defaults to `cargo-zigbuild` when any non-host target is
/// planned — so crates with native-C build dependencies cross-compile via zig —
/// and to plain `cargo` (`None`, i.e. `$CARGO`) for host-only runs. Users who
/// want a different wrapper, or plain `cargo` even when cross-compiling, set
/// `driver` explicitly.
fn resolve_driver(
    options: &Options,
    ws_config: &config::WorkspaceConfig,
    plan_set: &runner::ExecutionPlanSet,
    env: &impl target::TargetEnvironment,
) -> eyre::Result<Option<String>> {
    if let Some(driver) = &options.driver {
        return normalize_driver(driver, "--driver");
    }
    if let Some(driver) = &ws_config.driver {
        return normalize_driver(driver, "[workspace.metadata.cargo-fc].driver");
    }
    if plan_set.plans.is_empty() {
        return Ok(None);
    }
    // Detecting the host is only needed to decide whether any planned target is a
    // cross target. If that fails, fall back to plain `cargo` (the conservative
    // default) instead of aborting the whole run, mirroring how missing-target
    // installation degrades on the same failure.
    let host = match env.host_target() {
        Ok(host) => host,
        Err(err) => {
            print_warning!(
                "could not detect host target to select a build driver: {err}; using plain cargo"
            );
            return Ok(None);
        }
    };
    let cross = plan_set.plans.iter().any(|plan| plan.target != host);
    if cross {
        Ok(Some("cargo-zigbuild".to_string()))
    } else {
        Ok(None)
    }
}

fn normalize_driver(driver: &str, source: &str) -> eyre::Result<Option<String>> {
    let driver = driver.trim();
    if driver.is_empty() {
        eyre::bail!("{source} must not be empty");
    }
    // `driver = "cargo"` selects plain Cargo; resolve it to `None` so the spawn
    // still honors `$CARGO` (e.g. a rustup or CI override), matching the default
    // host-only path rather than forcing the literal `cargo` on `PATH`.
    if driver == "cargo" {
        Ok(None)
    } else {
        Ok(Some(driver.to_string()))
    }
}

/// Resolve the effective target execution mode, emitting a note when an
/// explicitly requested `--aggregate-targets` falls back to serial or is a
/// no-op.
fn resolve_execution_mode(
    options: &Options,
    cargo_args: &[&str],
    plan_set: &runner::ExecutionPlanSet<'_>,
) -> runner::TargetExecutionMode {
    use runner::TargetExecutionMode;

    if !options.aggregate_targets {
        return TargetExecutionMode::SerialPerTarget;
    }

    if plan_set.plans.len() <= 1 {
        print_note!("--aggregate-targets has no effect for a single target; running normally");
        return TargetExecutionMode::SerialPerTarget;
    }

    if cargo_subcommand(cargo_args) == cli::CargoSubcommand::Run {
        print_note!(
            "--aggregate-targets does not apply to `run` (cargo runs one target at a time); running targets serially"
        );
        return TargetExecutionMode::SerialPerTarget;
    }

    if plan_set.show_pruned {
        print_note!(
            "--aggregate-targets is disabled because pruned summaries are target-specific; running targets serially"
        );
        return TargetExecutionMode::SerialPerTarget;
    }

    TargetExecutionMode::Aggregate
}

#[cfg(test)]
mod test {
    use super::*;
    use color_eyre::eyre;
    use serde_json::json;

    fn execution_plan_set(
        targets: &[&str],
        show_pruned: bool,
    ) -> runner::ExecutionPlanSet<'static> {
        runner::ExecutionPlanSet {
            plans: targets
                .iter()
                .map(|target| runner::ExecutionPlan {
                    target: target::TargetTriple((*target).to_string()),
                    package_plans: Vec::new(),
                })
                .collect(),
            show_pruned,
            show_target: targets.len() > 1,
        }
    }

    struct DriverTestEnv {
        host: Option<&'static str>,
    }

    impl target::TargetEnvironment for DriverTestEnv {
        fn cargo_build_target(&self) -> Option<String> {
            None
        }

        fn host_target(&self) -> eyre::Result<target::TargetTriple> {
            let Some(host) = self.host else {
                eyre::bail!("host failed");
            };
            Ok(target::TargetTriple(host.to_string()))
        }
    }

    #[test]
    fn resolve_driver_defaults_to_plain_cargo_for_host_only_plan() -> eyre::Result<()> {
        let driver = resolve_driver(
            &Options::default(),
            &config::WorkspaceConfig::default(),
            &execution_plan_set(&["host"], false),
            &DriverTestEnv { host: Some("host") },
        )?;

        assert_eq!(driver, None);
        Ok(())
    }

    #[test]
    fn resolve_driver_defaults_to_zigbuild_for_cross_plan() -> eyre::Result<()> {
        let driver = resolve_driver(
            &Options::default(),
            &config::WorkspaceConfig::default(),
            &execution_plan_set(&["host", "wasm"], false),
            &DriverTestEnv { host: Some("host") },
        )?;

        assert_eq!(driver, Some("cargo-zigbuild".to_string()));
        Ok(())
    }

    #[test]
    fn resolve_driver_treats_explicit_cargo_as_plain_cargo() -> eyre::Result<()> {
        let options = Options {
            driver: Some("cargo".to_string()),
            ..Options::default()
        };
        let driver = resolve_driver(
            &options,
            &config::WorkspaceConfig::default(),
            &execution_plan_set(&["host", "wasm"], false),
            &DriverTestEnv { host: Some("host") },
        )?;

        assert_eq!(driver, None);
        Ok(())
    }

    #[test]
    fn resolve_driver_uses_explicit_custom_driver() -> eyre::Result<()> {
        let options = Options {
            driver: Some("cross".to_string()),
            ..Options::default()
        };
        let driver = resolve_driver(
            &options,
            &config::WorkspaceConfig::default(),
            &execution_plan_set(&["host"], false),
            &DriverTestEnv { host: Some("host") },
        )?;

        assert_eq!(driver, Some("cross".to_string()));
        Ok(())
    }

    #[test]
    fn resolve_driver_falls_back_to_plain_cargo_when_host_detection_fails() -> eyre::Result<()> {
        let driver = resolve_driver(
            &Options::default(),
            &config::WorkspaceConfig::default(),
            &execution_plan_set(&["wasm"], false),
            &DriverTestEnv { host: None },
        )?;

        assert_eq!(driver, None);
        Ok(())
    }

    #[test]
    fn aggregate_execution_mode_selected_for_supported_multi_target_command() {
        let options = Options {
            aggregate_targets: true,
            ..Options::default()
        };
        let plan_set = execution_plan_set(&["t1", "t2"], false);

        assert_eq!(
            resolve_execution_mode(&options, &["check"], &plan_set),
            runner::TargetExecutionMode::Aggregate
        );
    }

    #[test]
    fn aggregate_execution_mode_falls_back_for_run() {
        let options = Options {
            aggregate_targets: true,
            ..Options::default()
        };
        let plan_set = execution_plan_set(&["t1", "t2"], false);

        assert_eq!(
            resolve_execution_mode(&options, &["run"], &plan_set),
            runner::TargetExecutionMode::SerialPerTarget
        );
    }

    #[test]
    fn aggregate_execution_mode_falls_back_for_pruned_summaries() {
        let options = Options {
            aggregate_targets: true,
            ..Options::default()
        };
        let plan_set = execution_plan_set(&["t1", "t2"], true);

        assert_eq!(
            resolve_execution_mode(&options, &["check"], &plan_set),
            runner::TargetExecutionMode::SerialPerTarget
        );
    }

    #[test]
    fn aggregate_execution_mode_is_noop_for_single_target() {
        let options = Options {
            aggregate_targets: true,
            ..Options::default()
        };
        let plan_set = execution_plan_set(&["t1"], false);

        assert_eq!(
            resolve_execution_mode(&options, &["check"], &plan_set),
            runner::TargetExecutionMode::SerialPerTarget
        );
    }

    #[test]
    fn no_targets_flag_denies_capability() {
        let options = Options {
            no_targets: true,
            ..Options::default()
        };
        let ws = config::WorkspaceConfig::default();
        // Even a target-capable built-in command is denied configured targets
        // when `--no-targets` is set.
        assert!(!resolve_capability_and_warn(
            &options,
            Some("check"),
            Some("check"),
            &ws,
            DEFAULT_METADATA_KEY,
            &[]
        ));
    }

    #[test]
    fn builtin_command_allows_capability_without_no_targets() {
        let options = Options::default();
        let ws = config::WorkspaceConfig::default();
        assert!(resolve_capability_and_warn(
            &options,
            Some("check"),
            Some("check"),
            &ws,
            DEFAULT_METADATA_KEY,
            &[]
        ));
    }

    #[test]
    fn builtin_command_can_be_disabled_by_workspace_policy() {
        let options = Options::default();
        let mut ws = config::WorkspaceConfig::default();
        ws.subcommand_overrides.insert(
            "build".to_string(),
            config::CommandTargetCapability { targets: false },
        );

        assert!(!resolve_capability_and_warn(
            &options,
            Some("build"),
            Some("build"),
            &ws,
            DEFAULT_METADATA_KEY,
            &[]
        ));
    }

    #[test]
    fn resolved_alias_inherits_builtin_capability_by_default() {
        let options = Options::default();
        let ws = config::WorkspaceConfig::default();

        assert!(resolve_capability_and_warn(
            &options,
            Some("lint"),
            Some("clippy"),
            &ws,
            DEFAULT_METADATA_KEY,
            &[]
        ));
    }

    #[test]
    fn explicit_alias_policy_wins_over_resolved_builtin_policy() {
        let options = Options::default();
        let mut ws = config::WorkspaceConfig::default();
        ws.subcommand_overrides.insert(
            "lint".to_string(),
            config::CommandTargetCapability { targets: false },
        );

        assert!(!resolve_capability_and_warn(
            &options,
            Some("lint"),
            Some("clippy"),
            &ws,
            DEFAULT_METADATA_KEY,
            &[]
        ));
    }

    #[test]
    fn explicit_alias_policy_can_enable_unresolved_expanded_command() {
        let options = Options::default();
        let mut ws = config::WorkspaceConfig::default();
        ws.subcommand_overrides.insert(
            "lint".to_string(),
            config::CommandTargetCapability { targets: true },
        );

        assert!(resolve_capability_and_warn(
            &options,
            Some("lint"),
            Some("custom-wrapper"),
            &ws,
            DEFAULT_METADATA_KEY,
            &[]
        ));
    }

    #[test]
    fn no_targets_flag_denies_capability_for_matrix() {
        let options = Options {
            no_targets: true,
            command: Some(Command::FeatureMatrix { pretty: false }),
            ..Options::default()
        };
        let ws = config::WorkspaceConfig::default();
        assert!(!resolve_capability_and_warn(
            &options,
            None,
            None,
            &ws,
            DEFAULT_METADATA_KEY,
            &[]
        ));
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
