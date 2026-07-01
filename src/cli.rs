//! CLI argument parsing, options, and help text.

use color_eyre::eyre::{self, WrapErr};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::config::FlagConfig;

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
    /// Explicit cargo-fc flag overrides provided by CLI flags or environment.
    pub flags: FlagConfig,
}

/// Helper trait to provide simple argument parsing over `Vec<String>`.
trait ArgumentParser {
    /// Extract all occurrences of an argument and their values.
    ///
    /// When `has_value` is `true`, this matches `--flag value` and
    /// `--flag=value` forms and returns the value part. When `has_value` is
    /// `false`, it matches bare flags like `--flag`.
    fn get_all(&self, arg: &str, has_value: bool)
    -> Vec<(std::ops::RangeInclusive<usize>, String)>;
}

impl ArgumentParser for Vec<String> {
    fn get_all(
        &self,
        arg: &str,
        has_value: bool,
    ) -> Vec<(std::ops::RangeInclusive<usize>, String)> {
        let mut matched = Vec::new();
        for (idx, a) in self.iter().enumerate() {
            if a == "--" {
                break;
            }
            match (a, self.get(idx + 1)) {
                (key, Some(value)) if key == arg && has_value => {
                    matched.push((idx..=idx + 1, value.clone()));
                }
                (key, _) if key == arg && !has_value => {
                    matched.push((idx..=idx, key.clone()));
                }
                (key, _) if key.starts_with(&format!("{arg}=")) => {
                    let value = key.trim_start_matches(&format!("{arg}="));
                    matched.push((idx..=idx, value.to_string()));
                }
                _ => {}
            }
        }
        matched.reverse();
        matched
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum CargoSubcommand {
    Build,
    Check,
    Lint,
    Test,
    Doc,
    Run,
    Other,
}

/// Determine the cargo subcommand implied by the argument list.
pub(crate) fn cargo_subcommand(args: &[impl AsRef<str>]) -> CargoSubcommand {
    match cargo_subcommand_token(args) {
        Some(token) => subcommand_from_token(&token),
        None => CargoSubcommand::Other,
    }
}

fn subcommand_from_token(arg: &str) -> CargoSubcommand {
    builtin_command(arg).map_or(CargoSubcommand::Other, |command| command.subcommand)
}

/// Extract the raw cargo subcommand token from the argument list.
///
/// Unlike [`cargo_subcommand`], this preserves the literal token (e.g. `lint`,
/// `clippy`, `c`) so the command target-capability registry can reason about
/// aliases that the [`CargoSubcommand`] enum collapses to `Other`.
///
/// Returns `None` when no subcommand token is present (e.g. an unknown leading
/// flag or an early `--`).
pub(crate) fn cargo_subcommand_token(args: &[impl AsRef<str>]) -> Option<String> {
    subcommand_token_index(args)
        .and_then(|idx| args.get(idx))
        .map(|arg| arg.as_ref().to_string())
}

/// Index of the subcommand token in `args`, using the same skip rules as
/// [`cargo_subcommand_token`]. Used by alias expansion to replace the token in
/// place.
pub(crate) fn subcommand_token_index(args: &[impl AsRef<str>]) -> Option<usize> {
    fn is_no_value_flag(arg: &str) -> bool {
        matches!(
            arg,
            "-q" | "--quiet"
                | "--frozen"
                | "--locked"
                | "--offline"
                | "-h"
                | "--help"
                | "-V"
                | "--version"
                | "--list"
                | "--verbose"
        ) || arg.starts_with("-v")
    }

    fn takes_value(arg: &str) -> bool {
        matches!(arg, "--color" | "--config" | "--explain" | "-C" | "-Z")
    }

    fn has_inline_value(arg: &str) -> bool {
        arg.starts_with("--color=")
            || arg.starts_with("--config=")
            || arg.starts_with("--explain=")
            || (arg.starts_with("-C") && arg.len() > 2)
            || (arg.starts_with("-Z") && arg.len() > 2)
    }

    let mut skip_next = false;
    for (idx, arg) in args.iter().map(AsRef::as_ref).enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--" {
            return None;
        }
        if arg.starts_with('+') {
            continue;
        }
        if is_no_value_flag(arg) {
            continue;
        }
        if takes_value(arg) {
            skip_next = true;
            continue;
        }
        if has_inline_value(arg) {
            continue;
        }

        if arg.starts_with("--") {
            return None;
        }

        if arg.starts_with('-') {
            return None;
        }

        return Some(idx);
    }

    None
}

