---
title: Per-command configuration
weight: 5
---

# Per-command configuration

Just as you can override configuration per target triple, you can override it per cargo subcommand:

```toml
[package.metadata.cargo-fc.subcommands.<command>]
```

A subcommand override accepts the **same feature-matrix keys as a target override** — `exclude_features`, `include_features`, `only_features`, the `*_feature_sets` keys, `skip_optional_dependencies`, `no_empty_feature_set`, and `matrix` — with identical patch semantics.

## Example: build it, but don't test it

Enable a heavy `gpu` feature when building, but skip those combinations when testing:

```toml
[package.metadata.cargo-fc]
# `gpu` is part of the matrix by default (e.g. for `cargo fc build`).

[package.metadata.cargo-fc.subcommands.test]
# ...but never test the gpu combinations.
exclude_features = { add = ["gpu"] }
```

Or restrict `cargo fc test` to a single focused set:

```toml
[package.metadata.cargo-fc.subcommands.test]
only_features = ["core"]
```

The override applies to that command only.

## Aliases inherit command overrides

Built-in short aliases (`t` → `test`, `b` → `build`, …) and your own `.cargo/config.toml` aliases that resolve to a built-in inherit the override automatically, the same way [configured targets]({{< relref "../targets/configured-targets.md" >}}) and flag defaults resolve aliases.

## Composing target and command

A `target.'cfg(...)'.subcommands.<command>` section applies only when **both** conditions hold — the target matches *and* the command is selected:

```toml
[package.metadata.cargo-fc.target.'cfg(target_os = "linux")'.subcommands.test]
exclude_features = { add = ["cuda"] }
```

The feature-matrix layers then resolve broad-to-narrow (later wins):

1. package base
2. matching `subcommands.<command>`
3. matching `target.'cfg(...)'`
4. matching `target.'cfg(...)'.subcommands.<command>`

## `expand_targets`: giving a custom command target capability

Built-in commands (`check`, `clippy`, `build`, `doc`, `test`, `run`, and `matrix`) already know how to expand [configured targets]({{< relref "../targets/configured-targets.md" >}}). A custom subcommand that doesn't resolve to a built-in needs an explicit opt-in:

```toml
[workspace.metadata.cargo-fc.subcommands.my-custom-cmd]
expand_targets = true
```

You can also override a built-in's default. For example, lint every configured target but keep `build` on the single effective target:

```toml
[workspace.metadata.cargo-fc.subcommands.build]
expand_targets = false
```

`expand_targets` is a per-subcommand capability — it only appears in `subcommands.<cmd>` tables.

> [!NOTE]
> **Scope note.** Feature sets are per package, so the feature-matrix keys are only accepted in **package**-scope subcommand tables. Workspace-scope subcommand tables (`[workspace.metadata.cargo-fc.subcommands.<command>]`) accept only the `targets` capability, `expand_targets`, `driver`, and `cargo fc` flags.
