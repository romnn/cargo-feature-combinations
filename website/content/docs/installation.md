---
title: Installation
weight: 2
---

# Installation

`cargo-feature-combinations` installs a `cargo` subcommand. Once it's on your `PATH`, `cargo fc` works from any crate or workspace.

## From crates.io

```bash
cargo install --locked cargo-feature-combinations
```

`--locked` builds against the versions in the published `Cargo.lock`, which is the most reproducible option.

## Homebrew

A prebuilt binary is available through the author's tap, which avoids compiling from source:

```bash
brew install --cask romnn/tap/cargo-fc
```

## Nix

There is an **unofficial**, community-maintained package (not maintained by the author of the tool):

```bash
nix-shell --packages cargo-feature-combinations
```

## Verify the installation

```bash
cargo fc version
# or
cargo fc --help
```

If `cargo fc` is not found, confirm that Cargo's binary directory (`~/.cargo/bin` by default) is on your `PATH`.

## Requirements

- **Rust and Cargo.** `cargo fc` drives your normal toolchain; use `+toolchain` just as you would with cargo (for example `cargo fc +nightly check`).
- **`cargo-zigbuild` and `zig`** — only if you cross-compile. When a non-host target is planned, `cargo fc` defaults to the `cargo-zigbuild` driver so native-C dependencies cross-compile cleanly. Host-only runs use plain `cargo` and need nothing extra. See [Build drivers]({{< relref "targets/drivers.md" >}}).

## Use in GitHub Actions

In CI you usually don't `cargo install`; use the setup action instead, which downloads a released binary:

```yaml
- uses: romnn/cargo-feature-combinations@main
- run: cargo fc check
```

See [Continuous integration]({{< relref "ci/_index.md" >}}) for complete workflows.
