---
title: Introduction
weight: 1
---

# Introduction

`cargo-feature-combinations` is a plugin for `cargo` that runs a command against selected — or all — combinations of a crate's features. You invoke it as **`cargo fc`**.

## The problem

Cargo features are meant to be additive, but real crates rarely behave that way. A crate can:

- compile with its `default` features but fail with `--no-default-features`,
- compile with each feature alone but fail when two are enabled together,
- pass `cargo build` yet fail `cargo test` for a particular combination,
- compile on your host but not when cross-compiled.

Checking only the default set, or only `--all-features`, hides every one of these cases. `--all-features` in particular enables everything at once, which is exactly the combination least likely to expose a conflict between two mutually-exclusive features.

## The approach

`cargo fc` enumerates the combinations of your features, runs a `cargo` command for each one, and prints a single summary of the results:

{{< terminal name="check" >}}

By default the matrix is the powerset of your features. You then **shape** it from `Cargo.toml` — excluding combinations that don't make sense, restricting it to an allowlist, or pinning a few exact sets. See [Configuration]({{< relref "configuration/_index.md" >}}).

## What you can do with it

| Goal | How |
|---|---|
| Run any cargo command across the matrix | `cargo fc <command>` — see [Running commands]({{< relref "commands/running-commands.md" >}}) |
| Cut output down to warnings and errors | `--diagnostics-only`, `--dedupe` — see [Output modes]({{< relref "commands/output-modes.md" >}}) |
| Emit a JSON matrix for CI | `cargo fc matrix` — see [The matrix subcommand]({{< relref "commands/matrix.md" >}}) |
| Shape which combinations run | [Feature matrix configuration]({{< relref "configuration/feature-matrix.md" >}}) |
| Check every combination on every target | [Configured targets]({{< relref "targets/configured-targets.md" >}}) |

## The `cargo fc` interface

`cargo fc` behaves like `cargo`: it forwards all cargo arguments through to the underlying command, with three exceptions it manages itself because they define the matrix — `--all-features`, `--features`, and `--no-default-features`.

On top of cargo's arguments it adds its own flags (such as `--dedupe` and `--fail-fast`) and the `matrix` subcommand. Those are covered in the [CLI reference]({{< relref "commands/cli-reference.md" >}}).

> [!NOTE]
> **Supported surface.** The CLI is the supported, stable interface. The crate also exposes a Rust API, but it exists only for the tool's own binaries and integration tests and has no stability guarantees — do not depend on it.
