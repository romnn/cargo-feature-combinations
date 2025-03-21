## cargo-feature-combinations

[<img alt="build status" src="https://img.shields.io/github/actions/workflow/status/romnn/cargo-feature-combinations/build.yaml?label=build">](https://github.com/romnn/cargo-feature-combinations/actions/workflows/build.yaml)
[<img alt="test status" src="https://img.shields.io/github/actions/workflow/status/romnn/cargo-feature-combinations/test.yaml?label=test">](https://github.com/romnn/cargo-feature-combinations/actions/workflows/test.yaml)
[![dependency status](https://deps.rs/repo/github/romnn/cargo-feature-combinations/status.svg)](https://deps.rs/repo/github/romnn/cargo-feature-combinations)
[<img alt="crates.io" src="https://img.shields.io/crates/v/cargo-feature-combinations">](https://crates.io/crates/cargo-feature-combinations)

Plugin for `cargo` to run commands against selected combinations of features.

### Installation

```bash
brew install romnn/tap/cargo-fc

# or install from source
cargo install cargo-feature-combinations
```

### Usage

In most cases, just use the command as if it was `cargo`:

```bash
cargo fc check
cargo fc test
cargo fc build
```

In addition, there are a few optional flags and the `matrix` subcommand.
To get an idea, consider these examples:

```bash
# run tests and fail on the first failing combination of features
cargo fc --fail-fast test

# silence output and only show final summary
cargo fc --silent build

# print all combinations of features in JSON (useful for usage in github actions)
cargo fc matrix --pretty
```

For details, please refer to `--help`:

```bash
$ cargo fc --help

USAGE:
    cargo [+toolchain] [SUBCOMMAND] [SUBCOMMAND_OPTIONS]
    cargo [+toolchain] [OPTIONS] [CARGO_OPTIONS] [CARGO_SUBCOMMAND]

SUBCOMMAND:
    matrix                  Print JSON feature combination matrix to stdout
        --pretty            Print pretty JSON

OPTIONS:
    --help                  Print help information
    --silent                Hide cargo output and only show summary
    --fail-fast             Fail fast on the first bad feature combination
    --errors-only           Allow all warnings, show errors only (-Awarnings)
    --pedantic              Treat warnings like errors in summary and
                            when using --fail-fast
```

### Configuration

In your `Cargo.toml`, you can configure the feature combination matrix:

```toml
[package.metadata.cargo-feature-combinations]
# Exclude groupings of features that are incompatible or do not make sense
skip_feature_sets = [ ["foo", "bar"], ]

# Exclude features from the feature combination matrix
denylist = ["default", "full"]
```

### Usage with github-actions

The github-actions [matrix](https://docs.github.com/en/actions/using-jobs/using-a-matrix-for-your-jobs) feature allows more efficient testing of all feature set combinations in CI.

The following workflow file uses `cargo-feature-combinations` to automatically generate a feature matrix and runs up to 256 feature combinations in a matrix job.

```yaml
# TODO: embed example
```

#### Local development

For local development and testing, you can point `cargo fc` to another project using
the `--manifest-path` flag.

```bash
cargo run -- cargo check --manifest-path ../path/to/Cargo.toml
cargo run -- cargo matrix --manifest-path ../path/to/Cargo.toml --pretty
```

#### Acknowledgements

The [`cargo-all-features`](https://crates.io/crates/cargo-all-features) crate is similar yet offers more complex configuration and is lacking a summary.
