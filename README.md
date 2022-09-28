## cargo-feature-combinations

[<img alt="build status" src="https://img.shields.io/github/workflow/status/romnn/cargo-feature-combinations/build?label=build">](https://github.com/romnn/cargo-feature-combinations/actions/workflows/build.yml)
[<img alt="test status" src="https://img.shields.io/github/workflow/status/romnn/cargo-feature-combinations/test?label=test">](https://github.com/romnn/cargo-feature-combinations/actions/workflows/test.yml)
[<img alt="benchmarks" src="https://img.shields.io/github/workflow/status/romnn/cargo-feature-combinations/bench?label=bench">](https://romnn.github.io/cargo-feature-combinations/)
[<img alt="crates.io" src="https://img.shields.io/crates/v/cargo-feature-combinations">](https://crates.io/crates/cargo-feature-combinations)
[<img alt="docs.rs" src="https://img.shields.io/docsrs/cargo-feature-combinations/latest?label=docs.rs">](https://docs.rs/cargo-feature-combinations)

Plugin for `cargo` to run commands against selected combinations of features.

### Installation

```bash
cargo install cargo-feature-combinations
```

### Usage

In most cases, just use the command as if it was `cargo`.
However, there are a few optional flags and the `matrix` subcommand.

```bash
cargo feature-combinations check
cargo feature-combinations test
cargo feature-combinations --fail-fast test
cargo feature-combinations build
cargo feature-combinations --silent build
cargo feature-combinations matrix
```

For details, please refer to `--help`:
```bash
$ cargo feature-combinations --help

USAGE:
    cargo [+toolchain] [SUBCOMMAND]
    cargo [+toolchain] [OPTIONS] [CARGO_OPTIONS] [CARGO_SUBCOMMAND]

SUBCOMMAND:
    matrix                  Print JSON feature combination matrix to stdout

OPTIONS:
    --silent                Hide cargo output and only show summary
    --fail-fast             Fail fast on the first bad feature combination
    --help                  Print help information
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

#### Linting

```bash
cargo clippy --tests --benches --examples -- -Dclippy::all -Dclippy::pedantic
```

#### Acknowledgements

The `[cargo-all-features](https://crates.io/crates/cargo-all-features)` crate is similar yet offers more complex configuration and is lacking a summary.

#### TODO
- when `--silent`, still print the failing feature set
