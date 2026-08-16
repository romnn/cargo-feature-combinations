---
title: Build drivers
weight: 2
---

# Build drivers

A "driver" is the program `cargo fc` invokes in place of `cargo` for each build. The default depends on the target being built.

## Why a driver

Cross-compiling a crate with native-C build dependencies (for example `aws-lc-sys`, pulled in via `rustls`) needs a cross C toolchain — the host `cc` can't target another OS. To make that transparent, **`cargo fc` chooses the driver per target: the host target builds with plain `cargo`, every non-host target with [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild)**, so zig supplies the cross C compiler and linker exactly where one is needed.

This means for cross-compilation you need `cargo-zigbuild` and `zig` installed. **Host-only runs use plain `cargo`** and need nothing extra. The automatic choice verifies `cargo-zigbuild --version` succeeds before using it; a missing *or broken* installation (a tool-manager shim with no version configured, say) falls back to plain cargo with a warning that includes the probe's stderr.

The choice is per target rather than per run for a reason: a driver that rewrites the C and linker environment changes the fingerprints of every unit that reads it. Driving the host target through zig just because some *other* target in the same run needs it would make `cargo fc` and everyday `cargo check` / `cargo test` invalidate each other's artifacts on every switch, even though both build the same target into the same directory.

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

The automatic host-vs-cross rule is itself addressable as the [`cfg(cross)` selector]({{< relref "../configuration/per-target.md#cfgcross" >}}) — it matches exactly the targets that would fall back to the cross driver, so `target.'cfg(cross)'` sections can pin a driver (or any other setting) for cross rows only, on whatever machine the matrix runs:

```toml
[workspace.metadata.cargo-fc.target.'cfg(cross)']
driver = "cross"            # use cross-rs instead of cargo-zigbuild for cross rows
```

Precedence, narrow wins:

- `--driver` beats all config.
- Within config, a narrower scope beats a broader one.
- Both beat the automatic choice.

Point `--driver` at any cargo wrapper (`cross`, `cargo-careful`, …), or set `cargo` to force plain cargo even when cross-compiling. If the selected driver is missing, `cargo fc` warns with the install/override options before returning the spawn error.

## Interaction with `--aggregate-targets`

`--aggregate-targets` batches compatible package-targets into one Cargo invocation. If a package resolves **different** drivers per target, cargo-fc keeps aggregate mode but splits those targets into separate per-driver invocations. The resolved child environment is part of the same compatibility key.

## `CARGO_DRIVER`

The resolved driver is exported to child processes as the `CARGO_DRIVER` environment variable, so build scripts and wrappers can see which driver was chosen. Explicit [child environment]({{< relref "../configuration/environment.md" >}}) config or CLI overrides win, including removal.
