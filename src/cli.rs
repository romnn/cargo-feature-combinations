//! CLI argument parsing and cargo-fc options.

use color_eyre::eyre::{self, WrapErr};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::config::env::{validate_name, validate_value};
use crate::config::{EnvValue, FlagConfig, FlagSource};
use crate::print_warning;

/// High-level command requested by the user.
#[derive(Debug)]
pub enum Command {
    /// Print a JSON feature matrix to stdout.
    ///
    /// The matrix is produced by combining [`crate::Package::feature_matrix`]
    /// for all selected packages into a single JSON array.
    FeatureMatrix {
        /// Whether to pretty-print the JSON feature matrix.
        pretty: bool,
    },
    /// Print the tool version and exit.
    Version,
    /// Print help text and exit.
    Help,
}

/// Command-line options recognized by this crate.
///
/// Instances of this type are produced by [`parse_arguments`] and consumed by
/// [`crate::run`] to drive command selection and filtering.
#[derive(Debug, Default)]
pub struct Options {
    /// Optional path to the Cargo manifest that should be inspected.
    pub manifest_path: Option<PathBuf>,
    /// Explicit list of package names to include.
    pub packages: HashSet<String>,
    /// List of package names to exclude.
    pub exclude_packages: HashSet<String>,
    /// High-level command to execute.
    pub command: Option<Command>,
    /// Build driver to invoke in place of `cargo` for each combination.
    ///
    /// Set by `--driver <bin>`. Overrides both the `[workspace.metadata.cargo-fc]
    /// .driver` config and cargo-fc's automatic driver selection. When unset,
    /// cargo-fc picks per target: plain `cargo` for the host target, and
    /// `cargo-zigbuild` for every non-host target (so native-C deps
    /// cross-compile). Set it to `cargo` to force plain cargo, or to any other
    /// cargo wrapper (`cross`, `cargo-careful`, …).
    pub driver: Option<String>,
    /// Explicit child-process environment additions from `--env KEY=VALUE`.
    pub env_set: Vec<(String, EnvValue)>,
    /// Explicit child-process environment removals from `--unset-env KEY`.
    pub env_remove: Vec<String>,
    /// Explicit cargo-fc flag overrides provided by CLI flags or environment.
    pub flags: FlagConfig,
}

mod cargo_commands;
mod help;

pub(crate) use cargo_commands::{
    CargoSubcommand, builtin_canonical_command, builtin_diagnostics_safe,
    builtin_target_capability, cargo_subcommand, cargo_subcommand_token,
    known_quiet_cargo_subcommand, rustup_toolchain, subcommand_token_index,
};
use cargo_commands::{cargo_flag_has_inline_value, cargo_flag_takes_value, is_cargo_no_value_flag};
pub(crate) use help::print_help;

static VALID_BOOLS: [&str; 6] = ["yes", "true", "y", "t", "1", "on"];
static FALSE_BOOLS: [&str; 6] = ["no", "false", "n", "f", "0", "off"];

fn verbose_from_env() -> Option<bool> {
    std::env::var("CARGO_FC_VERBOSE")
        .ok()
        .as_deref()
        .and_then(parse_bool)
        .or_else(|| {
            std::env::var("VERBOSE")
                .ok()
                .as_deref()
                .and_then(parse_bool)
        })
}

/// Parse a boolean written the way an environment variable or an inline flag
/// value spells it.
fn parse_bool(value: &str) -> Option<bool> {
    let normalized = value.trim().to_lowercase();
    if VALID_BOOLS.contains(&normalized.as_str()) {
        Some(true)
    } else if FALSE_BOOLS.contains(&normalized.as_str()) {
        Some(false)
    } else {
        None
    }
}

