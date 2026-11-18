---
title: Per-command configuration
weight: 5
---

# Per-command configuration

Just as you can override configuration per target triple, you can override it per cargo subcommand:

```toml
[package.metadata.cargo-fc.subcommands.<command>]
```

A subcommand override accepts the **same feature-matrix keys as a target
override** — `exclude_features`, `include_features`, `only_features`,
`mutually_exclusive_features`, `required_feature_groups`, the `*_feature_sets` keys,
`skip_optional_dependencies`, `no_empty_feature_set`, `max_combinations`, and
`matrix` — with identical patch semantics.

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

Environment settings can be command-local too. For example, a crate can
download ONNX Runtime only for checks while other commands keep their normal
environment:

```toml
[package.metadata.cargo-fc.subcommands.check]
env = { add = { ORT_STRATEGY = "download" } }
```

## Aliases inherit command overrides

Built-in short aliases (`t` → `test`, `b` → `build`, …) and your own `.cargo/config.toml` aliases that resolve to a built-in inherit the override automatically, the same way [configured targets]({{< relref "../targets/configured-targets.md" >}}) and flag defaults resolve aliases.

cargo-fc resolves aliases as Cargo does. A user alias may shadow the short forms `b`, `c`, `d`, `r`, `t`, and `rm` and the external `clippy`, but never a full built-in name such as `check`. A shadowing alias that resolves to `build`, `check`, `clippy`, `doc`, `test`, or `run` takes the override, diagnostics mode, and target handling of that command: with `c = "clippy"`, `subcommands.c` applies if present, and otherwise `subcommands.clippy`. One that resolves to any other command, such as `t = "nextest run"`, keeps its built-in meaning, so `cargo fc t` plans as `test` and Cargo runs the alias. Aliases come from `.cargo/config.toml`, looked up from the directory cargo-fc runs in or the one `-C` names, and from `CARGO_ALIAS_*` variables, including those `--env` and `--unset-env` change when commands run (`cargo fc matrix` ignores both). An `env` patch in the manifest cannot redirect an alias, because the patch itself depends on the command the alias selects.

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
> **Scope note.** Feature sets are per package, so the feature-matrix keys are only accepted in **package**-scope subcommand tables. Workspace-scope subcommand tables (`[workspace.metadata.cargo-fc.subcommands.<command>]`) accept the `targets` capability, `expand_targets`, `driver`, `env`, and `cargo fc` flags.
