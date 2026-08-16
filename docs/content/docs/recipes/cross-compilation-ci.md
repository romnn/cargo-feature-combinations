---
title: Cross-compilation in CI
weight: 7
---

# Cross-compilation in CI

**Scenario:** you want to check every feature combination on several target triples, from a single CI job, without a matrix of GitHub Actions jobs.

## Declare the targets

Declare the target list in the workspace `Cargo.toml`:

{{< cargofile "targets" >}}

A single `cargo fc check` then visits every feature combination on every target:

{{< terminal name="targets" >}}

(Add more triples — `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, … — to the `targets` list as needed.)

## Keep host-executing commands host-only

`check` and `clippy` only need each target's `rustc`, so they cross-compile fine. `test` and `run` execute the binary and can't run a foreign target — keep them host-only:

```toml
[workspace.metadata.cargo-fc.subcommands.test]
expand_targets = false
```

## Lint everything in one invocation

```yaml
- uses: actions/checkout@v4
- uses: dtolnay/rust-toolchain@stable
  with:
    targets: x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, wasm32-unknown-unknown
- uses: romnn/cargo-feature-combinations@main
- run: cargo fc clippy
```

A single `cargo fc clippy` iterates every configured target and feature combination.

## Native-C dependencies

If a target pulls in native-C build dependencies, `cargo fc` uses [`cargo-zigbuild`]({{< relref "../targets/drivers.md" >}}) automatically for non-host targets. Install `zig` and `cargo-zigbuild` on the runner, or override the driver.

## Features that can't cross-compile

Some feature-gated toolchains only build natively — nvcc has no cross setup, Metal shader compilation needs a macOS host. Don't encode the runner's architecture into the manifest (`cfg(not(target_arch = "x86_64"))` is wrong the day a native aarch64 runner joins); subtract those rows only where they are actually cross-compiled, with the [`cfg(cross)` selector]({{< relref "../configuration/per-target.md#cfgcross" >}}):

```toml
[package.metadata.cargo-fc.target.'cfg(cross)']
exclude_features = { add = ["cuda"] }
```

Every runner then keeps its own native rows: the x86_64 job checks `cuda` on x86_64, an aarch64 job checks it on aarch64, and each skips the other's. The excluded rows only exist on a native runner of that target, so cover each such platform with its own job.

## Throughput

Add `--aggregate-targets` to batch each combination's targets into one Cargo invocation on many-core runners:

```yaml
- run: cargo fc clippy --aggregate-targets
```

For fanning targets out across separate jobs instead, see [Continuous integration]({{< relref "../ci/_index.md" >}}).