/// Parse command-line arguments for the `cargo-*` binary.
///
/// The returned [`Options`] drives workspace discovery and filtering, while
/// the remaining `Vec<String>` contains the raw cargo arguments.
///
/// # Errors
///
/// Returns an error if the manifest path passed via `--manifest-path` does
/// not exist or can not be canonicalized.
pub fn parse_arguments(bin_name: &str) -> eyre::Result<(Options, Vec<String>)> {
    let args: Vec<String> = std::env::args_os()
        // Skip executable name
        .skip(1)
        // Skip our own cargo-* command name
        .skip_while(|arg| {
            let arg = arg.as_os_str();
            arg == bin_name || arg == "cargo"
        })
        .map(|s| s.to_string_lossy().to_string())
        .collect();

    parse_normalized_args(&args)
}

fn parse_normalized_args(args: &[String]) -> eyre::Result<(Options, Vec<String>)> {
    let verbose_from_env = verbose_from_env();
    let mut options = Options {
        flags: FlagConfig {
            verbose: verbose_from_env,
            ..FlagConfig::default()
        },
        ..Options::default()
    };

    let mut forwarded = Vec::with_capacity(args.len());
    let mut index = 0usize;
    let mut subcommand_seen = false;
    let mut subcommand_blocked = false;
    let mut raw_manifest_path: Option<PathBuf> = None;

    while let Some(arg) = args.get(index) {
        if arg == "--" {
            if let Some(rest) = args.get(index..) {
                forwarded.extend(rest.iter().cloned());
            }
            break;
        }

        if let Some(consumed) =
            consume_value_option(args, index, &mut options, &mut raw_manifest_path)?
        {
            index += consumed;
            continue;
        }

        if consume_flag_or_command(arg, &mut options, &mut subcommand_seen, subcommand_blocked)? {
            index += 1;
            continue;
        }

        if !subcommand_seen && !subcommand_blocked {
            if let Some(consumed) =
                forward_leading_cargo_arg(args, index, arg, &mut forwarded, &mut subcommand_blocked)
            {
                index += consumed;
                continue;
            }
            subcommand_seen = true;
        }

        forwarded.push(arg.clone());
        index += 1;
    }

    if let Some(manifest_path) = raw_manifest_path {
        let manifest_path = manifest_path
            .canonicalize()
            .wrap_err_with(|| format!("manifest {} does not exist", manifest_path.display()))?;
        options.manifest_path = Some(manifest_path);
    }

    // Flags typed on the command line are one more config layer, held to the
    // same contradiction rules before they are overlaid onto the narrowest one.
    options.flags.normalize(FlagSource::CommandLine)?;

    Ok((options, forwarded))
}

fn consume_value_option(
    args: &[String],
    index: usize,
    options: &mut Options,
    raw_manifest_path: &mut Option<PathBuf>,
) -> eyre::Result<Option<usize>> {
    let Some(arg) = args.get(index).map(String::as_str) else {
        return Ok(None);
    };

    if let Some(value) = inline_value(arg, "--manifest-path") {
        *raw_manifest_path = Some(PathBuf::from(value));
        return Ok(Some(1));
    }
    if arg == "--manifest-path" {
        *raw_manifest_path = Some(PathBuf::from(next_value(args, index, arg)?));
        return Ok(Some(2));
    }

    if let Some(value) = inline_value(arg, "--package") {
        insert_trimmed(&mut options.packages, value);
        return Ok(Some(1));
    }
    if arg == "--package" || arg == "-p" {
        insert_trimmed(&mut options.packages, &next_value(args, index, arg)?);
        return Ok(Some(2));
    }
    if let Some(value) = arg.strip_prefix("-p")
        && !value.is_empty()
    {
        insert_trimmed(&mut options.packages, value);
        return Ok(Some(1));
    }

    if let Some(value) =
        inline_value(arg, "--exclude-package").or_else(|| inline_value(arg, "--exclude"))
    {
        insert_trimmed(&mut options.exclude_packages, value);
        return Ok(Some(1));
    }
    if arg == "--exclude-package" || arg == "--exclude" {
        insert_trimmed(
            &mut options.exclude_packages,
            &next_value(args, index, arg)?,
        );
        return Ok(Some(2));
    }

    if let Some(value) = inline_value(arg, "--driver") {
        options.driver = Some(value.to_string());
        return Ok(Some(1));
    }
    if arg == "--driver" {
        options.driver = Some(next_value(args, index, arg)?);
        return Ok(Some(2));
    }

    if let Some(value) = inline_value(arg, "--env") {
        options.env_set.push(parse_env_assignment(value)?);
        return Ok(Some(1));
    }
    if arg == "--env" {
        let value = next_value(args, index, arg)?;
        options.env_set.push(parse_env_assignment(&value)?);
        return Ok(Some(2));
    }

    if let Some(name) = inline_value(arg, "--unset-env") {
        options.env_remove.push(parse_unset_env(name)?);
        return Ok(Some(1));
    }
    if arg == "--unset-env" {
        let name = next_value(args, index, arg)?;
        options.env_remove.push(parse_unset_env(&name)?);
        return Ok(Some(2));
    }

    Ok(None)
}

