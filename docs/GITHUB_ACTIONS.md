## Using cargo-fc with github-actions

The github-actions [matrix](https://docs.github.com/en/actions/using-jobs/using-a-matrix-for-your-jobs) feature can be used together with `cargo fc` to more efficiently test combinations of features in CI.


First, add a workflow `feature-matrix.yaml` that computes the feature matrix for your project.
We will re-use this workflow in our `build.yaml` workflow.

```yaml
# .github/workflows/feature-matrix.yaml
name: feature-matrix
on:
  workflow_call:
    outputs:
      matrix:
        description: "feature matrix"
        value: ${{ jobs.matrix.outputs.matrix }}
jobs:
  matrix:
    name: Generate feature matrix
    runs-on: ubuntu-24.04
    outputs:
      matrix: ${{ steps.compute-matrix.outputs.matrix }}
    steps:
      - uses: actions/checkout@v4
      - uses: romnn/cargo-feature-combinations@main
      - name: Compute feature matrix
        id: compute-matrix
        run: |-
          MATRIX="$(cargo fc matrix)"
          echo "${MATRIX}"
          echo "matrix=${MATRIX}" >> "$GITHUB_OUTPUT"
```

Now, we can use the `feature-matrix.yaml` workflow result to dynamically create jobs that build each combination of features in parallel.

```yaml
# .github/workflows/build.yaml
name: build
on:
  push: {}
  pull_request: {}
jobs:
  feature-matrix:
    uses: ./.github/workflows/feature-matrix.yaml

  build:
    name: build ${{ matrix.package.name }} (${{ matrix.os }}, features ${{ matrix.package.features }})
    runs-on: ${{ matrix.os }}
    needs: [feature-matrix]
    strategy:
      fail-fast: false
      matrix:
        os: [macos-latest, ubuntu-24.04]
        package: ${{ fromJson(needs.feature-matrix.outputs.matrix) }}

    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build
        # prettier-ignore
        run: >-
          cargo build
          --package "${{ matrix.package.name }}"
          --features "${{ matrix.package.features }}"
          --all-targets
```

Of course you can also apply the same approach for your `test.yaml` or `lint.yaml` workflows!
Per job, up to 256 feature sets can be processed in parallel.

### Configured targets

If you declare [configured targets](../README.md#configured-targets) in your
`Cargo.toml`, every `cargo fc matrix` row also carries a `target` field, so you
can fan out the GitHub Actions matrix over targets too:

```yaml
strategy:
  fail-fast: false
  matrix:
    package: ${{ fromJson(needs.feature-matrix.outputs.matrix) }}
steps:
  - uses: actions/checkout@v4
  - uses: dtolnay/rust-toolchain@stable
    with:
      targets: ${{ matrix.package.target }}
  - run: >-
      cargo check
      --package "${{ matrix.package.name }}"
      --features "${{ matrix.package.features }}"
      --target "${{ matrix.package.target }}"
```

Alternatively, for linting/checking where you do not need a separate CI job per
combination, a **single** invocation iterates every configured target and
feature combination for you (no GitHub Actions matrix required):

```yaml
- uses: actions/checkout@v4
- uses: romnn/cargo-feature-combinations@main
- run: cargo fc clippy   # or: cargo fc check
```

Add `--aggregate-targets` to batch each combination's targets into one Cargo
invocation for extra throughput on many-core runners.


