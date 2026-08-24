//! CLI argument parsing and cargo-fc options.

use color_eyre::eyre::{self, WrapErr};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::config::env::{validate_name, validate_value};
use crate::config::{EnvValue, FlagConfig, FlagSource};
use crate::print_warning;

/// A subcommand cargo-fc answers itself instead of forwarding to Cargo.
///
/// Unlike a Cargo subcommand, these own every argument that follows their
/// token: no child process is spawned, so there is nobody left to interpret —
/// or complain about — an argument cargo-fc does not recognize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnSubcommand {
    /// `cargo fc matrix`
    Matrix,
    /// `cargo fc version`
    Version,
}

impl OwnSubcommand {
    /// The literal token that selects this subcommand.
    fn token(self) -> &'static str {
        match self {
            Self::Matrix => "matrix",
            Self::Version => "version",
        }
    }
}

impl std::fmt::Display for OwnSubcommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.token())
    }
}

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
    Help {
        /// Subcommand whose help was asked for, or `None` for the top-level
        /// help.
        topic: Option<OwnSubcommand>,
    },
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
    /// Cargo's lockfile flags (`--frozen`, `--locked`, `--offline`).
    ///
    /// Besides being forwarded to every spawned cargo process, these must
    /// also constrain the `cargo metadata` discovery step: resolving
    /// dependency metadata may otherwise update `Cargo.lock`, defeating
    /// `--locked` before the first real cargo invocation runs.
    pub locking_flags: Vec<String>,
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

/// What the parser has decided about the subcommand position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubcommandState {
    /// Still before the subcommand token: leading Cargo flags are forwarded,
    /// and a bare `matrix`/`version` still selects a cargo-fc subcommand.
    Pending,
    /// An unrecognized leading flag ruled out a cargo-fc subcommand, so the
    /// rest of `argv` is Cargo's even if it never names a subcommand.
    Blocked,
    /// A Cargo subcommand token was seen; the rest of `argv` is Cargo's.
    Cargo,
    /// A cargo-fc subcommand token was seen; cargo-fc parses the rest itself.
    Own(OwnSubcommand),
}

/// The `matrix` arguments that still have to reach the planning pipeline as
/// Cargo arguments.
///
/// `matrix` spawns nothing, but which rows it prints depends on the command it
/// is asked about (per-command configuration) and the target it is resolved
/// for (`cfg(...)` target overrides). Both are replayed after parsing rather
/// than in the order they were typed, because Cargo's subcommand token has to
/// come before any flag that would hide it.
#[derive(Debug, Default)]
struct MatrixTail {
    /// Cargo subcommand whose per-command configuration the matrix reflects.
    command: Option<String>,
    /// Explicit `--target <triple>` the matrix rows are resolved for.
    target: Option<String>,
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
    let mut state = SubcommandState::Pending;
    let mut matrix_tail = MatrixTail::default();
    let mut raw_manifest_path: Option<PathBuf> = None;

    while let Some(arg) = args.get(index) {
        if arg == "--" {
            if let SubcommandState::Own(subcommand) = state {
                eyre::bail!(
                    "`cargo fc {subcommand}` runs no program, so `--` has nothing to forward to"
                );
            }
            if let Some(rest) = args.get(index..) {
                forwarded.extend(rest.iter().cloned());
            }
            break;
        }

        if state != SubcommandState::Cargo
            && let Some(command) = terminal_flag_command(arg, state)?
        {
            options.command = Some(command);
            break;
        }

        if let SubcommandState::Own(subcommand) = state {
            index += consume_own_subcommand_arg(
                args,
                index,
                arg,
                subcommand,
                &mut options,
                &mut raw_manifest_path,
                &mut matrix_tail,
            )?;
            continue;
        }

        if is_locking_flag(arg) {
            record_locking_flag(&mut options, arg);
            forwarded.push(arg.clone());
            index += 1;
            continue;
        }

        if let Some(consumed) =
            consume_value_option(args, index, &mut options, &mut raw_manifest_path)?
        {
            index += consumed;
            continue;
        }

        if consume_flag_or_command(arg, &mut options, &mut state)? {
            index += 1;
            continue;
        }

        if state == SubcommandState::Pending {
            if let Some(consumed) =
                forward_leading_cargo_arg(args, index, arg, &mut forwarded, &mut state)
            {
                index += consumed;
                continue;
            }
            state = SubcommandState::Cargo;
        }

        forwarded.push(arg.clone());
        index += 1;
    }