fn parse_env_assignment(assignment: &str) -> eyre::Result<(String, EnvValue)> {
    let Some((name, value)) = assignment.split_once('=') else {
        eyre::bail!("--env requires KEY=VALUE");
    };
    if let Err(reason) = validate_name(name) {
        eyre::bail!("environment variable name for --env {reason}");
    }
    if let Err(reason) = validate_value(value) {
        eyre::bail!("environment variable value for --env {reason}");
    }
    Ok((
        name.to_string(),
        EnvValue::from_validated(value.to_string()),
    ))
}

fn parse_unset_env(name: &str) -> eyre::Result<String> {
    if let Err(reason) = validate_name(name) {
        eyre::bail!("environment variable name for --unset-env {reason}");
    }
    Ok(name.to_string())
}

fn next_value(args: &[String], index: usize, flag: &str) -> eyre::Result<String> {
    let Some(value) = args.get(index + 1).filter(|value| value.as_str() != "--") else {
        eyre::bail!("{flag} requires a value");
    };
    Ok(value.clone())
}

fn consume_flag_or_command(
    arg: &str,
    options: &mut Options,
    subcommand_seen: &mut bool,
    subcommand_blocked: bool,
) -> eyre::Result<bool> {
    let (flag, value) = split_inline_value(arg);

    if set_bool_flag(&mut options.flags, flag, value)? {
        return Ok(true);
    }

    match flag {
        "--workspace" => {}
        "--pretty" if matches!(options.command, Some(Command::FeatureMatrix { .. })) => {
            if let Some(Command::FeatureMatrix { ref mut pretty }) = options.command {
                *pretty = true;
            }
        }
        "--help" | "-h" if !*subcommand_seen => options.command = Some(Command::Help),
        "--version" | "-V" if !*subcommand_seen => options.command = Some(Command::Version),
        "matrix" if !*subcommand_seen && !subcommand_blocked => {
            options.command = Some(Command::FeatureMatrix { pretty: false });
            *subcommand_seen = true;
        }
        "version" if !*subcommand_seen && !subcommand_blocked => {
            options.command = Some(Command::Version);
            *subcommand_seen = true;
        }
        // Anything else belongs to cargo, inline value included.
        _ => return Ok(false),
    }

    // What remains are commands and switches with no configurable default, so
    // unlike the flags above they have nothing for a value to override.
    if value.is_some() {
        eyre::bail!("{flag} does not accept a value");
    }
    Ok(true)
}

/// Split `--flag=value` into the flag token and its inline value.
fn split_inline_value(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((flag, value)) => (flag, Some(value)),
        None => (arg, None),
    }
}

