---
title: cargo-feature-combinations
type: docs
bookToc: false
---

<div class="cfc-hero">
  <div class="cfc-hero__text">
    <h1>cargo&#8209;feature&#8209;combinations</h1>
    <p class="cfc-hero__lead">A <code>cargo</code> subcommand that runs a command against combinations of a crate's features and reports the results. Open source, MIT-licensed.</p>
    <div class="cfc-hero__cmd">cargo fc check</div>
    <div class="cfc-hero__actions">
      <a class="cfc-btn cfc-btn--primary" href="{{< relref "/docs/introduction.md" >}}">Read the docs</a>
      <a class="cfc-btn" href="https://github.com/romnn/cargo-feature-combinations">Source on GitHub</a>
    </div>
  </div>
  <div class="cfc-hero__shot">
    {{< img src="images/check.png" alt="cargo fc check running across the feature combinations of a workspace" >}}
  </div>
</div>

<div class="cfc-badges">

[![build status](https://img.shields.io/github/actions/workflow/status/romnn/cargo-feature-combinations/build.yaml?label=build)](https://github.com/romnn/cargo-feature-combinations/actions/workflows/build.yaml)
[![test status](https://img.shields.io/github/actions/workflow/status/romnn/cargo-feature-combinations/test.yaml?label=test)](https://github.com/romnn/cargo-feature-combinations/actions/workflows/test.yaml)
[![crates.io](https://img.shields.io/crates/v/cargo-feature-combinations)](https://crates.io/crates/cargo-feature-combinations)
[![docs.rs](https://img.shields.io/docsrs/cargo-feature-combinations/latest?label=docs.rs)](https://docs.rs/cargo-feature-combinations)

</div>

## What it does

Cargo features are additive in principle, but a crate can compile with its default set and fail with `--no-default-features`, or fail only when two features are enabled together. Testing the default set — or `--all-features` — doesn't exercise those cases.

`cargo fc` enumerates combinations of a crate's features, runs a `cargo` command against each, and prints one summary. It works on single crates and workspaces, can drive the run across several target triples, and can emit a JSON matrix for CI.

<div class="cfc-cards">
  <div class="cfc-card">
    <h3>Feature matrix</h3>
    <p>Runs a command against the powerset of a crate's features. Prune, restrict, or pin combinations from <code>Cargo.toml</code>.</p>
  </div>
  <div class="cfc-card">
    <h3>Output modes</h3>
    <p>Reduce output to warnings and errors, deduplicate diagnostics across combinations, or show only the summary.</p>
  </div>
  <div class="cfc-card">
    <h3>CI matrix</h3>
    <p><code>cargo fc matrix</code> prints a JSON matrix for a GitHub Actions build matrix — one row per combination.</p>
  </div>
  <div class="cfc-card">
    <h3>Targets</h3>
    <p>Check every combination across multiple target triples, with a zig-based driver for cross-compiling native-C dependencies.</p>
  </div>
</div>

## Example

```bash
# Install
cargo install --locked cargo-feature-combinations

# Run a command across the feature matrix
cargo fc check
cargo fc clippy
cargo fc test

# Warnings and errors only, deduplicated across combinations
cargo fc --dedupe clippy

# A JSON matrix for CI
cargo fc matrix --pretty
```

Shape the matrix in `Cargo.toml` when the defaults don't fit:

```toml
[package.metadata.cargo-fc]
# Two features that must not be enabled together.
exclude_feature_sets = [["postgres", "sqlite"]]
# Don't vary the implicit features generated for optional dependencies.
skip_optional_dependencies = true
```

## Documentation

- [Introduction]({{< relref "/docs/introduction.md" >}}) and [Installation]({{< relref "/docs/installation.md" >}}).
- [Quick start]({{< relref "/docs/quick-start.md" >}}) — a first run and how to read the output.
- [Configuration]({{< relref "/docs/configuration/_index.md" >}}) — shape the matrix, including per-target and per-command overrides.
- [Recipes]({{< relref "/docs/recipes/_index.md" >}}) — configurations for common scenarios.
- [Continuous integration]({{< relref "/docs/ci/_index.md" >}}) — use the matrix in GitHub Actions.
