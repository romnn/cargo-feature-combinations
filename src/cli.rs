//! CLI argument parsing, options, and help text.

use color_eyre::eyre::{self, WrapErr};
use std::collections::HashSet;
use std::path::PathBuf;

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
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    /// Optional path to the Cargo manifest that should be inspected.
    pub manifest_path: Option<PathBuf>,
    /// Explicit list of package names to include.
    pub packages: HashSet<String>,
    /// List of package names to exclude.
    pub exclude_packages: HashSet<String>,
    /// High-level command to execute.
    pub command: Option<Command>,
    /// Whether to restrict processing to packages with a library target.
    pub only_packages_with_lib_target: bool,
    /// Whether to hide cargo output and only show the final summary.
    ///
    /// Set by `--summary-only` or its backward-compatible aliases `--summary`
    /// and `--silent`.
    pub summary_only: bool,
    /// Whether to show only diagnostics (warnings/errors) per feature
    /// combination, suppressing compilation progress noise.
    ///
    /// Set by `--diagnostics-only`.
    pub diagnostics_only: bool,
    /// Whether to deduplicate diagnostics across feature combinations.
    ///
    /// Implies `--diagnostics-only`. Identical diagnostics are printed only
    /// once; the summary reports how many were suppressed.
    pub dedupe: bool,
    /// Whether to print more verbose information such as the full cargo command.
    pub verbose: bool,
    /// Whether to treat warnings like errors for the summary and `--fail-fast`.
    pub pedantic: bool,
    /// Whether to silence warnings from rustc and only show errors.
    pub errors_only: bool,
    /// Whether to only list packages instead of all feature combinations.
    pub packages_only: bool,
    /// Whether to stop processing after the first failing feature combination.
    pub fail_fast: bool,
    /// Whether to disable automatic pruning of implied feature combinations.
    ///
    /// Set by `--no-prune-implied`.
    pub no_prune_implied: bool,
    /// Whether to show pruned feature combinations in the summary.
    ///
    /// Set by `--show-pruned`.
    pub show_pruned: bool,
}

/// Helper trait to provide simple argument parsing over `Vec<String>`.
pub trait ArgumentParser {
    /// Check whether an argument flag exists, either as a standalone flag or
    /// in `--flag=value` form.
    fn contains(&self, arg: &str) -> bool;
    /// Extract all occurrences of an argument and their values.
    ///
    /// When `has_value` is `true`, this matches `--flag value` and
    /// `--flag=value` forms and returns the value part. When `has_value` is
    /// `false`, it matches bare flags like `--flag`.
    fn get_all(&self, arg: &str, has_value: bool)
    -> Vec<(std::ops::RangeInclusive<usize>, String)>;
}

impl ArgumentParser for Vec<String> {
    fn contains(&self, arg: &str) -> bool {
        self.iter()
            .any(|a| a == arg || a.starts_with(&format!("{arg}=")))
    }

    fn get_all(
        &self,
        arg: &str,
        has_value: bool,
    ) -> Vec<(std::ops::RangeInclusive<usize>, String)> {
        let mut matched = Vec::new();
        for (idx, a) in self.iter().enumerate() {
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
    Test,
    Doc,
    Run,
    Other,
}

/// Determine the cargo subcommand implied by the argument list.
pub(crate) fn cargo_subcommand(args: &[impl AsRef<str>]) -> CargoSubcommand {
    let args: HashSet<&str> = args.iter().map(AsRef::as_ref).collect();
    if args.contains("build") || args.contains("b") {
        CargoSubcommand::Build
    } else if args.contains("check") || args.contains("c") || args.contains("clippy") {
        CargoSubcommand::Check
    } else if args.contains("test") || args.contains("t") {
        CargoSubcommand::Test
    } else if args.contains("doc") || args.contains("d") {
        CargoSubcommand::Doc
    } else if args.contains("run") || args.contains("r") {
        CargoSubcommand::Run
    } else {
        CargoSubcommand::Other
    }
}

static VALID_BOOLS: [&str; 4] = ["yes", "true", "y", "t"];

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
                            feature combination, suppressing build noise
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

    let mut options = Options {
        verbose: VALID_BOOLS.contains(
            &std::env::var("VERBOSE")
                .unwrap_or_default()
                .to_lowercase()
                .as_str(),
        ),
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

    for (span, _) in args.get_all("--only-packages-with-lib-target", false) {
        options.only_packages_with_lib_target = true;
        args.drain(span);
    }

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

    let mut drain_flag = |flag: &str, field: &mut bool| {
        for (span, _) in args.get_all(flag, false) {
            *field = true;
            args.drain(span);
        }
    };
    drain_flag("--pedantic", &mut options.pedantic);
    drain_flag("--errors-only", &mut options.errors_only);
    drain_flag("--packages-only", &mut options.packages_only);
    drain_flag("--diagnostics-only", &mut options.diagnostics_only);
    drain_flag("--fail-fast", &mut options.fail_fast);
    drain_flag("--no-prune-implied", &mut options.no_prune_implied);
    drain_flag("--show-pruned", &mut options.show_pruned);

    // --dedupe implies --diagnostics-only
    for flag in ["--dedupe", "--dedup"] {
        for (span, _) in args.get_all(flag, false) {
            options.dedupe = true;
            options.diagnostics_only = true;
            args.drain(span);
        }
    }

    // --summary-only aliases
    for flag in ["--summary-only", "--summary", "--silent"] {
        for (span, _) in args.get_all(flag, false) {
            options.summary_only = true;
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