/// Extract a rustup toolchain override from forwarded Cargo args.
///
/// Cargo accepts `+toolchain` before the subcommand. When cargo-fc installs
/// missing target components, rustup must receive the same override or it may
/// install targets into the default toolchain instead.
pub(crate) fn rustup_toolchain(args: &[impl AsRef<str>]) -> Option<String> {
    let first = args.first()?.as_ref();
    first
        .strip_prefix('+')
        .filter(|toolchain| !toolchain.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Debug, Clone, Copy)]
struct BuiltinCommand {
    canonical: &'static str,
    diagnostics_safe: bool,
    subcommand: CargoSubcommand,
}

/// Built-in Cargo subcommands cargo-fc understands without user config.
///
/// Every command in this table accepts Cargo's `--target` flag. Diagnostics
/// safety is narrower: broad config may enable diagnostics-only output for
/// commands that emit rustc JSON diagnostics by default.
fn builtin_command(token: &str) -> Option<BuiltinCommand> {
    let (canonical, diagnostics_safe, subcommand) = match token {
        "build" | "b" => ("build", true, CargoSubcommand::Build),
        "check" | "c" => ("check", true, CargoSubcommand::Check),
        "clippy" => ("clippy", true, CargoSubcommand::Lint),
        "doc" | "d" => ("doc", true, CargoSubcommand::Doc),
        "test" | "t" => ("test", false, CargoSubcommand::Test),
        "run" | "r" => ("run", false, CargoSubcommand::Run),
        _ => return None,
    };
    Some(BuiltinCommand {
        canonical,
        diagnostics_safe,
        subcommand,
    })
}

/// Whether cargo-fc's built-in registry knows this command accepts `--target`.
#[must_use]
pub(crate) fn builtin_target_capability(token: Option<&str>) -> bool {
    token.and_then(builtin_command).is_some()
}

/// Whether broad config-driven diagnostics output is safe for this built-in command.
#[must_use]
pub(crate) fn builtin_diagnostics_safe(token: Option<&str>) -> bool {
    token
        .and_then(builtin_command)
        .is_some_and(|command| command.diagnostics_safe)
}

/// Return a command override for one token, including built-in short aliases.
#[must_use]
pub(crate) fn command_override_for_token<'a>(
    token: Option<&str>,
    subcommands: &'a std::collections::BTreeMap<String, crate::config::CommandCapabilities>,
) -> Option<&'a crate::config::CommandCapabilities> {
    let token = token?;
    if let Some(capability) = subcommands.get(token) {
        return Some(capability);
    }
    builtin_command(token).and_then(|command| subcommands.get(command.canonical))
}

/// Return the command override for the user's token, falling back to the
/// resolved alias target only when the raw token has no explicit entry.
#[must_use]
pub(crate) fn selected_command_override<'a>(
    raw_token: Option<&str>,
    resolved_token: Option<&str>,
    subcommands: &'a std::collections::BTreeMap<String, crate::config::CommandCapabilities>,
) -> Option<&'a crate::config::CommandCapabilities> {
    if let Some(raw) = raw_token
        && let Some(capability) = command_override_for_token(Some(raw), subcommands)
    {
        return Some(capability);
    }
    if resolved_token == raw_token {
        None
    } else {
        command_override_for_token(resolved_token, subcommands)
    }
}

static VALID_BOOLS: [&str; 6] = ["yes", "true", "y", "t", "1", "on"];
static FALSE_BOOLS: [&str; 6] = ["no", "false", "n", "f", "0", "off"];

const HELP_TEXT: &str = r#"Run cargo commands for all feature combinations

USAGE:
    cargo fc [+toolchain] [SUBCOMMAND] [SUBCOMMAND_OPTIONS]
    cargo fc [+toolchain] [OPTIONS] [CARGO_OPTIONS] [CARGO_SUBCOMMAND]

SUBCOMMAND:
    matrix                  Print JSON feature combination matrix to stdout
        --pretty            Print pretty JSON