    // Help and version print and exit: neither the manifest nor a flag
    // contradiction matters, and failing on one would hide the help that was
    // asked for.
    if matches!(
        options.command,
        Some(Command::Help { .. } | Command::Version)
    ) {
        return Ok((options, forwarded));
    }

    if let Some(command) = matrix_tail.command {
        forwarded.push(command);
    }
    if let Some(target) = matrix_tail.target {
        forwarded.push("--target".to_string());
        forwarded.push(target);
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

/// Recognize a flag that ends parsing with an immediate answer.
///
/// `--help` and `--version` address cargo-fc everywhere except inside a Cargo
/// command's tail, where they belong to the spawned command. Once seen,
/// nothing after them may demote the answer to a run: `cargo fc --help matrix`
/// prints the help, not the matrix. Inside a cargo-fc subcommand's tail the
/// help is that subcommand's own.
///
/// # Errors
///
/// Returns an error if the flag carries an inline value.
fn terminal_flag_command(arg: &str, state: SubcommandState) -> eyre::Result<Option<Command>> {
    let (flag, value) = split_inline_value(arg);
    let command = match flag {
        "--help" | "-h" => Command::Help {
            topic: match state {
                SubcommandState::Own(subcommand) => Some(subcommand),
                _ => None,
            },
        },
        "--version" | "-V" => Command::Version,
        _ => return Ok(None),
    };
    if value.is_some() {
        eyre::bail!("{flag} does not accept a value");
    }
    Ok(Some(command))
}

/// Cargo's lockfile flags, honored by `cargo metadata` as well as by every
/// spawned cargo process.
fn is_locking_flag(arg: &str) -> bool {
    matches!(arg, "--frozen" | "--locked" | "--offline")
}

fn record_locking_flag(options: &mut Options, arg: &str) {
    // Repeating a flag is legal for cargo, but one copy is enough for the
    // metadata invocation.
    if !options.locking_flags.iter().any(|flag| flag == arg) {
        options.locking_flags.push(arg.to_string());
    }
}

fn consume_flag_or_command(
    arg: &str,
    options: &mut Options,
    state: &mut SubcommandState,
) -> eyre::Result<bool> {
    let (flag, value) = split_inline_value(arg);

    if set_bool_flag(&mut options.flags, flag, value)? {
        return Ok(true);
    }

    match flag {
        "--workspace" => {}
        // `--pretty` only shapes matrix output. Forwarding it from the leading
        // Cargo flag position would make `cargo fc --pretty matrix` treat
        // `matrix` as a Cargo subcommand and run it once per combination.
        "--pretty" if *state == SubcommandState::Pending => eyre::bail!(
            "`--pretty` belongs to `cargo fc matrix`; pass it after the `matrix` subcommand"
        ),
        "matrix" if *state == SubcommandState::Pending => {
            options.command = Some(Command::FeatureMatrix { pretty: false });
            *state = SubcommandState::Own(OwnSubcommand::Matrix);
        }
        "version" if *state == SubcommandState::Pending => {
            options.command = Some(Command::Version);
            *state = SubcommandState::Own(OwnSubcommand::Version);
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

/// Parse one argument from a cargo-fc subcommand's own tail, returning how
/// many arguments it consumed.
///
/// A Cargo subcommand can be handed a leftover argument and will explain it
/// itself. `matrix` and `version` spawn nothing, so an argument cargo-fc does
/// not recognize has nowhere to go, and forwarding it amounted to dropping it:
/// typos produced a correct-looking matrix, and `cargo fc matrix --help`
/// printed the matrix rather than the help.
///
/// # Errors
///
/// Returns an error for any argument the subcommand does not accept.
fn consume_own_subcommand_arg(
    args: &[String],
    index: usize,
    arg: &str,
    subcommand: OwnSubcommand,
    options: &mut Options,
    raw_manifest_path: &mut Option<PathBuf>,
    matrix_tail: &mut MatrixTail,
) -> eyre::Result<usize> {
    // Checked before the positional below, which would otherwise read
    // `+nightly` as the Cargo command the matrix is asked about.
    if let Some(toolchain) = arg.strip_prefix('+') {
        eyre::bail!(
            "a `+{toolchain}` toolchain override must come before the `{subcommand}` subcommand"
        );
    }

    if subcommand == OwnSubcommand::Matrix
        && let Some(consumed) =
            consume_matrix_arg(args, index, arg, options, raw_manifest_path, matrix_tail)?
    {
        return Ok(consumed);
    }

    let hint = match subcommand {
        OwnSubcommand::Matrix => {
            "it prints the feature matrix instead of running cargo; \
             see `cargo fc matrix --help` for the arguments it accepts"
        }
        OwnSubcommand::Version => "it takes no arguments",
    };
    eyre::bail!("unexpected argument `{arg}` for `cargo fc {subcommand}`; {hint}");
}

/// Parse one argument that only `cargo fc matrix` accepts.
///
/// Returns `None` for anything the matrix does not recognize, leaving the
/// caller to report it.
fn consume_matrix_arg(
    args: &[String],
    index: usize,
    arg: &str,
    options: &mut Options,
    raw_manifest_path: &mut Option<PathBuf>,
    matrix_tail: &mut MatrixTail,
) -> eyre::Result<Option<usize>> {
    if is_locking_flag(arg) {
        // Nothing is spawned, but the flags still constrain the
        // `cargo metadata` step the matrix is built from.
        record_locking_flag(options, arg);
        return Ok(Some(1));
    }

    if let Some(consumed) = consume_value_option(args, index, options, raw_manifest_path)? {
        return Ok(Some(consumed));
    }

    let (flag, value) = split_inline_value(arg);
    // Run-only flags land here too. They change nothing about the printed
    // matrix, but they are accepted so that a command line keeps working when
    // `matrix` is dropped in front of a real run, and vice versa;
    // `note_matrix_noop_flags` names the ones that had no effect.
    if set_bool_flag(&mut options.flags, flag, value)? {
        return Ok(Some(1));
    }

    // Cargo-fc iterates the workspace itself, so `--workspace` is a no-op it
    // accepts for symmetry with `--exclude`.
    if matches!(flag, "--pretty" | "--workspace") {
        if value.is_some() {
            eyre::bail!("{flag} does not accept a value");
        }
        if flag == "--pretty"
            && let Some(Command::FeatureMatrix { ref mut pretty }) = options.command
        {
            *pretty = true;
        }
        return Ok(Some(1));
    }

    // An explicit target decides both the triple every row carries and the
    // `cfg(...)` overrides the rows are resolved under.
    if let Some(triple) = inline_value(arg, "--target") {
        set_matrix_target(matrix_tail, triple)?;
        return Ok(Some(1));
    }
    if arg == "--target" {
        let triple = next_value(args, index, arg)?;
        set_matrix_target(matrix_tail, &triple)?;
        return Ok(Some(2));
    }

    // The one positional: the Cargo command whose per-command configuration
    // the matrix should reflect, so `cargo fc matrix build` previews exactly
    // what `cargo fc build` would run.
    if !arg.is_empty() && !arg.starts_with('-') {
        if let Some(previous) = &matrix_tail.command {
            eyre::bail!(
                "`cargo fc matrix` previews one cargo command at a time; got `{previous}` and \
                 `{arg}` (only the command name selects per-command configuration, so pass the \
                 name alone)"
            );
        }
        matrix_tail.command = Some(arg.to_string());
        return Ok(Some(1));
    }

    Ok(None)
}

fn set_matrix_target(matrix_tail: &mut MatrixTail, triple: &str) -> eyre::Result<()> {
    if triple.is_empty() {
        eyre::bail!("--target requires a value");
    }
    if let Some(existing) = &matrix_tail.target {
        eyre::bail!(
            "cargo-fc supports only one explicit --target at a time; got `{existing}` and `{triple}`"
        );
    }
    matrix_tail.target = Some(triple.to_string());
    Ok(())
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
    state: &mut SubcommandState,
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
        *state = SubcommandState::Blocked;
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
    use super::{Command, OwnSubcommand, parse_bool, parse_normalized_args};
    use crate::config::DEPRECATED_NO_PRUNE_IMPLIED;
    use crate::config::FlagConfig;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn parse_args(values: &[&str]) -> eyre::Result<(super::Options, Vec<String>)> {
        let args = values.iter().copied().map(String::from).collect::<Vec<_>>();
        parse_normalized_args(&args)
    }

    /// The message of an error the parser is expected to produce.
    fn parse_error(values: &[&str], expectation: &str) -> String {
        parse_args(values).expect_err(expectation).to_string()
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

    /// `cargo fc matrix` spawns no cargo process, so a `--help` after it has
    /// nobody else to reach: it must print the matrix help, not the matrix.
    #[test]
    fn parse_help_after_matrix_asks_for_the_matrix_help() -> eyre::Result<()> {
        let (options, _forwarded) = parse_args(&["matrix", "-p", "cli", "--pretty", "--help"])?;

        assert!(matches!(
            options.command,
            Some(Command::Help {
                topic: Some(OwnSubcommand::Matrix)
            })
        ));

        let (short, _forwarded) = parse_args(&["matrix", "-h"])?;
        assert!(matches!(
            short.command,
            Some(Command::Help {
                topic: Some(OwnSubcommand::Matrix)
            })
        ));
        Ok(())
    }

    #[test]
    fn parse_help_after_version_asks_for_the_version_help() -> eyre::Result<()> {
        let (options, _forwarded) = parse_args(&["version", "--help"])?;

        assert!(matches!(
            options.command,
            Some(Command::Help {
                topic: Some(OwnSubcommand::Version)
            })
        ));
        Ok(())
    }

    /// Cargo build flags used to be forwarded into a void, so a typo produced
    /// a plausible-looking matrix instead of a complaint.
    #[test]
    fn parse_matrix_rejects_arguments_it_cannot_honor() {
        for arg in ["--all-targets", "--release", "--pretyy", "--features=a", ""] {
            let err = parse_error(&["matrix", arg], "unsupported matrix argument");
            assert!(err.contains(arg), "{err}");
            assert!(err.contains("cargo fc matrix"), "{err}");
        }
    }

    #[test]
    fn parse_version_subcommand_rejects_arguments() {
        let err = parse_error(&["version", "--pretty"], "`version` takes no arguments");
        assert!(err.contains("takes no arguments"), "{err}");
    }

    /// The matrix reflects one command's per-command configuration, so a
    /// second bare word is a mistake rather than a silent winner.
    #[test]
    fn parse_matrix_takes_at_most_one_cargo_command() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["matrix", "build"])?;
        assert!(matches!(
            options.command,
            Some(Command::FeatureMatrix { pretty: false })
        ));
        sim_assert_eq!(forwarded, vec!["build".to_string()]);

        let err = parse_error(&["matrix", "build", "test"], "two commands are ambiguous");
        assert!(err.contains("one cargo command at a time"), "{err}");
        Ok(())
    }

    /// The command token has to reach cargo's own subcommand position, so
    /// `--target` may not be replayed in front of it however it was typed.
    #[test]
    fn parse_matrix_replays_the_command_before_the_target() -> eyre::Result<()> {
        let expected = vec![
            "build".to_string(),
            "--target".to_string(),
            "wasm32-unknown-unknown".to_string(),
        ];

        let (_options, forwarded) =
            parse_args(&["matrix", "--target", "wasm32-unknown-unknown", "build"])?;
        sim_assert_eq!(forwarded, expected);

        let (_options, forwarded) =
            parse_args(&["matrix", "build", "--target=wasm32-unknown-unknown"])?;
        sim_assert_eq!(forwarded, expected);
        Ok(())
    }

    /// Flags that only shape a run stay accepted so the same command line can
    /// be pointed at `matrix` or at a real run; `note_matrix_noop_flags`
    /// reports the ones that did nothing.
    #[test]
    fn parse_matrix_accepts_run_only_flags() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&[
            "matrix",
            "--driver",
            "cross",
            "--summary-only",
            "--packages-only",
        ])?;

        assert_eq!(options.driver.as_deref(), Some("cross"));
        assert_eq!(options.flags.summary_only, Some(true));
        assert_eq!(options.flags.packages_only, Some(true));
        assert!(forwarded.is_empty());
        Ok(())
    }

    #[test]
    fn parse_matrix_rejects_a_double_dash_it_cannot_forward() {
        let err = parse_error(
            &["matrix", "--", "extra"],
            "`matrix` has nothing to forward to",
        );
        assert!(err.contains("runs no program"), "{err}");
    }

    /// A `+toolchain` is only a toolchain override in cargo's leading position;
    /// after `matrix` it would silently become the cargo command instead.
    #[test]
    fn parse_matrix_rejects_a_trailing_toolchain_override() -> eyre::Result<()> {
        let err = parse_error(
            &["matrix", "+nightly"],
            "a trailing +toolchain is not a command",
        );
        assert!(err.contains("must come before"), "{err}");

        let (options, forwarded) = parse_args(&["+nightly", "matrix"])?;
        assert!(matches!(
            options.command,
            Some(Command::FeatureMatrix { .. })
        ));
        sim_assert_eq!(forwarded, vec!["+nightly".to_string()]);
        Ok(())
    }

    /// A leading `--help`/`--version` ends parsing: a later subcommand token
    /// used to overwrite it, so `cargo fc --help matrix` printed the matrix.
    #[test]
    fn parse_leading_help_wins_over_a_later_subcommand() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["--help", "matrix"])?;
        assert!(matches!(
            options.command,
            Some(Command::Help { topic: None })
        ));
        assert!(forwarded.is_empty());

        let (options, _forwarded) = parse_args(&["--version", "check"])?;
        assert!(matches!(options.command, Some(Command::Version)));
        Ok(())
    }

