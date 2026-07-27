//! The cargo-fc `--help` text.

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
    --maximal-features      Run only maximal feature sets: combinations that
                            are not a subset of another generated combination.
                            An unconstrained matrix collapses into a single
                            all-features invocation per package-target, while
                            mutually exclusive features and other matrix
                            constraints keep one invocation per alternative
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

cargo-fc passes `--no-default-features` to every Cargo invocation, then enables
the features in the current matrix row. The empty row therefore enables no
features; a row containing `default` reproduces Cargo's normal defaults.

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
exclude_features = ["full"] # formerly "denylist"

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

# When enabled, never include the empty feature set (no enabled features), even
# if it would otherwise be generated.
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
exclude_features = ["experimental"]

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
