//! CLI argument parsing and cargo-fc options.

use color_eyre::eyre::{self, WrapErr};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::config::env::{validate_name, validate_value};
use crate::config::{EnvValue, FlagConfig};

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
    /// cargo-fc uses plain `cargo` for host-only runs and defaults to
    /// `cargo-zigbuild` when any non-host target is planned (so native-C deps
    /// cross-compile). Set it to `cargo` to force plain cargo, or to any other
    /// cargo wrapper (`cross`, `cargo-careful`, …).
    pub driver: Option<String>,
    /// Explicit child-process environment additions from `--env KEY=VALUE`.
    pub env_set: Vec<(String, EnvValue)>,
    /// Explicit child-process environment removals from `--unset-env KEY`.
    pub env_remove: Vec<String>,
    /// Whether to retain only maximal compatible feature sets.
    pub maximal_features: bool,
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
        .and_then(verbose_from_env_value)
        .or_else(|| {
            std::env::var("VERBOSE")
                .ok()
                .as_deref()
                .and_then(verbose_from_env_value)
        })
}

fn verbose_from_env_value(value: &str) -> Option<bool> {
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

        if let Some(flag) = cargo_fc_bool_inline_flag(arg) {
            eyre::bail!(
                "{flag} does not accept an inline value; configure false values in Cargo.toml"
            );
        }

        if let Some(consumed) =
            consume_value_option(args, index, &mut options, &mut raw_manifest_path)?
        {
            index += consumed;
            continue;
        }

        if consume_flag_or_command(arg, &mut options, &mut subcommand_seen, subcommand_blocked) {
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
) -> bool {
    match arg {
        "--only-packages-with-lib-target" => {
            options.flags.only_packages_with_lib_target = Some(true);
        }
        "--pedantic" => options.flags.pedantic = Some(true),
        "--errors-only" => options.flags.errors_only = Some(true),
        "--packages-only" => options.flags.packages_only = Some(true),
        "--diagnostics-only" => options.flags.diagnostics_only = Some(true),
        "--fail-fast" => options.flags.fail_fast = Some(true),
        "--no-prune-implied" => options.flags.no_prune_implied = Some(true),
        "--show-pruned" => options.flags.show_pruned = Some(true),
        "--maximal-features" => options.maximal_features = true,
        "--aggregate-targets" => options.flags.aggregate_targets = Some(true),
        "--no-targets" => options.flags.no_targets = Some(true),
        "--install-missing-targets" => options.flags.install_missing_targets = Some(true),
        "--dedupe" | "--dedup" => {
            options.flags.dedupe = Some(true);
            options.flags.diagnostics_only = Some(true);
        }
        "--summary-only" | "--summary" | "--silent" => options.flags.summary_only = Some(true),
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
        _ => return false,
    }
    true
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

fn cargo_fc_bool_inline_flag(arg: &str) -> Option<&'static str> {
    [
        "--only-packages-with-lib-target",
        "--pedantic",
        "--errors-only",
        "--packages-only",
        "--diagnostics-only",
        "--fail-fast",
        "--no-prune-implied",
        "--show-pruned",
        "--maximal-features",
        "--aggregate-targets",
        "--no-targets",
        "--install-missing-targets",
        "--dedupe",
        "--dedup",
        "--summary-only",
        "--summary",
        "--silent",
        "--workspace",
        "--pretty",
    ]
    .into_iter()
    .find(|flag| {
        arg.strip_prefix(*flag)
            .is_some_and(|rest| rest.starts_with('='))
    })
}

#[cfg(test)]
mod test {
    use super::{Command, parse_normalized_args, verbose_from_env_value};
    use crate::config::FlagConfig;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn parse_args(values: &[&str]) -> eyre::Result<(super::Options, Vec<String>)> {
        let args = values.iter().copied().map(String::from).collect::<Vec<_>>();
        parse_normalized_args(&args)
    }

    #[test]
    fn verbose_env_value_uses_common_boolean_spellings() {
        assert_eq!(verbose_from_env_value("1"), Some(true));
        assert_eq!(verbose_from_env_value("on"), Some(true));
        assert_eq!(verbose_from_env_value("true"), Some(true));
        assert_eq!(verbose_from_env_value("0"), Some(false));
        assert_eq!(verbose_from_env_value("off"), Some(false));
        assert_eq!(verbose_from_env_value("false"), Some(false));
        assert_eq!(verbose_from_env_value(""), None);
        assert_eq!(verbose_from_env_value("maybe"), None);
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

        assert!(options.maximal_features);
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

    #[test]
    fn parse_rejects_inline_values_for_cargo_fc_bool_flags() {
        let err = parse_args(&["check", "--summary-only=false"])
            .expect_err("inline bool values should fail clearly");

        assert!(err.to_string().contains("--summary-only"));
    }
}
