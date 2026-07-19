//! CLI argument parsing, options, and help text.

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
    /// Explicit cargo-fc flag overrides provided by CLI flags or environment.
    pub flags: FlagConfig,
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
    let mut skip_next = false;
    for (idx, arg) in args.iter().map(AsRef::as_ref).enumerate() {
        if skip_next {
            skip_next = false;
            if arg == "--" {
                return None;
            }
            continue;
        }
        if arg == "--" {
            return None;
        }
        if arg.starts_with('+') {
            continue;
        }
        if is_cargo_no_value_flag(arg) {
            continue;
        }
        if cargo_flag_takes_value(arg) {
            skip_next = true;
            continue;
        }
        if cargo_flag_has_inline_value(arg) {
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

fn is_cargo_no_value_flag(arg: &str) -> bool {
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

fn cargo_flag_takes_value(arg: &str) -> bool {
    matches!(arg, "--color" | "--config" | "--explain" | "-C" | "-Z")
}

fn cargo_flag_has_inline_value(arg: &str) -> bool {
    arg.starts_with("--color=")
        || arg.starts_with("--config=")
        || arg.starts_with("--explain=")
        || (arg.starts_with("-C") && arg.len() > 2)
        || (arg.starts_with("-Z") && arg.len() > 2)
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

/// Return the canonical built-in command name for a token or short alias.
#[must_use]
pub(crate) fn builtin_canonical_command(token: &str) -> Option<&'static str> {
    builtin_command(token).map(|command| command.canonical)
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

/// Whether cargo-fc should avoid capability hints for a known cargo command.
///
/// This is intentionally only a warning policy. These commands do not gain
/// target or diagnostics capability unless a user configures it explicitly.
#[must_use]
pub(crate) fn known_quiet_cargo_subcommand(token: Option<&str>) -> bool {
    let Some(token) = token else {
        return false;
    };
    matches!(
        token,
        "about"
            | "add"
            | "apk"
            | "asm"
            | "audit"
            | "binstall"
            | "bloat"
            | "bundle"
            | "cache"
            | "careful"
            | "chef"
            | "component"
            | "contract"
            | "cov"
            | "crev"
            | "criterion"
            | "deb"
            | "deny"
            | "dist"
            | "dylint"
            | "edit"
            | "espflash"
            | "expand"
            | "flamegraph"
            | "fuzz"
            | "geiger"
            | "generate"
            | "generate-rpm"
            | "hack"
            | "insta"
            | "info"
            | "install-update"
            | "lambda"
            | "leptos"
            | "license"
            | "llvm-cov"
            | "llvm-lines"
            | "machete"
            | "make"
            | "miri"
            | "modules"
            | "msrv"
            | "mutants"
            | "ndk"
            | "nextest"
            | "nm"
            | "objcopy"
            | "objdump"
            | "outdated"
            | "pgrx"
            | "profdata"
            | "public-api"
            | "public-items"
            | "quickinstall"
            | "readelf"
            | "readme"
            | "readobj"
            | "release"
            | "remove"
            | "rm"
            | "rpm"
            | "semver-checks"
            | "set-version"
            | "shear"
            | "shuttle"
            | "size"
            | "sort"
            | "sqlx"
            | "strip"
            | "sweep"
            | "tauri"
            | "tarpaulin"
            | "udeps"
            | "upgrade"
            | "vet"
            | "watch"
            | "wasi"
            | "whatfeatures"
            | "workspaces"
            | "wix"
            | "zigbuild"
    )
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
    version                 Print version information

OPTIONS:
    -h, --help              Print help information
    -V, --version           Print version information
    --manifest-path <path>  Path to Cargo.toml to inspect
    -p, --package <name>    Include only this workspace package (repeatable)
    --exclude-package <name>
    --exclude <name>        Exclude a workspace package from feature
                            combinations (repeatable). `--exclude` is accepted
                            with `--workspace` for Cargo-compatible workspace
                            package selection.
    --diagnostics-only      Show only diagnostics (warnings/errors) per
                            feature combination. Subcommand must accept
                            --message-format=... and emit rustc JSON
                            diagnostics (e.g. build, check, clippy, doc,
                            or any alias/wrapper that does the same)
    --dedupe, --dedup       Like --diagnostics-only, but also deduplicate
                            identical diagnostics across feature combinations
    --summary-only
    --summary
    --silent                Hide cargo output and only show the final summary
    --fail-fast             Fail fast on the first bad feature combination
    --errors-only           Allow all warnings, show errors only (-Awarnings).
                            This appends to RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS;
                            like any RUSTFLAGS env override, it shadows
                            config-file target rustflags.
    --packages-only         In matrix mode, emit one row per package-target
                            instead of one row per feature combination
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
    --env <KEY=VALUE>       Set an environment variable in each matching Cargo
                            invocation (repeatable; last value for a key wins)
    --unset-env <KEY>       Remove an environment variable from each matching
                            Cargo invocation (repeatable)

ENVIRONMENT:
    CARGO                   Program used for plain Cargo invocations
    CARGO_DRIVER            Set in child processes to the resolved driver unless
                            explicitly set or removed by child env configuration
    CARGO_FC_VERBOSE        Boolean default for verbose cargo-fc headers
    VERBOSE                 Deprecated fallback for CARGO_FC_VERBOSE

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

# Permit at most one feature from each group while preserving the powerset over
# all features outside the groups. The no-member choice is also generated.
mutually_exclusive_features = [
    ["cuda", "coreml", "webgpu"],
]

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
# Referencing an unknown feature here is an error. Other configuration
# options are ignored for these sets.
include_feature_sets = [
    ["foo-a", "bar-a", "other-a"],
] # formerly "exact_combinations"

# Allow only the listed feature sets.
#
# When this list is non-empty, the feature matrix will consist exactly of the
# configured sets. No powerset is generated.
allow_feature_sets = [
    ["hydrate"],
    ["ssr"],
]

# When enabled, never include the empty feature set (no `--features`), even if
# it would otherwise be generated.
no_empty_feature_set = true

# Override the default safety limit of 100000 generated feature combinations.
max_combinations = 250000

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
- `inherit = false` starts from a fresh default config for that target.
  When `inherit = false` is set, patchable fields in that same section must not
  use `add`/`remove`.
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
    use super::{
        CargoSubcommand, Command, builtin_diagnostics_safe, cargo_subcommand,
        cargo_subcommand_token, known_quiet_cargo_subcommand, parse_normalized_args,
        rustup_toolchain, verbose_from_env_value,
    };
    use crate::config::FlagConfig;
    use color_eyre::eyre;
    use similar_asserts::assert_eq as sim_assert_eq;

    fn parse_args(values: &[&str]) -> eyre::Result<(super::Options, Vec<String>)> {
        let args = values.iter().copied().map(String::from).collect::<Vec<_>>();
        parse_normalized_args(&args)
    }

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
    fn cargo_subcommand_token_stops_at_double_dash_after_value_flag() {
        sim_assert_eq!(cargo_subcommand_token(&["--config", "--", "clippy"]), None);
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
    fn known_quiet_subcommands_do_not_gain_builtin_capabilities() {
        for token in [
            "add",
            "generate",
            "license",
            "msrv",
            "nextest",
            "machete",
            "objdump",
            "public-api",
            "udeps",
            "leptos",
            "audit",
        ] {
            assert!(known_quiet_cargo_subcommand(Some(token)));
            assert!(!super::builtin_target_capability(Some(token)));
            assert!(!builtin_diagnostics_safe(Some(token)));
        }
        assert!(!known_quiet_cargo_subcommand(Some("clippy")));
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
