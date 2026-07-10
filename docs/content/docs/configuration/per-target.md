---
title: Per-target configuration
weight: 4
---

# Per-target configuration

Override configuration for specific targets using Cargo-style `cfg(...)` selectors. This uses the same forms and precedence as everything else — see [the override model]({{< relref "override-model.md" >}}) — applied at a narrower scope.

Overrides live under:

```toml
[package.metadata.cargo-fc.target.'cfg(...)']
```

## Example: different features per OS

```toml
[package.metadata.cargo-fc]
exclude_features = ["default"]

[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
exclude_features = { add = ["metal"] }

[package.metadata.cargo-fc.target.'cfg(target_os = "macos")']
exclude_features = { add = ["cuda"] }
```

The base excludes `default` everywhere. On Linux, `metal` is *also* excluded (the `add` unions into the inherited value); on macOS, `cuda` is. Remember: an array like `exclude_features = ["metal"]` would have **replaced** the base instead of extending it.

## Patch semantics recap

Collection-like keys — `exclude_features`, `include_features`, `only_features`, and the `*_feature_sets` keys — take:

- `key = [...]` or `{ override = [...] }` — replace the inherited value.
- `{ add = [...] }` — union with the inherited value.
- `{ remove = [...] }` — subtract from the inherited value.

Applied in order: override (or base), then remove, then add; `add` wins ties. When multiple `cfg(...)` sections match (e.g. both `cfg(unix)` and `cfg(target_os = "linux")`), their `add`/`remove` sets are unioned. Conflicting `override` values are an error.

Matrix metadata tables merge recursively; other metadata values, including arrays, replace.

## Which selector matches

A section applies when its `cfg(...)` predicate matches the concrete target being resolved. `cfg(feature = "...")` predicates are **not** supported in target-override keys. If `--target <triple>` or `CARGO_BUILD_TARGET` is set, that value selects matching overrides — this also applies to `cargo fc matrix`.

## `inherit = false`

Sections inherit the base by default (`inherit = true`). A matching target section can set `inherit = false` to start from a fresh default config instead. When it does, patchable fields in that section may only use `override` (arrays), not `add`/`remove`:

```toml
[package.metadata.cargo-fc]
exclude_features = ["default"]
skip_optional_dependencies = true

[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
inherit = false
exclude_features = ["default", "cuda"]   # fresh config; nothing inherited
```

## Workspace target overrides

Workspace target sections can patch `exclude_packages` and set flag defaults for matching targets, using the same `cfg(...)` selectors:

```toml
[workspace.metadata.cargo-fc]
targets = ["x86_64-unknown-linux-gnu", "wasm32-unknown-unknown"]

[workspace.metadata.cargo-fc.target.'cfg(target_arch = "wasm32")']
exclude_packages = { add = ["native-cli"] }

[workspace.metadata.cargo-fc.target.'cfg(target_os = "linux")']
exclude_packages = { add = ["wasm-app"] }
fail_fast = false
```

These apply to every concrete effective target, including single-target runs selected by `--target`, `CARGO_BUILD_TARGET`, or the host.

## Combining with commands

A `target.'cfg(...)'.subcommands.<command>` section applies only when **both** the target matches and the command is selected — see [Per-command configuration]({{< relref "per-command.md" >}}).