OPTIONS:
    --help                  Print help information
    --diagnostics-only      Show only diagnostics (warnings/errors) per
                            feature combination. Subcommand must accept
                            --message-format=... and emit rustc JSON
                            diagnostics (e.g. build, check, clippy, doc,
                            or any alias/wrapper that does the same)
    --dedupe                Like --diagnostics-only, but also deduplicate
                            identical diagnostics across feature combinations
    --summary-only          Hide cargo output and only show the final summary
    --fail-fast             Fail fast on the first bad feature combination
    --errors-only           Allow all warnings, show errors only (-Awarnings)
    --exclude-package       Exclude a package from feature combinations
    --only-packages-with-lib-target
                            Only consider packages with a library target
    --pedantic              Treat warnings like errors in summary and
                            when using --fail-fast
    --no-prune-implied      Disable automatic pruning of redundant feature
                            combinations implied by other features
    --show-pruned           Show pruned feature combinations in the summary
    --aggregate-targets     Batch each combination's configured targets into a
                            single Cargo invocation (one `--target` per target)
                            instead of one invocation per target. Faster on
                            many cores; reports results per target group. Falls
                            back to serial for `run` and pruned summaries.
    --no-targets            Ignore configured target lists for this invocation
                            and use Cargo's default single target (--target,
                            then CARGO_BUILD_TARGET, then host). An alternative
                            to passing an explicit --target <triple>.
    --install-missing-targets
                            Install missing Rust target components with rustup
                            before running Cargo. Explicit opt-in because this
                            may mutate the toolchain and use the network.
    --driver <bin>          Program invoked in place of `cargo` for each build
                            (e.g. `cargo-zigbuild`, `cross`). Defaults to plain
                            `cargo` for host-only runs and to `cargo-zigbuild`
                            when any non-host target is planned, so native-C
                            dependencies cross-compile. Also settable via
                            [workspace.metadata.cargo-fc].driver; pass `cargo` to
                            force plain cargo.

Feature sets can be configured in your Cargo.toml configuration.
The following metadata key aliases are all supported:

    [package.metadata.cargo-fc]            (recommended)
    [package.metadata.fc]
    [package.metadata.cargo-feature-combinations]
    [package.metadata.feature-combinations]

For example:

```toml
[package.metadata.cargo-fc]

# Exclude groupings of features that are incompatible or do not make sense
exclude_feature_sets = [ ["foo", "bar"], ] # formerly "skip_feature_sets"

# To exclude only the empty feature set from the matrix, you can either enable
# `no_empty_feature_set = true` or explicitly list an empty set here:
#
# exclude_feature_sets = [[]]

# Exclude features from the feature combination matrix
exclude_features = ["default", "full"] # formerly "denylist"

# Include features in the feature combination matrix
#
# These features will be added to every generated feature combination.
# This does not restrict which features are varied for the combinatorial
# matrix. To restrict the matrix to a specific allowlist of features, use
# `only_features`.
include_features = ["feature-that-must-always-be-set"]

# Only consider these features when generating the combinatorial matrix.
#
# When set, features not listed here are ignored for the combinatorial matrix.
# When empty, all package features are considered.
only_features = ["default", "full"]

# Skip implicit features that correspond to optional dependencies from the
# matrix.
#
# When enabled, the implicit features that Cargo generates for optional
# dependencies (of the form `foo = ["dep:foo"]` in the feature graph) are
# removed from the combinatorial matrix. This mirrors the behaviour of the
# `skip_optional_dependencies` flag in the `cargo-all-features` crate.
skip_optional_dependencies = true

# In the end, always add these exact combinations to the overall feature matrix,
# unless one is already present there.
#
# Non-existent features are ignored. Other configuration options are ignored.
include_feature_sets = [
    ["foo-a", "bar-a", "other-a"],
] # formerly "exact_combinations"

# Allow only the listed feature sets.
#
# When this list is non-empty, the feature matrix will consist exactly of the
# configured sets (after dropping non-existent features). No powerset is
# generated.
allow_feature_sets = [
    ["hydrate"],
    ["ssr"],
]

# When enabled, never include the empty feature set (no `--features`), even if
# it would otherwise be generated.
no_empty_feature_set = true

# Automatically prune redundant feature combinations whose resolved feature
# set (after Cargo's feature unification) matches a smaller combination.
# Enabled by default. Disable with `prune_implied = false`.
# prune_implied = true

# When at least one isolated feature set is configured, stop taking all project
# features as a whole, and instead take them in these isolated sets. Build a
# sub-matrix for each isolated set, then merge sub-matrices into the overall
# feature matrix. If any two isolated sets produce an identical feature
# combination, such combination will be included in the overall matrix only once.
#
# This feature is intended for projects with large number of features, sub-sets
# of which are completely independent, and thus don't need cross-play.
#
# Other configuration options are still respected.
isolated_feature_sets = [
    ["foo-a", "foo-b", "foo-c"],
    ["bar-a", "bar-b"],
    ["other-a", "other-b", "other-c"],
]
```

