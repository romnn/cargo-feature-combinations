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

## Throughput

Add `--aggregate-targets` to batch each combination's targets into one Cargo invocation on many-core runners:

```yaml
- run: cargo fc clippy --aggregate-targets
```

For fanning targets out across separate jobs instead, see [Continuous integration]({{< relref "../ci/_index.md" >}}).
