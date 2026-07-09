---
title: Build drivers
weight: 2
---

# Build drivers

A "driver" is the program `cargo fc` invokes in place of `cargo` for each build. The default depends on whether you're cross-compiling.

## Why a driver

Cross-compiling a crate with native-C build dependencies (for example `aws-lc-sys`, pulled in via `rustls`) needs a cross C toolchain — the host `cc` can't target another OS. To make that transparent, **when any non-host target is planned, `cargo fc` invokes [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) instead of plain `cargo`**, so zig supplies the cross C compiler and linker for every target.

This means for cross-compilation you need `cargo-zigbuild` and `zig` installed. **Host-only runs use plain `cargo`** and need nothing extra.

## Choosing the driver

Override with `--driver <bin>` or in config:

```toml
[workspace.metadata.cargo-fc]
driver = "cargo-zigbuild"   # the cross-compile default; set "cargo" to opt out
```

`driver` is a normal scalar setting, so it follows the same [precedence chain]({{< relref "../configuration/override-model.md" >}}) as everything else. `cargo fc` launches each package × target × command separately, so each can resolve its own driver:

```toml
[package.metadata.cargo-fc.target.'cfg(target_arch = "wasm32")']
driver = "cargo"            # build wasm for this crate with plain cargo
```

Precedence, narrow wins:

- `--driver` beats all config.
- Within config, a narrower scope beats a broader one.
- Both beat the automatic choice.

Point `--driver` at any cargo wrapper (`cross`, `cargo-careful`, …), or set `cargo` to force plain cargo even when cross-compiling. If the selected driver is missing, `cargo fc` warns with the install/override options before returning the spawn error.

## Interaction with `--aggregate-targets`

`--aggregate-targets` batches a package's targets into one Cargo invocation, which can only use one driver. If a package resolves **different** drivers per target, `cargo fc` runs those targets serially instead.

## `CARGO_DRIVER`

The resolved driver is exported to child processes as the `CARGO_DRIVER` environment variable, so build scripts and wrappers can see which driver was chosen.