Target-specific configuration can be expressed via Cargo-style `cfg(...)` selectors:

```toml
[package.metadata.cargo-fc]
exclude_features = ["default"]

[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
exclude_features = { add = ["metal"] }
```

Notes:

- Arrays in target overrides are always treated as overrides.
  Use `{ add = [...] }` / `{ remove = [...] }` for additive changes.
- Patches are applied in order: override (or base), then remove, then add.
  If a value appears in both `add` and `remove`, add wins.
- When multiple sections match, their `add`/`remove` sets are unioned.
  Conflicting `override` values result in an error.
- `replace = true` starts from a fresh default config for that target.
  When `replace = true` is set, patchable fields must not use `add`/`remove`.
- `cfg(feature = "...")` predicates are not supported in target override keys.
- If `--target <triple>` or `CARGO_BUILD_TARGET` is set, it is used to select
  matching target overrides (this also applies to `cargo fc matrix`).

When using a cargo workspace, you can also exclude packages in your workspace `Cargo.toml`:

```toml
[workspace.metadata.cargo-fc]
# Exclude packages in the workspace metadata, or the metadata of the *root* package.
exclude_packages = ["package-a", "package-b"]
```

For more information, see 'https://github.com/romnn/cargo-feature-combinations'.

See 'cargo help <command>' for more information on a specific command.
"#;

/// Print the help text to stdout.
pub(crate) fn print_help() {
    println!("{HELP_TEXT}");
}

fn mark_flag(args: &mut Vec<String>, flag: &str, slot: &mut Option<bool>) {
    for (span, _) in args.get_all(flag, false) {
        *slot = Some(true);
        args.drain(span);
    }
}

fn verbose_from_env() -> Option<bool> {
    let value = std::env::var("VERBOSE").ok()?;
    verbose_from_env_value(&value)
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
#[expect(
    clippy::too_many_lines,
    reason = "linear CLI argument parser; splitting it into fragments hurts readability more than its length"
)]
pub fn parse_arguments(bin_name: &str) -> eyre::Result<(Options, Vec<String>)> {
    let mut args: Vec<String> = std::env::args_os()
        // Skip executable name
        .skip(1)
        // Skip our own cargo-* command name
        .skip_while(|arg| {
            let arg = arg.as_os_str();
            arg == bin_name || arg == "cargo"
        })
        .map(|s| s.to_string_lossy().to_string())
        .collect();

    let verbose_from_env = verbose_from_env();
    let mut options = Options {
        flags: FlagConfig {
            verbose: verbose_from_env,
            ..FlagConfig::default()
        },
        ..Options::default()
    };

    // Extract path to manifest to operate on
    for (span, manifest_path) in args.get_all("--manifest-path", true) {
        let manifest_path = PathBuf::from(manifest_path);
        let manifest_path = manifest_path
            .canonicalize()
            .wrap_err_with(|| format!("manifest {} does not exist", manifest_path.display()))?;
        options.manifest_path = Some(manifest_path);
        args.drain(span);
    }

    // Extract packages to operate on
    for flag in ["--package", "-p"] {
        for (span, package) in args.get_all(flag, true) {
            options.packages.insert(package);
            args.drain(span);
        }
    }

    for (span, package) in args.get_all("--exclude-package", true) {
        options.exclude_packages.insert(package.trim().to_string());
        args.drain(span);
    }
    for (span, driver) in args.get_all("--driver", true) {
        options.driver = Some(driver);
        args.drain(span);
    }

    mark_flag(
        &mut args,
        "--only-packages-with-lib-target",
        &mut options.flags.only_packages_with_lib_target,
    );

    // Check for matrix command
    for (span, _) in args.get_all("matrix", false) {
        options.command = Some(Command::FeatureMatrix { pretty: false });
        args.drain(span);
    }
    // Check for pretty matrix option
    for (span, _) in args.get_all("--pretty", false) {
        if let Some(Command::FeatureMatrix { ref mut pretty }) = options.command {
            *pretty = true;
        }
        args.drain(span);
    }

    // Check for help command
    for (span, _) in args.get_all("--help", false) {
        options.command = Some(Command::Help);
        args.drain(span);
    }

    // Check for version flag
    for (span, _) in args.get_all("--version", false) {
        options.command = Some(Command::Version);
        args.drain(span);
    }

    // Check for version command
    for (span, _) in args.get_all("version", false) {
        options.command = Some(Command::Version);
        args.drain(span);
    }

    mark_flag(&mut args, "--pedantic", &mut options.flags.pedantic);
    mark_flag(&mut args, "--errors-only", &mut options.flags.errors_only);
    mark_flag(
        &mut args,
        "--packages-only",
        &mut options.flags.packages_only,
    );
    mark_flag(
        &mut args,
        "--diagnostics-only",
        &mut options.flags.diagnostics_only,
    );
    mark_flag(&mut args, "--fail-fast", &mut options.flags.fail_fast);
    mark_flag(
        &mut args,
        "--no-prune-implied",
        &mut options.flags.no_prune_implied,
    );
    mark_flag(&mut args, "--show-pruned", &mut options.flags.show_pruned);
    mark_flag(
        &mut args,
        "--aggregate-targets",
        &mut options.flags.aggregate_targets,
    );
    mark_flag(&mut args, "--no-targets", &mut options.flags.no_targets);
    mark_flag(
        &mut args,
        "--install-missing-targets",
        &mut options.flags.install_missing_targets,
    );

    // --dedupe implies --diagnostics-only
    for flag in ["--dedupe", "--dedup"] {
        for (span, _) in args.get_all(flag, false) {
            options.flags.dedupe = Some(true);
            options.flags.diagnostics_only = Some(true);
            args.drain(span);
        }
    }

    // --summary-only aliases
    for flag in ["--summary-only", "--summary", "--silent"] {
        for (span, _) in args.get_all(flag, false) {
            options.flags.summary_only = Some(true);
            args.drain(span);
        }
    }

    // Ignore `--workspace`. This tool already discovers the relevant workspace
    // packages via `cargo metadata` and then runs cargo separately in each
    // package's directory. Forwarding `--workspace` to those per-package
    // invocations would re-enable workspace-level feature application and can
    // cause spurious errors when some workspace members do not define a
    // particular feature.
    for (span, _) in args.get_all("--workspace", false) {
        args.drain(span);
    }

    Ok((options, args))
}