/// Apply a cargo-fc boolean flag, reporting whether `flag` is one at all.
///
/// Every flag here mirrors a [`FlagConfig`] key, so each takes an optional
/// inline value — `--flag` alone means `--flag=true`, and `--flag=false` turns
/// off a default configured in `Cargo.toml`. Cargo-fc claims only tokens it
/// already owns, because a `--no-<flag>` spelling would swallow flags that
/// belong to cargo (`--no-fail-fast` for `test`, `--no-dedupe` for `tree`).
///
/// # Errors
///
/// Returns an error if an inline value is not a recognized boolean.
fn set_bool_flag(flags: &mut FlagConfig, flag: &str, value: Option<&str>) -> eyre::Result<bool> {
    let enabled = || match value {
        Some(value) => parse_bool(value).ok_or_else(|| {
            eyre::eyre!(
                "invalid value `{value}` for {flag}; expected true/false, yes/no, on/off or 1/0"
            )
        }),
        None => Ok(true),
    };

    match flag {
        "--only-packages-with-lib-target" => {
            flags.only_packages_with_lib_target = Some(enabled()?);
        }
        "--pedantic" => flags.pedantic = Some(enabled()?),
        "--errors-only" => flags.errors_only = Some(enabled()?),
        "--packages-only" => flags.packages_only = Some(enabled()?),
        "--diagnostics-only" => flags.diagnostics_only = Some(enabled()?),
        "--fail-fast" => flags.fail_fast = Some(enabled()?),
        "--prune-implied" => flags.prune_implied = Some(enabled()?),
        "--no-prune-implied" => {
            let enabled = enabled()?;
            print_warning!(
                "`--no-prune-implied` is deprecated; use `--prune-implied={}` instead",
                !enabled,
            );
            flags.deprecated.no_prune_implied = Some(enabled);
        }
        "--show-pruned" => flags.show_pruned = Some(enabled()?),
        "--maximal-features" => flags.maximal_features = Some(enabled()?),
        "--aggregate-targets" => flags.aggregate_targets = Some(enabled()?),
        "--no-targets" => flags.no_targets = Some(enabled()?),
        "--install-missing-targets" => flags.install_missing_targets = Some(enabled()?),
        "--omit-host-target-flag" => flags.omit_host_target_flag = Some(enabled()?),
        "--dedupe" | "--dedup" => {
            let enabled = enabled()?;
            flags.dedupe = Some(enabled);
            // Dedupe consumes the diagnostics-only stream, so enabling it here
            // implies that mode; turning it off says nothing about diagnostics.
            if enabled {
                flags.diagnostics_only = Some(true);
            }
        }
        "--summary-only" | "--summary" | "--silent" => flags.summary_only = Some(enabled()?),
        _ => return Ok(false),
    }
    Ok(true)
}

fn forward_leading_cargo_arg(
    args: &[String],
    index: usize,
    arg: &str,
    forwarded: &mut Vec<String>,
    subcommand_blocked: &mut bool,
) -> Option<usize> {
    if arg.starts_with('+') || is_cargo_no_value_flag(arg) || cargo_flag_has_inline_value(arg) {
        forwarded.push(arg.to_string());
        return Some(1);
    }
    if cargo_flag_takes_value(arg) {
        forwarded.push(arg.to_string());
        if let Some(value) = args.get(index + 1) {
            forwarded.push(value.clone());
            return Some(2);
        }
        return Some(1);
    }
    if arg.starts_with('-') {
        *subcommand_blocked = true;
        forwarded.push(arg.to_string());
        return Some(1);
    }
    None
}

fn inline_value<'a>(arg: &'a str, flag: &str) -> Option<&'a str> {
    arg.strip_prefix(flag)?.strip_prefix('=')
}

fn insert_trimmed(values: &mut HashSet<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        values.insert(value.to_string());
    }
}

#[cfg(test)]
mod test {
    use super::{Command, parse_bool, parse_normalized_args};
    use crate::config::DEPRECATED_NO_PRUNE_IMPLIED;
    use crate::config::FlagConfig;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn parse_args(values: &[&str]) -> eyre::Result<(super::Options, Vec<String>)> {
        let args = values.iter().copied().map(String::from).collect::<Vec<_>>();
        parse_normalized_args(&args)
    }

