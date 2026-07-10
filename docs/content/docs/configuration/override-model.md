---
title: The override model
weight: 3
---

# The override model

Every setting resolves along **one precedence chain**, from broadest to narrowest. Once you know the chain and the three ways to write a value, per-target and per-command configuration are just the same rules applied at a narrower scope.

## The precedence chain

Broadest to narrowest:

```text
workspace → package
  within each scope:  base → subcommands.<cmd> → target.'cfg(...)' → target.'cfg(...)'.subcommands.<cmd>
```

A narrower scope overrides a broader one. So a `target.'cfg(...)'` override beats a bare `subcommands.<cmd>` one, and a package setting beats the workspace.

## The three forms

Wherever a setting is valid, it accepts the same three forms:

**1. Override** — replace the inherited value exactly.

```toml
exclude_features = ["cuda"]                       # array shorthand
exclude_features = { override = ["cuda"] }        # identical, explicit
```

For scalars (`bool`s and `driver`), override is the only operation — it's just `key = value`.

**2. Patch** — incremental edits to a set-like value.

```toml
exclude_features = { add = ["cuda"] }       # union into the inherited value
exclude_features = { remove = ["cuda"] }    # subtract from the inherited value
```

Patches apply in order: **override (or base), then remove, then add.** If a value is in both `add` and `remove`, `add` wins. When several matching sections contribute patches, their `add` and `remove` sets are unioned; conflicting `override` values are an error.

**3. Discard** — `inherit = false` on a section discards everything broader in the chain and starts that section from defaults (the default is `inherit = true`). See [below](#inherit--false).

> [!WARNING]
> **An array is always an override, never an add.** `exclude_features = ["cuda"]` *replaces* the inherited value. To extend it, you must write `{ add = ["cuda"] }`. This is the single most common configuration mistake.

## Where each setting may be overridden

`ws` = workspace, `pkg` = package; `·target`, `·sub` = the target and subcommand refinements.

| Setting | ws | ws·target | ws·sub | ws·tgt·sub | pkg | pkg·target | pkg·sub | pkg·tgt·sub |
|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| `cargo fc` flags | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| feature matrix |  |  |  |  | ✓ | ✓ | ✓ | ✓ |
| `exclude_packages` | ✓ | ✓ | ✓ | ✓ |  |  |  |  |
| `targets` (the list) | ✓ |  | ✓ |  | ✓ |  | ✓ |  |
| `expand_targets` |  |  | ✓ | ✓ |  |  | ✓ | ✓ |
| `driver` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `inherit` |  | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

The blank cells are deliberate:

1. **Feature-matrix keys** are package-only — a workspace isn't a crate and has no features to shape.
2. **`exclude_packages`** is workspace-only — a package can't exclude its siblings; run membership is a workspace decision.
3. **`targets` (the list)** can't be set inside a `target.'cfg(...)'` section — that section was selected *because* a target matched, so redefining the list there would be circular. (Per-subcommand lists are fine.)
4. **`inherit = false`** has nothing to discard at the workspace base, so it isn't allowed there. At a package base it's fine — it discards the inherited workspace config for that package.
5. **`expand_targets`** is a per-subcommand capability, so it only appears in `subcommands.<cmd>` tables. See [Per-command configuration]({{< relref "per-command.md" >}}).

## `inherit = false`

Sections inherit from everything broader by default (`inherit = true`). When a matching section sets `inherit = false`, resolution starts from a fresh default configuration for that section instead of inheriting. To avoid ambiguity, patchable fields in that same section may then only use `override` (arrays), not `add`/`remove`.

```toml
[package.metadata.cargo-fc]
exclude_features = ["default"]
isolated_feature_sets = [["gpu"], ["ui"]]
skip_optional_dependencies = true

[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
inherit = false
# Fresh config on Linux: isolated_feature_sets and skip_optional_dependencies
# are NOT inherited.
exclude_features = ["default", "cuda"]
```

## How feature-matrix layers resolve

For the feature-matrix keys, the package-scope layers apply broad-to-narrow (later wins):

1. package base
2. matching `subcommands.<cmd>`
3. matching `target.'cfg(...)'`
4. matching `target.'cfg(...)'.subcommands.<cmd>`

`cargo fc` flags follow the analogous broad-to-narrow order across workspace and package scopes; see [Flags in config]({{< relref "flags.md" >}}). CLI flags always win last.

---

With this model in hand, the next two pages are just applications of it: [per target]({{< relref "per-target.md" >}}) and [per command]({{< relref "per-command.md" >}}).