#[cfg(test)]
mod test {
    use super::{
        ArgumentParser, CargoSubcommand, builtin_diagnostics_safe, cargo_subcommand,
        cargo_subcommand_token, command_override_for_token, rustup_toolchain,
        verbose_from_env_value,
    };
    use crate::config::{CommandCapabilities, FlagConfig};
    use similar_asserts::assert_eq as sim_assert_eq;
    use std::collections::BTreeMap;

    #[test]
    fn cargo_subcommand_detects_build_and_short_build() {
        sim_assert_eq!(cargo_subcommand(&["build"]), CargoSubcommand::Build);
        sim_assert_eq!(cargo_subcommand(&["b"]), CargoSubcommand::Build);
    }

    #[test]
    fn cargo_subcommand_detects_check_and_short_check() {
        sim_assert_eq!(cargo_subcommand(&["check"]), CargoSubcommand::Check);
        sim_assert_eq!(cargo_subcommand(&["c"]), CargoSubcommand::Check);
    }

    #[test]
    fn cargo_subcommand_detects_clippy_as_lint() {
        sim_assert_eq!(cargo_subcommand(&["clippy"]), CargoSubcommand::Lint);
    }

    #[test]
    fn cargo_subcommand_detects_test_and_short_test() {
        sim_assert_eq!(cargo_subcommand(&["test"]), CargoSubcommand::Test);
        sim_assert_eq!(cargo_subcommand(&["t"]), CargoSubcommand::Test);
    }

    #[test]
    fn cargo_subcommand_detects_doc_and_short_doc() {
        sim_assert_eq!(cargo_subcommand(&["doc"]), CargoSubcommand::Doc);
        sim_assert_eq!(cargo_subcommand(&["d"]), CargoSubcommand::Doc);
    }

    #[test]
    fn cargo_subcommand_detects_run_and_short_run() {
        sim_assert_eq!(cargo_subcommand(&["run"]), CargoSubcommand::Run);
        sim_assert_eq!(cargo_subcommand(&["r"]), CargoSubcommand::Run);
    }

    #[test]
    fn cargo_subcommand_skips_known_leading_cargo_flags_and_values() {
        let subcommand = cargo_subcommand(&[
            "+nightly",
            "--config",
            "net.retry=2",
            "--color=always",
            "-vv",
            "--frozen",
            "clippy",
            "build",
        ]);

        sim_assert_eq!(subcommand, CargoSubcommand::Lint);
    }