    #[test]
    fn bool_values_use_common_spellings() {
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool("on"), Some(true));
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("false"), Some(false));
        assert_eq!(parse_bool(""), None);
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn parsed_flags_use_structured_flag_config() {
        let options = super::Options {
            flags: FlagConfig {
                fail_fast: Some(true),
                summary_only: Some(true),
                ..FlagConfig::default()
            },
            ..super::Options::default()
        };

        assert_eq!(options.flags.fail_fast, Some(true));
        assert_eq!(options.flags.summary_only, Some(true));
    }

    #[test]
    fn maximal_features_is_consumed_after_custom_subcommand() -> eyre::Result<()> {
        let (options, forwarded) =
            parse_args(&["+nightly", "udeps", "--maximal-features", "--all-targets"])?;

        assert_eq!(options.flags.maximal_features, Some(true));
        sim_assert_eq!(forwarded, vec!["+nightly", "udeps", "--all-targets"]);
        Ok(())
    }

    #[test]
    fn structured_flags_preserve_explicit_false_values() {
        let options = super::Options {
            flags: FlagConfig {
                verbose: Some(false),
                ..FlagConfig::default()
            },
            ..super::Options::default()
        };

        assert_eq!(options.flags.verbose, Some(false));
    }

    #[test]
    fn parse_keeps_cargo_fc_flags_after_double_dash() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&[
            "run",
            "--",
            "--help",
            "matrix",
            "--driver",
            "cross",
            "--env",
            "TOKEN=secret",
            "--unset-env",
            "OLD_TOKEN",
        ])?;

        assert!(options.command.is_none());
        assert!(options.driver.is_none());
        assert!(options.env_set.is_empty());
        assert!(options.env_remove.is_empty());
        sim_assert_eq!(
            forwarded,
            vec![
                "run".to_string(),
                "--".to_string(),
                "--help".to_string(),
                "matrix".to_string(),
                "--driver".to_string(),
                "cross".to_string(),
                "--env".to_string(),
                "TOKEN=secret".to_string(),
                "--unset-env".to_string(),
                "OLD_TOKEN".to_string(),
            ]
        );
        Ok(())
    }

    #[test]
    fn parse_matrix_only_at_subcommand_position() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["test", "--features", "matrix"])?;

        assert!(options.command.is_none());
        sim_assert_eq!(
            forwarded,
            vec![
                "test".to_string(),
                "--features".to_string(),
                "matrix".to_string()
            ],
        );
        Ok(())
    }

    #[test]
    fn parse_version_only_at_subcommand_position() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["test", "version"])?;

        assert!(options.command.is_none());
        sim_assert_eq!(forwarded, vec!["test".to_string(), "version".to_string()]);
        Ok(())
    }

    #[test]
    fn parse_pretty_only_for_matrix_command() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["nextest", "run", "--pretty"])?;

        assert!(options.command.is_none());
        sim_assert_eq!(
            forwarded,
            vec![
                "nextest".to_string(),
                "run".to_string(),
                "--pretty".to_string()
            ],
        );

        let (options, forwarded) = parse_args(&["matrix", "--pretty"])?;
        assert!(matches!(
            options.command,
            Some(Command::FeatureMatrix { pretty: true })
        ));
        assert!(forwarded.is_empty());
        Ok(())
    }

    #[test]
    fn parse_help_after_subcommand_is_forwarded() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["clippy", "--help"])?;

        assert!(options.command.is_none());
        sim_assert_eq!(forwarded, vec!["clippy".to_string(), "--help".to_string()]);
        Ok(())
    }

    #[test]
    fn parse_value_options_are_last_wins() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["--driver", "cross", "--driver=cargo", "check"])?;

        assert_eq!(options.driver.as_deref(), Some("cargo"));
        sim_assert_eq!(forwarded, vec!["check".to_string()]);
        Ok(())
    }

    #[test]
    fn parse_env_options_accepts_inline_and_split_forms() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&[
            "--env",
            "FIRST=one",
            "--env=SECOND=two=parts",
            "--env=EMPTY=",
            "--unset-env",
            "OLD",
            "--unset-env=OLDER",
            "check",
        ])?;

        sim_assert_eq!(
            serde_json::to_value(&options.env_set)?,
            serde_json::json!([["FIRST", "one"], ["SECOND", "two=parts"], ["EMPTY", ""],])
        );
        sim_assert_eq!(options.env_remove, vec!["OLD", "OLDER"]);
        sim_assert_eq!(forwarded, vec!["check".to_string()]);
        Ok(())
    }

    #[test]
    fn parsed_options_debug_redacts_env_values() -> eyre::Result<()> {
        let (options, _forwarded) = parse_args(&["--env", "TOKEN=super-secret", "check"])?;

        let debug = format!("{options:?}");

        assert!(debug.contains("TOKEN"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(!debug.contains("super-secret"), "{debug}");
        Ok(())
    }

    #[test]
    fn parse_env_options_reject_invalid_assignments() {
        let missing_equals =
            parse_args(&["--env", "TOKEN", "check"]).expect_err("--env requires an assignment");
        assert!(
            missing_equals
                .to_string()
                .contains("--env requires KEY=VALUE"),
            "{missing_equals}"
        );

        let empty_name =
            parse_args(&["--env", "=value", "check"]).expect_err("--env requires a nonempty name");
        assert!(empty_name.to_string().contains("must not be empty"));

        let nul_name = parse_args(&["--env", "BAD\0NAME=value", "check"])
            .expect_err("--env rejects NUL in names");
        assert!(nul_name.to_string().contains("NUL"));

        let nul_value = parse_args(&["--env", "TOKEN=bad\0value", "check"])
            .expect_err("--env rejects NUL in values");
        assert!(nul_value.to_string().contains("NUL"));

        let unset_equals = parse_args(&["--unset-env", "BAD=NAME", "check"])
            .expect_err("--unset-env rejects equals in names");
        assert!(unset_equals.to_string().contains("must not contain `=`"));
    }

    #[test]
    fn parse_exclude_alias_strips_cargo_workspace_exclude() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["check", "--workspace", "--exclude", " skip "])?;

        assert!(options.exclude_packages.contains("skip"));
        sim_assert_eq!(forwarded, vec!["check".to_string()]);
        Ok(())
    }

    /// Every configurable flag needs a CLI spelling, or a `Cargo.toml` default
    /// cannot be overridden for a single run.
    ///
    /// `verbose` is the deliberate exception: `--verbose` is cargo's own flag
    /// and is forwarded, so `CARGO_FC_VERBOSE` carries the cargo-fc setting.
    #[test]
    fn every_config_flag_key_is_settable_from_the_cli() -> eyre::Result<()> {
        for key in crate::config::FLAG_KEYS {
            // `dedup` is a spelling alias of `dedupe` rather than a field of its
            // own, and `no_prune_implied` is deprecated: both fold into another
            // key during normalization, so neither survives to be asserted on.
            // `parse_deprecated_prune_spelling_still_works` covers the latter.
            if matches!(*key, "dedup" | "verbose" | DEPRECATED_NO_PRUNE_IMPLIED) {
                continue;
            }
            let flag = format!("--{}=false", key.replace('_', "-"));
            let (options, forwarded) = parse_args(&["check", &flag])?;

            sim_assert_eq!(forwarded, vec!["check".to_string()], "{flag} reached cargo");
            sim_assert_eq!(
                serde_json::to_value(options.flags)?.get(*key),
                Some(&serde_json::Value::Bool(false)),
                "{flag} did not set `{key}`",
            );
        }
        Ok(())
    }

    /// Every boolean flag can turn a `Cargo.toml` default back off for one run,
    /// which is the whole point of accepting an inline value.
    #[test]
    fn parse_bool_flags_accept_an_inline_value() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&[
            "check",
            "--summary-only=false",
            "--fail-fast=off",
            "--maximal-features=0",
            "--omit-host-target-flag=no",
            "--pedantic=true",
        ])?;

        assert_eq!(options.flags.summary_only, Some(false));
        assert_eq!(options.flags.fail_fast, Some(false));
        assert_eq!(options.flags.maximal_features, Some(false));
        assert_eq!(options.flags.omit_host_target_flag, Some(false));
        assert_eq!(options.flags.pedantic, Some(true));
        sim_assert_eq!(forwarded, vec!["check".to_string()]);
        Ok(())
    }

    /// A bare flag keeps meaning "on", so existing invocations are unaffected.
    #[test]
    fn parse_bare_bool_flag_still_enables() -> eyre::Result<()> {
        let (options, _forwarded) = parse_args(&["check", "--summary-only"])?;

        assert_eq!(options.flags.summary_only, Some(true));
        Ok(())
    }

    /// `--dedupe` implies diagnostics-only output, but `--dedupe=false` must not
    /// silently enable a mode the user never asked for.
    #[test]
    fn parse_dedupe_only_implies_diagnostics_when_enabled() -> eyre::Result<()> {
        let (enabled, _) = parse_args(&["clippy", "--dedupe"])?;
        assert_eq!(enabled.flags.dedupe, Some(true));
        assert_eq!(enabled.flags.diagnostics_only, Some(true));

        let (disabled, _) = parse_args(&["clippy", "--dedupe=false"])?;
        assert_eq!(disabled.flags.dedupe, Some(false));
        assert_eq!(disabled.flags.diagnostics_only, None);
        Ok(())
    }

    #[test]
    fn parse_prune_implied_sets_the_current_key() -> eyre::Result<()> {
        let (enabled, _) = parse_args(&["check", "--prune-implied"])?;
        assert_eq!(enabled.flags.prune_implied, Some(true));

        let (disabled, _) = parse_args(&["check", "--prune-implied=false"])?;
        assert_eq!(disabled.flags.prune_implied, Some(false));
        Ok(())
    }

    /// The deprecated spelling keeps working and folds into the current key,
    /// inverted, so nothing downstream of parsing sees it.
    #[test]
    fn parse_deprecated_prune_spelling_still_works() -> eyre::Result<()> {
        let (disabled, _) = parse_args(&["check", "--no-prune-implied"])?;
        assert_eq!(disabled.flags.prune_implied, Some(false));
        assert_eq!(disabled.flags.deprecated.no_prune_implied, None);

        let (enabled, _) = parse_args(&["check", "--no-prune-implied=false"])?;
        assert_eq!(enabled.flags.prune_implied, Some(true));
        Ok(())
    }

    /// Naming one setting twice is a mistake, not a race the flag order
    /// silently settles.
    #[test]
    fn parse_rejects_both_prune_spellings_together() {
        let err = parse_args(&["check", "--no-prune-implied", "--prune-implied"])
            .expect_err("mixing prune spellings should fail");

        let message = err.to_string();
        assert!(message.contains("`--no-prune-implied`"), "{message}");
        assert!(message.contains("pass only one"), "{message}");
    }

    #[test]
    fn parse_rejects_unparsable_inline_bool_values() {
        let err = parse_args(&["check", "--summary-only=maybe"])
            .expect_err("a non-boolean value should fail clearly");

        assert!(err.to_string().contains("--summary-only"), "{err}");
        assert!(err.to_string().contains("maybe"), "{err}");
    }

    /// `--workspace` and the commands have no configurable default, so a value
    /// is a mistake rather than an override.
    #[test]
    fn parse_rejects_values_for_switches_without_a_default() {
        let err = parse_args(&["check", "--workspace=true"])
            .expect_err("--workspace should not accept a value");

        assert!(err.to_string().contains("does not accept a value"), "{err}");
    }

    /// A cargo flag that happens to carry an inline value must reach cargo
    /// untouched.
    #[test]
    fn parse_forwards_inline_values_of_cargo_flags() -> eyre::Result<()> {
        let (_options, forwarded) = parse_args(&["check", "--features=a,b"])?;

        sim_assert_eq!(
            forwarded,
            vec!["check".to_string(), "--features=a,b".to_string()]
        );
        Ok(())
    }
}
