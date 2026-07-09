---
title: Documentation
bookToc: false
bookFlatSection: false
---

# Documentation

`cargo-feature-combinations` runs `cargo` commands against selected — or all — combinations of your crate's features. This documentation takes you from installation to advanced, per-target and per-command matrix configuration.

## Start here

- **[Introduction]({{< relref "introduction.md" >}})** — what the tool does and how it thinks about features.
- **[Installation]({{< relref "installation.md" >}})** — install the `cargo fc` subcommand.
- **[Quick start]({{< relref "quick-start.md" >}})** — your first run and how to read the output.

## Go deeper

- **[Commands]({{< relref "commands/_index.md" >}})** — running cargo through `fc`, output modes, the `matrix` subcommand, and the full CLI reference.
- **[Configuration]({{< relref "configuration/_index.md" >}})** — shape the feature matrix from `Cargo.toml`, including the precedence model, per-target and per-command overrides.
- **[Targets & cross-compilation]({{< relref "targets/_index.md" >}})** — check every combination on every target triple, and pick a build driver.
- **[Recipes]({{< relref "recipes/_index.md" >}})** — copy-paste configurations for common scenarios.
- **[Continuous integration]({{< relref "ci/_index.md" >}})** — fan the matrix out across GitHub Actions jobs.
- **[FAQ & troubleshooting]({{< relref "faq.md" >}})** — answers to common questions.

> [!NOTE]
> The **CLI is the supported interface.** The Rust API exists for the project's own binaries and integration tests and carries no stability guarantees.
