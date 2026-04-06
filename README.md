## cargo-feature-combinations

[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/romnn/cargo-feature-combinations/build.yaml?label=build">](https://github.com/romnn/cargo-feature-combinations/actions/workflows/build.yaml)
[<img alt="test status" src="https://img.shields.io/github/actions/workflow/status/romnn/cargo-feature-combinations/test.yaml?label=test">](https://github.com/romnn/cargo-feature-combinations/actions/workflows/test.yaml)
[![dependency status](https://deps.rs/repo/github/romnn/cargo-feature-combinations/status.svg)](https://deps.rs/repo/github/romnn/cargo-feature-combinations)
[<img alt="docs.rs" src="https://img.shields.io/docsrs/cargo-feature-combinations/latest?label=docs.rs">](https://docs.rs/cargo-feature-combinations)
[<img alt="crates.io" src="https://img.shields.io/crates/v/cargo-feature-combinations">](https://crates.io/crates/cargo-feature-combinations)

Plugin for `cargo` to run commands against selected (or all) combinations of features.

<p align="center">
  <img src="test-data/screenshot.png" alt="cargo-feature-combinations demo" width="550">
</p>

### Installation

```bash
brew install --cask romnn/tap/cargo-fc

# Or install from source
cargo install --locked cargo-feature-combinations
```

### Usage

Just use the command as if it was `cargo`:

```bash
cargo fc check
cargo fc test
cargo fc build

# All cargo arguments are passed along, except 
#   - `--all-features`
#   - `--features` 
#   - `--no-default-features` 
cargo fc check -p <my-crate> --all-targets
```

In addition, there are a few new flags and the `matrix` subcommand.
To get an idea, consider these examples:

```bash
# Run tests and fail on the first failing combination of features
cargo fc --fail-fast test

# Silence output and only show final summary
cargo fc --silent build

# Print all combinations of features in JSON (useful for usage in github actions)
cargo fc matrix --pretty
```

For details, please refer to `--help`:

```bash
$ cargo fc --help

USAGE:
    cargo fc [+toolchain] [SUBCOMMAND] [SUBCOMMAND_OPTIONS]
    cargo fc [+toolchain] [OPTIONS] [CARGO_OPTIONS] [CARGO_SUBCOMMAND]

SUBCOMMAND:
    matrix                  Print JSON feature combination matrix to stdout
        --pretty            Print pretty JSON

OPTIONS:
    --help                  Print help information
    --silent                Hide cargo output and only show summary
    --fail-fast             Fail fast on the first bad feature combination
    --exclude-package       Exclude a package from feature combinations 
    --only-packages-with-lib-target
                            Only consider packages with a library target
    --errors-only           Allow all warnings, show errors only (-Awarnings)
    --pedantic              Treat warnings like errors in summary and
                            when using --fail-fast
```

### Configuration

In your `Cargo.toml`, you can configure the feature combination matrix.
The following metadata key aliases are all supported:

```
[package.metadata.cargo-fc]              (recommended)
[package.metadata.fc]
[package.metadata.cargo-feature-combinations]
[package.metadata.feature-combinations]
```

For example:

```toml
[package.metadata.cargo-fc]

# Exclude groupings of features that are incompatible or do not make sense
exclude_feature_sets = [ ["foo", "bar"], ] # formerly "skip_feature_sets"

# To exclude only the empty feature set from the matrix, you can either enable
# `no_empty_feature_set = true` or explicitly list an empty set here:
exclude_feature_sets = [[]]

# Exclude features from the feature combination matrix
exclude_features = ["default", "full"] # formerly "denylist"

# Skip implicit features that correspond to optional dependencies from the
# matrix.
#
# When enabled, the implicit features that Cargo generates for optional
# dependencies (of the form `foo = ["dep:foo"]` in the feature graph) are
# removed from the combinatorial matrix. This mirrors the behaviour of the
# `skip_optional_dependencies` flag in the `cargo-all-features` crate.
skip_optional_dependencies = true

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

# When at least one isolated feature set is configured, stop taking all project 
# features as a whole, and instead take them in these isolated sets. Build a 
# sub-matrix for each isolated set, then merge sub-matrices into the overall 
# feature matrix. If any two isolated sets produce an identical feature 
# combination, such combination will be included in the overall matrix only once.
#
# This feature is intended for projects with large number of features, sub-sets 
# of which are completely independent, and thus don’t need cross-play.
#
# Non-existent features are ignored. Other configuration options are still 
# respected.
isolated_feature_sets = [
    ["foo-a", "foo-b", "foo-c"],
    ["bar-a", "bar-b"],
    ["other-a", "other-b", "other-c"],
]

# Optional: Additional metadata merged into `cargo fc matrix` output
# $ cargo fc matrix --pretty
#   [
#     { "name": "my-crate", "features": "", "kind": "ci" },
#     { "name": "my-crate", "features": "a", "kind": "ci" },
#     { "name": "my-crate", "features": "b", "kind": "ci" },
#     { "name": "my-crate", "features": "a,b", "kind": "ci" },
#   ]
matrix = { kind = "ci" }

# Optional: The `matrix` metadata from before can also be its own section
# $ cargo fc matrix --pretty
#   [{
#       "requires-gpu": false,
#       "value-for-this-crate": "will show up in the feature matrix",
#       ..
#    }, .. ]
[package.metadata.cargo-fc.matrix]
value-for-this-crate = "will show up in the feature matrix"
requires-gpu = false
```

When using a cargo workspace, you can also exclude packages in your workspace `Cargo.toml`:

```toml
[workspace.metadata.cargo-fc]
# Exclude packages in the workspace metadata, or the metadata of the *root* package.
exclude_packages = ["package-a", "package-b"]
```

<details>
<summary>Example: skipping optional dependency features</summary>

```toml
[features]
default = []
core = []
cli = ["core"]

[dependencies]
tokio = { version = "1", optional = true }
serde = { version = "1", optional = true }

[package.metadata.cargo-fc]
exclude_features = ["default"]
skip_optional_dependencies = true
```

With this configuration, the feature matrix will only vary the `core` and
`cli` features. The implicit `tokio` and `serde` features that correspond to
optional dependencies are excluded from the matrix, avoiding a combinatorial
explosion over integration features. If you still want to test specific
combinations that include `tokio` or `serde`, you can list them explicitly in
`include_feature_sets`.

</details>

---

### Target-specific configuration

You can override configuration for specific targets using Cargo-style `cfg(...)` expressions.
Overrides are configured under:

```toml
[package.metadata.cargo-fc.target.'cfg(...)']
```

Example (exclude different features per OS):

```toml
[package.metadata.cargo-fc]
exclude_features = ["default"]

[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
exclude_features = { add = ["metal"] }

[package.metadata.cargo-fc.target.'cfg(target_os = "macos")']
exclude_features = { add = ["cuda"] }
```

Patch semantics for collection-like keys such as `exclude_features`, `include_features`,
`only_features`, `*_feature_sets`:

- **Array syntax is always an override**
  - `exclude_features = ["cuda"]` replaces the entire value.
  - This is equivalent to `exclude_features = { override = ["cuda"] }`.
- **Patch object syntax is explicit**
  - Override (replace the entire value):
    - `exclude_features = { override = ["cuda"] }`
  - Add (union with the base value):
    - `exclude_features = { add = ["cuda"] }`
  - Remove (subtract from the base value):
    - `exclude_features = { remove = ["cuda"] }`

Patches are applied in order: override (or base), then remove, then add.
If a value appears in both `add` and `remove`, add wins.

When multiple target override sections match (e.g. `cfg(unix)` and `cfg(target_os = "linux")`),
their `add` and `remove` sets are unioned. Conflicting `override` values result in an error.

##### `replace = true`

If a matching target override sets `replace = true`, resolution starts from a fresh default
configuration (instead of inheriting from the base config). To avoid confusion, when
`replace = true` is set, patchable fields must not use `add` or `remove` (only override
is allowed).

<details>
<summary>Example: Start from fresh config with `replace=true`</summary>

```toml
[package.metadata.cargo-fc]
exclude_features = ["default"]
isolated_feature_sets = [
  ["gpu"],
  ["ui"],
]
skip_optional_dependencies = true

[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
replace = true

# Start from a fresh default config on Linux: `isolated_feature_sets` and
# `skip_optional_dependencies` are not inherited from the base config.
exclude_features = ["default", "cuda"] # using array shorthand, i.e. override
```
</details>

---

### Usage with github-actions

The github-actions [matrix](https://docs.github.com/en/actions/using-jobs/using-a-matrix-for-your-jobs) feature can be used together with `cargo fc` to more efficiently test combinations of features in CI. See [GITHUB_ACTIONS.md](./docs/GITHUB_ACTIONS.md) for more information.

### Local development

For local development and testing, you can point `cargo fc` to another project using
the `--manifest-path` flag.

```bash
cargo run -- cargo check --manifest-path ../path/to/Cargo.toml
cargo run -- cargo matrix --manifest-path ../path/to/Cargo.toml --pretty
```
