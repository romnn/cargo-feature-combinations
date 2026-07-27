---
title: Running commands
weight: 1
---

# Running commands

Use `cargo fc` as if it were `cargo`. The command and its arguments are forwarded to each combination's invocation:

```bash
cargo fc check
cargo fc clippy
cargo fc build
cargo fc test
cargo fc doc
```

## Forwarded arguments

Every cargo argument is passed through **except** the three that would conflict with the matrix `cargo fc` builds for you:

- `--features`
- `--all-features`
- `--no-default-features`

cargo-fc passes `--no-default-features` to every invocation, then enables the features in the
current row. The empty row (`[]`) therefore enables no features. A row containing `default`
reproduces an ordinary Cargo default-feature invocation; do not exclude `default` when the matrix
must cover that configuration.

Everything else is forwarded verbatim, so this works as expected:

```bash
cargo fc check -p my-crate --all-targets
cargo fc test --release -- --nocapture
```

Arguments after `--` are passed to the invoked program (for example the test binary), never interpreted by `cargo fc`.

## Running only maximal feature sets

Use `--maximal-features` when a command needs broad feature reachability but not every feature
interaction as a separate invocation. Cargo-fc keeps only the maximal feature sets of each
package-target matrix: combinations that are not a subset of another generated combination. In an
unconstrained matrix the full feature set is itself a combination, so everything collapses into a
single all-features run. Mutually exclusive features, excluded combinations, isolated feature sets,
and target-specific feature rules stay in force and keep one run per remaining maximal alternative.
The same collapse applies to `cargo fc matrix` output.

Unused-dependency auditing with
[`cargo-udeps`](https://github.com/est31/cargo-udeps) is a useful example:

```bash
cargo +nightly fc udeps --maximal-features --all-targets -p my-crate
```

This makes dependencies behind valid feature gates visible without Cargo's naive `--all-features`,
which may combine backends or platform features that the matrix deliberately keeps apart. Cargo-fc
has no udeps-specific behavior: it only supplies the generated
`--no-default-features --features=...` arguments and forwards the custom command.

Maximal mode is not a replacement for an exhaustive matrix when feature interactions are the thing
being tested. Keep ordinary `cargo fc check`, `clippy`, and `test` runs for that coverage.

## Package selection

In a workspace, `cargo fc` runs the selected packages. Use cargo's own selection flags:

```bash
# One package
cargo fc check -p engine

# The whole workspace, excluding one package
cargo fc check --workspace --exclude examples
```

`--exclude` is accepted for Cargo-compatible workspace selection. You can also exclude packages permanently in workspace metadata — see [Configuration basics]({{< relref "../configuration/basics.md" >}}).

## Toolchains

A leading `+toolchain` works exactly like it does with cargo:

```bash
cargo fc +nightly check
```

The toolchain is forwarded to every invocation. When `cargo fc` installs missing target components (see [Installing targets]({{< relref "../targets/installing-targets.md" >}})), it passes the same override to `rustup`.

## Built-in commands and aliases

`cargo fc` recognizes these built-in cargo commands, including their short aliases: `build` (`b`), `check` (`c`), `clippy`, `doc` (`d`), `test` (`t`), and `run` (`r`). Recognized commands automatically gain capabilities such as [configured targets]({{< relref "../targets/configured-targets.md" >}}).

`cargo fc` also resolves aliases from your `.cargo/config.toml` before running. If an alias expands to a built-in, it inherits that built-in's behavior:

```toml
# .cargo/config.toml
[alias]
lint = "clippy --all-targets --no-deps"
```

```bash
# Behaves like `cargo fc clippy`
cargo fc lint
```

A custom command that does **not** resolve to a built-in runs across the matrix all the same, but you must opt in explicitly to give it target or diagnostics capabilities. See [Per-command configuration]({{< relref "../configuration/per-command.md" >}}).

## Pointing at another project

For local development you can inspect a manifest elsewhere:

```bash
cargo run -- cargo check --manifest-path ../other/Cargo.toml
cargo run -- cargo matrix --manifest-path ../other/Cargo.toml --pretty
```
