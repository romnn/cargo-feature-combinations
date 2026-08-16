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
exclude_features = ["experimental"]

[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
exclude_features = { add = ["metal"] }

[package.metadata.cargo-fc.target.'cfg(target_os = "macos")']
exclude_features = { add = ["cuda"] }
```

The base excludes `experimental` everywhere. On Linux, `metal` is *also* excluded (the `add` unions into the inherited value); on macOS, `cuda` is. Remember: an array like `exclude_features = ["metal"]` would have **replaced** the base instead of extending it.

## Example: remove a host library path while cross-compiling

Environment patches use the same target selectors. This removes a host-only
ONNX Runtime path from every non-Linux child Cargo invocation, including when
the value exists only in the ambient environment:

```toml
[workspace.metadata.cargo-fc.target.'cfg(not(target_os = "linux"))']
env = { remove = ["ORT_LIB_PATH"] }
```

See [Child-process environment]({{< relref "environment.md" >}}) for the map
patch grammar and its interaction with cargo-fc's own variables.

## Patch semantics recap

Collection-like keys — `exclude_features`, `include_features`, `only_features`,
`mutually_exclusive_features`, and the `*_feature_sets` keys — take:

- `key = [...]` or `{ override = [...] }` — replace the inherited value.
- `{ add = [...] }` — union with the inherited value.
- `{ remove = [...] }` — subtract from the inherited value.

Applied in order: override (or base), then remove, then add; `add` wins ties. When multiple `cfg(...)` sections match (e.g. both `cfg(unix)` and `cfg(target_os = "linux")`), their `add`/`remove` sets are unioned. Conflicting `override` values are an error.

Matrix metadata tables merge recursively; other metadata values, including arrays, replace.

## Which selector matches

A section applies when its `cfg(...)` predicate matches the concrete target being resolved. Predicates are evaluated against the real `rustc --print cfg` output for that target, so all `target_*` predicates (`target_os`, `target_arch`, `target_env`, `target_family`, `target_vendor`, `target_abi`, `target_endian`, `target_pointer_width`, `target_has_atomic`, `target_feature`) and the bare `unix` / `windows` family shorthands work, including for custom targets. If `--target <triple>` or `CARGO_BUILD_TARGET` is set, that value selects matching overrides — this also applies to `cargo fc matrix`.

Two spellings are rejected as hard errors:

- `cfg(feature = "...")` — feature selection is the very thing cargo-fc varies.
- Any bare flag other than [`cross`](#cfgcross) — a typo like `cfg(corss)` would otherwise silently match nothing and disable the override without a diagnostic. <!-- spellcheck:ignore-line -->

## `cfg(cross)`

`cross` is a cargo-fc predicate, not a rustc cfg: it matches exactly when the section's evaluated target differs from the rustc host — the same rule the [automatic driver default]({{< relref "../targets/drivers.md" >}}) uses to decide between plain `cargo` and `cargo-zigbuild`. Use it for policy about cross-compilation itself, which no `target_*` predicate can express because it depends on the machine running the matrix, not on the target alone:

```toml
# nvcc cannot cross-compile: drop the CUDA rows only when the target is not
# the machine doing the build. A native aarch64 host with the CUDA toolkit
# keeps its cuda rows; an x86_64 host cross-checking aarch64 drops them.
[package.metadata.cargo-fc.target.'cfg(cross)']
exclude_features = { add = ["cuda"] }

# Composes with target predicates: compiling Metal shaders needs a macOS
# host, but type-checking the macOS rows from elsewhere works with stubs.
[package.metadata.cargo-fc.target.'cfg(all(target_os = "macos", cross))']
env = { add = { MISTRALRS_METAL_PRECOMPILE = "0" } }
```

Because `cross` depends on the host, a matrix that uses it is machine-relative: rows excluded under `cfg(cross)` run only on a native host of that target. That is the point — the same checked-in manifest does the right thing on every machine — but it means full coverage of such rows needs one runner per native platform; one machine's green run is not the whole matrix.

## `inherit = false`

Sections inherit the base by default (`inherit = true`). A matching target section can set `inherit = false` to start from a fresh default config instead. When it does, set/list patch fields in that section may only use `override` (arrays), not `add`/`remove`; `env.add` and `env.remove` remain valid because they patch the ambient child environment:

```toml
[package.metadata.cargo-fc]
exclude_features = ["experimental"]
skip_optional_dependencies = true

[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
inherit = false
exclude_features = ["experimental", "cuda"]   # fresh config; nothing inherited
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
