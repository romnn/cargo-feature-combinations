---
title: Output modes
weight: 2
---

# Output modes

A full matrix run can print a lot. These flags control how much you see. They are CLI flags, but most can also be set as [defaults in `Cargo.toml`]({{< relref "../configuration/flags.md" >}}).

## `--diagnostics-only`

Show only warnings and errors per combination, suppressing build progress and "Finished" chatter.

```bash
cargo fc --diagnostics-only clippy
```

{{< terminal name="diagnostics" >}}

Cargo's own warnings, such as the manifest lints `cargo::redundant_homepage` and `cargo::unused_workspace_dependencies`, are shown and counted too.

The command must accept `--message-format` and emit rustc JSON diagnostics. That is true for `build`, `check`, `clippy`, and `doc` — and any alias or wrapper that does the same. It is **not** safe for `test` or `run`, which is why broad diagnostics defaults don't apply to them automatically.

A `--message-format` you pass to the command wins over `--diagnostics-only`. With a JSON format other than `json-render-diagnostics`, Cargo prints compiler warnings as JSON on stdout. For `build` and `check`, cargo-fc relays that stdout unchanged, even under `--summary-only`, and counts the warnings in it. Other commands keep stdout to themselves, so the warnings they print as JSON are not counted: `test` and `run` print program output there, and an alias may shadow short forms such as `c` and external subcommands such as `clippy`. For build-only commands such as `clippy`, use `--diagnostics-only` to count their warnings instead.

## `--dedupe`

Like `--diagnostics-only`, but also collapses identical diagnostics that repeat across combinations, so a warning present in twenty combinations is shown once.

```bash
cargo fc --dedupe clippy
```

{{< terminal name="dedupe" >}}

`--dedupe` implies `--diagnostics-only`.

## `--summary-only`

Hide cargo output entirely and print only the final per-combination result table.

```bash
cargo fc --summary-only check
```

{{< terminal name="summary" >}}

## `--fail-fast`

Stop at the first combination that fails instead of running the whole matrix. Useful locally when you just want the first problem.

```bash
cargo fc --fail-fast test
```

## `--pedantic`

Treat warnings as failures in the summary and under `--fail-fast`. A combination with warnings but no errors is then reported as failing.

Warnings count whether rustc or Cargo itself reports them. Cargo's own warnings, such as manifest lints, count when Cargo summarizes them with a `generated N warnings` line; ones it prints without such a line, such as a build script's `cargo::warning`, do not.

```bash
cargo fc --pedantic --fail-fast clippy
```

## `--errors-only`

Allow all warnings and surface errors only (equivalent to `-A warnings`). This appends to the effective child `RUSTFLAGS` / `CARGO_ENCODED_RUSTFLAGS`, including values from cargo-fc [`env` config]({{< relref "../configuration/environment.md" >}}). Like any `RUSTFLAGS` environment override, the result shadows `rustflags` set in Cargo config files.

```bash
cargo fc --errors-only check
```

## Pruning: `--show-pruned` and `--prune-implied`

`cargo fc` prunes redundant feature combinations by default (you never need to enable this). `--show-pruned` includes them in the summary marked `SKIP`; `--prune-implied=false` disables pruning for the run:

```bash
cargo fc --show-pruned check
cargo fc --prune-implied=false check
```

See [Automatic pruning]({{< relref "../configuration/feature-matrix.md#automatic-pruning" >}}) for what it does and a worked example.

## Combining flags

Flags compose. A common local loop:

```bash
# Fast feedback: first failure only, warnings-as-errors, deduped diagnostics
cargo fc --dedupe --pedantic --fail-fast clippy
```

For the exhaustive list, see the [CLI reference]({{< relref "cli-reference.md" >}}).