    #[test]
    fn cargo_subcommand_handles_help_and_version_flags_before_subcommand() {
        sim_assert_eq!(
            cargo_subcommand(&["--verbose", "--help", "clippy"]),
            CargoSubcommand::Lint
        );
        sim_assert_eq!(
            cargo_subcommand(&["-vv", "--frozen", "test"]),
            CargoSubcommand::Test
        );
    }

    #[test]
    fn cargo_subcommand_returns_other_for_unknown_leading_flag() {
        let subcommand = cargo_subcommand(&["--mystery-flag", "clippy"]);

        sim_assert_eq!(subcommand, CargoSubcommand::Other);
    }

    #[test]
    fn argument_parser_ignores_cargo_fc_flags_after_double_dash() {
        let args = vec![
            "run".to_string(),
            "--".to_string(),
            "--help".to_string(),
            "matrix".to_string(),
            "--driver".to_string(),
            "cross".to_string(),
        ];

        assert!(args.get_all("--help", false).is_empty());
        assert!(args.get_all("matrix", false).is_empty());
        assert!(args.get_all("--driver", true).is_empty());
    }

    #[test]
    fn argument_parser_keeps_matches_before_double_dash() {
        let args = vec![
            "--driver".to_string(),
            "cargo".to_string(),
            "--".to_string(),
            "--driver".to_string(),
            "cross".to_string(),
        ];

        let matches = args.get_all("--driver", true);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].1, "cargo");
    }

    #[test]
    fn cargo_subcommand_treats_unknown_aliases_as_other() {
        sim_assert_eq!(cargo_subcommand(&["lint"]), CargoSubcommand::Other);
        sim_assert_eq!(cargo_subcommand(&["lint", "build"]), CargoSubcommand::Other);
    }

    #[test]
    fn cargo_subcommand_token_preserves_literal_token() {
        sim_assert_eq!(
            cargo_subcommand_token(&["clippy"]),
            Some("clippy".to_string())
        );
        sim_assert_eq!(cargo_subcommand_token(&["lint"]), Some("lint".to_string()));
        sim_assert_eq!(cargo_subcommand_token(&["c"]), Some("c".to_string()));
        sim_assert_eq!(
            cargo_subcommand_token(&["+nightly", "--frozen", "lint", "build"]),
            Some("lint".to_string())
        );
    }

    #[test]
    fn cargo_subcommand_token_none_for_missing_command() {
        let empty: [&str; 0] = [];
        sim_assert_eq!(cargo_subcommand_token(&empty), None);
        sim_assert_eq!(cargo_subcommand_token(&["--mystery-flag"]), None);
        sim_assert_eq!(cargo_subcommand_token(&["--"]), None);
    }

    #[test]
    fn rustup_toolchain_detects_cargo_toolchain_override() {
        sim_assert_eq!(
            rustup_toolchain(&["+nightly", "--frozen", "check"]),
            Some("nightly".to_string())
        );
    }

    #[test]
    fn rustup_toolchain_ignores_args_after_double_dash() {
        sim_assert_eq!(rustup_toolchain(&["run", "--", "+nightly"]), None);
    }

    #[test]
    fn rustup_toolchain_ignores_plus_values_after_leading_position() {
        sim_assert_eq!(rustup_toolchain(&["check", "--target-dir", "+out"]), None);
    }

    #[test]
    fn builtin_clippy_is_diagnostics_safe() {
        assert!(builtin_diagnostics_safe(Some("clippy")));
    }

    #[test]
    fn builtin_test_does_not_have_config_diagnostics_by_default() {
        assert!(!builtin_diagnostics_safe(Some("test")));
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
    fn builtin_short_alias_inherits_long_command_policy() {
        let mut subcommands = BTreeMap::new();
        subcommands.insert(
            "build".to_string(),
            CommandCapabilities {
                targets: Some(false),
                ..CommandCapabilities::default()
            },
        );

        let override_config = command_override_for_token(Some("b"), &subcommands);

        assert_eq!(
            override_config.and_then(|config| config.targets),
            Some(false)
        );
    }

    #[test]
    fn builtin_short_alias_exact_policy_wins_over_long_command_policy() {
        let mut subcommands = BTreeMap::new();
        subcommands.insert(
            "build".to_string(),
            CommandCapabilities {
                targets: Some(false),
                ..CommandCapabilities::default()
            },
        );
        subcommands.insert(
            "b".to_string(),
            CommandCapabilities {
                targets: Some(true),
                ..CommandCapabilities::default()
            },
        );

        let override_config = command_override_for_token(Some("b"), &subcommands);

        assert_eq!(
            override_config.and_then(|config| config.targets),
            Some(true)
        );
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
}
