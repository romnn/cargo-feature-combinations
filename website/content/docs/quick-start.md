---
title: Quick start
weight: 3
---

# Quick start

This walks through a first run and how to read the output. It assumes `cargo fc` is [installed]({{< relref "installation.md" >}}).

## 1. Run a command across the matrix

From any crate or workspace, prefix a cargo command with `fc`:

```bash
cargo fc check
```

`cargo fc` enumerates the combinations of your features, runs `cargo check` against each, and prints a summary:

{{< terminal name="check" >}}

Each row is one combination: the package, whether it passed, its error/warning counts, and the exact feature set. A non-zero exit status means at least one combination failed.

## 2. Try other commands

Any cargo command works — arguments are forwarded through:

```bash
cargo fc clippy
cargo fc test
cargo fc build --all-targets
cargo fc check -p my-crate
```

The only arguments `cargo fc` manages itself are `--features`, `--all-features`, and `--no-default-features`, because those define the matrix.

## 3. Cut the output down

Large matrices produce a lot of text. Focus on what matters:

```bash
# Only warnings and errors, no build chatter
cargo fc --diagnostics-only clippy

# The same, but fold identical diagnostics that repeat across combinations
cargo fc --dedupe clippy

# Only the final result table
cargo fc --summary-only check

# Stop at the first failing combination
cargo fc --fail-fast test
```

See [Output modes]({{< relref "commands/output-modes.md" >}}) for the full set.

## 4. Get a matrix for CI

{{< terminal name="matrix" >}}

Feed this into a GitHub Actions matrix to build every combination in parallel — see [Continuous integration]({{< relref "ci/_index.md" >}}).

## 5. Shape the matrix

When the powerset is too much (or contains combinations that can't compile), configure it in `Cargo.toml`:

```toml
[package.metadata.cargo-fc]
# Never enable these two features together.
exclude_feature_sets = [["postgres", "sqlite"]]

# Ignore the implicit features generated for optional dependencies.
skip_optional_dependencies = true

# Drop the `default` feature from the varied set.
exclude_features = ["default"]
```

Re-run `cargo fc check` and the matrix reflects the configuration. Continue with [Configuration]({{< relref "configuration/_index.md" >}}) to learn the full model, or browse the [Recipes]({{< relref "recipes/_index.md" >}}) for ready-made setups.
