---
title: GitHub Actions
weight: 1
---

# GitHub Actions

GitHub Actions' [matrix](https://docs.github.com/en/actions/using-jobs/using-a-matrix-for-your-jobs) feature pairs naturally with `cargo fc matrix` to test feature combinations in parallel.

## Approach 1 — fan out one job per combination

First, a reusable workflow that computes the feature matrix:

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

Then consume it to build every combination in parallel:

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
        run: >-
          cargo build
          --package "${{ matrix.package.name }}"
          --features "${{ matrix.package.features }}"
          --all-targets
```

The same pattern works for `test.yaml` or `lint.yaml`. Up to 256 feature sets can be processed per job.

### Custom metadata

If you configure [matrix metadata]({{< relref "../commands/matrix.md#custom-metadata" >}}), it's available as `matrix.package.metadata`:

```yaml
name: build ${{ matrix.package.name }} (${{ matrix.package.metadata.kind }})
```

## Fanning out over targets too

If you declare [configured targets]({{< relref "../targets/configured-targets.md" >}}), every matrix row carries a `target` field:

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

## Approach 2 — one job, the whole matrix

For linting or checking, where you don't need a separate CI job per combination, a **single** invocation iterates every configured target and feature combination:

```yaml
- uses: actions/checkout@v4
- uses: romnn/cargo-feature-combinations@main
- run: cargo fc clippy   # or: cargo fc check
```

If the runner doesn't already have the configured targets installed, either preinstall them (recommended — more reproducible and cache-friendly) or add `--install-missing-targets`:

```yaml
- uses: dtolnay/rust-toolchain@stable
  with:
    targets: x86_64-unknown-linux-gnu, wasm32-unknown-unknown
```

Add `--aggregate-targets` to batch each combination's targets into one Cargo invocation for extra throughput on many-core runners.

## The setup action

`romnn/cargo-feature-combinations@main` downloads a released binary and puts `cargo fc` on the `PATH`. It accepts an optional `version` input (defaults to the latest release).