    /// `cargo metadata` resolves dependencies itself, so the lockfile flags
    /// must reach it too or `--locked` could still update Cargo.lock during
    /// discovery. They also stay in the forwarded args for the spawned cargo.
    #[test]
    fn parse_locking_flags_reach_metadata_and_cargo() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["check", "--locked", "--offline"])?;
        sim_assert_eq!(options.locking_flags, vec!["--locked", "--offline"]);
        sim_assert_eq!(
            forwarded,
            vec![
                "check".to_string(),
                "--locked".to_string(),
                "--offline".to_string()
            ]
        );

        let (options, forwarded) = parse_args(&["--frozen", "check"])?;
        sim_assert_eq!(options.locking_flags, vec!["--frozen"]);
        sim_assert_eq!(forwarded, vec!["--frozen".to_string(), "check".to_string()]);
        Ok(())
    }

    /// `matrix` spawns nothing, so the lockfile flags constrain only the
    /// metadata step and are not replayed into the forwarded args.
    #[test]
    fn parse_matrix_accepts_locking_flags_for_metadata() -> eyre::Result<()> {
        let (options, forwarded) = parse_args(&["matrix", "--locked", "build"])?;
        sim_assert_eq!(options.locking_flags, vec!["--locked"]);
        sim_assert_eq!(forwarded, vec!["build".to_string()]);

        let err = parse_error(&["version", "--locked"], "`version` reads no metadata");
        assert!(err.contains("takes no arguments"), "{err}");
        Ok(())
    }

    /// Before, this forwarded `--pretty` as a leading cargo flag, which turned
    /// `matrix` into a cargo subcommand run once per feature combination.
    #[test]
    fn parse_pretty_before_matrix_is_rejected() {
        let err = parse_error(
            &["--pretty", "matrix"],
            "`--pretty` is not a leading cargo flag",
        );
        assert!(err.contains("belongs to `cargo fc matrix`"), "{err}");
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
