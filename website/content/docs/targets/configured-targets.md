---
title: Configured targets
weight: 1
---

# Configured targets

## Declaring a target list

Declare workspace-wide targets in the workspace `Cargo.toml`:

```toml
[workspace.metadata.cargo-fc]
targets = [
  "x86_64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc",
  "aarch64-apple-darwin",
]
```

Now `cargo fc check` visits every combination on every target.

## Per-package target lists

A package can override the workspace list, or opt out of it:

```toml
[package.metadata.cargo-fc]
# Run this package only on wasm (overrides the workspace list, does not merge).
targets = ["wasm32-unknown-unknown"]
```

The three states:

| Value | Meaning |
|---|---|
| key missing | inherit the workspace target list |
| `targets = []` | opt out of the workspace list; use the single effective target |
| `targets = ["…"]` | this package's own list (overrides, does not merge) |

`targets` only selects which targets are visited. The [`target.'cfg(...)'`]({{< relref "../configuration/per-target.md" >}}) overrides still shape the feature matrix for each concrete target.

## Precedence

For a command that supports targets, each package's target is resolved as:

1. an explicit Cargo `--target <triple>` (wins globally for the run),
2. the package's `targets`,
3. the workspace `targets`,
4. `CARGO_BUILD_TARGET`,
5. the host target.

> [!WARNING]
> **Configured lists intentionally beat `CARGO_BUILD_TARGET`.** Repository config is the declarative matrix and shouldn't be silently collapsed by a developer's ambient environment — this differs from Cargo's own `[build].target` precedence. To run a single target for one invocation, pass `--target <triple>` (overrides all configured lists) or `--no-targets` (ignore configured lists, fall back to Cargo's default single target).

## Which commands expand targets

Configured targets apply only to commands that accept Cargo's `--target` flag. The built-ins `check`, `clippy`, `build`, `doc`, `test`, `run`, and `cargo fc matrix` get this automatically. Aliases that resolve to a built-in inherit it.

A custom command that doesn't resolve to a built-in must opt in:

```toml
[workspace.metadata.cargo-fc.subcommands.my-custom-cmd]
expand_targets = true
```

The same table overrides built-in defaults — for example, lint every target but keep `build` host-only:

```toml
[workspace.metadata.cargo-fc.subcommands.build]
expand_targets = false
```

Well-known plugins (`nextest`, `audit`, `deny`, `machete`, `udeps`, `leptos`, …) have their capability hint suppressed to avoid noise; this does not grant capability. Opt in or out explicitly with the same `subcommands.<name>` table. If configured targets exist but the command lacks the capability, `cargo fc` warns once and falls back to the single effective target; an explicit `expand_targets = false` is quiet.

> [!CAUTION]
> **`test` and `run` execute the binary.** The `targets` list is shared by all target-capable commands, but `check`/`clippy` only need the target's `rustc`, while `test`/`run` **run** the produced binary and can't execute a foreign target. Keep them host-only — narrow with `--target` or `--no-targets`.

## Per-target package selection

Workspace package exclusions can vary by target, using the same `cfg(...)` selectors and patch semantics:

```toml
[workspace.metadata.cargo-fc]
targets = ["x86_64-unknown-linux-gnu", "wasm32-unknown-unknown"]

[workspace.metadata.cargo-fc.target.'cfg(target_arch = "wasm32")']
exclude_packages = { add = ["native-cli"] }

[workspace.metadata.cargo-fc.target.'cfg(target_os = "linux")']
exclude_packages = { add = ["wasm-app"] }
```

## Throughput: `--aggregate-targets`

`--aggregate-targets` batches a combination's configured targets into a single Cargo invocation (one `--target` per target) instead of one invocation per target. It's faster on many-core machines and reports results per target group. It falls back to serial execution for `run`, for pruned summaries, and when a package resolves different [drivers]({{< relref "drivers.md" >}}) per target.

Each row now covers a combination's whole target group — note `targets = [...]` (plural), and one invocation per combination instead of one per target:

{{< terminal name="aggregate-targets" >}}
