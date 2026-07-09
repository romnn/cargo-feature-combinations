---
title: Per-command differences
weight: 6
---

# Build a feature you never test

**Scenario:** a `renderer` crate has a heavy `gpu` feature. It should be compiled to catch build breakage, but its tests need a GPU that CI doesn't have. You want `cargo fc build` to include `gpu`, but `cargo fc test` to skip it.

{{< cargofile "docs/recipes/per-command" >}}

`cargo fc build` varies both features — four combinations, including the `gpu` ones:

{{< terminal name="recipe-per-cmd-build" >}}

`cargo fc test` applies the `subcommands.test` override and drops every combination containing `gpu` — two combinations:

{{< terminal name="recipe-per-cmd-test" >}}

The override applies to `test` only; `build`, `check`, and `clippy` keep the full matrix. Built-in short aliases (`t` → `test`) and `.cargo/config.toml` aliases that resolve to `test` inherit it automatically.

## Restrict a single command instead

To make `cargo fc test` focus on one set while other commands keep the full matrix:

```toml
[package.metadata.cargo-fc.subcommands.test]
only_features = ["simd"]
```

## Command-specific on a specific target

Compose target and command — this applies only when **both** match:

```toml
[package.metadata.cargo-fc.target.'cfg(target_os = "linux")'.subcommands.test]
exclude_features = { add = ["cuda"] }
```

See [Per-command configuration]({{< relref "../configuration/per-command.md" >}}).
