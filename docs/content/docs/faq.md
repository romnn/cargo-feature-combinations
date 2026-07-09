---
title: FAQ & troubleshooting
weight: 9
---

# FAQ & troubleshooting

## How is this different from `--all-features`?

`--all-features` enables every feature at once — a single configuration. `cargo fc` runs *many* configurations: the combinations of your features. That's what catches a conflict between two features that each work alone but not together, or a crate that breaks with `--no-default-features`.

## How is this different from `cargo-all-features`?

Both explore feature combinations. `cargo fc` adds a single readable summary, diagnostics de-duplication, automatic pruning of redundant combinations, a JSON `matrix` for CI, per-target and per-command configuration, and driver-based cross-compilation. The `skip_optional_dependencies` behavior is intentionally compatible.

## My matrix is huge / I hit `max_combinations`.

The powerset grows as `2ⁿ`. Rather than only raising [`max_combinations`]({{< relref "configuration/feature-matrix.md#max_combinations" >}}), shape the matrix:

- [`skip_optional_dependencies`]({{< relref "configuration/feature-matrix.md#skip_optional_dependencies" >}}) if optional deps are inflating it.
- [`isolated_feature_sets`]({{< relref "recipes/large-feature-sets.md" >}}) for independent feature groups.
- [`only_features`]({{< relref "configuration/feature-matrix.md#only_features" >}}) or [`allow_feature_sets`]({{< relref "recipes/restrict-matrix.md" >}}) to restrict what's varied.

## My `target.'cfg(...)'` addition replaced the base instead of extending it.

An array is always an override. Use a patch object:

```toml
# replaces the inherited value
exclude_features = ["cuda"]
# extends the inherited value
exclude_features = { add = ["cuda"] }
```

See [the override model]({{< relref "configuration/override-model.md" >}}).

## `--diagnostics-only` shows nothing for `cargo fc test`.

Diagnostics-only needs a command that emits rustc JSON diagnostics (`build`, `check`, `clippy`, `doc`). `test` and `run` aren't diagnostics-safe, so broad diagnostics defaults are ignored for them. You can opt a specific command in explicitly — see [Flags in config]({{< relref "configuration/flags.md#diagnostics-safety" >}}).

## `cargo fc test` fails for a non-host target.

`test` executes the compiled binary, which can't run a foreign target. Keep `test`/`run` host-only: narrow with `--target`, `--no-targets`, or `expand_targets = false`. See [Configured targets]({{< relref "targets/configured-targets.md" >}}).

## A configured target isn't installed.

By default `cargo fc` won't touch the toolchain. Preinstall the target with `rustup target add`, or pass [`--install-missing-targets`]({{< relref "targets/installing-targets.md" >}}).

## The driver `cargo-zigbuild` is missing.

When a non-host target is planned, `cargo fc` defaults to `cargo-zigbuild` so native-C deps cross-compile. Install `zig` and `cargo-zigbuild`, or force plain cargo with `--driver cargo` (or `driver = "cargo"` in config). See [Build drivers]({{< relref "targets/drivers.md" >}}).

## `CARGO_BUILD_TARGET` is being ignored.

Configured target lists intentionally take precedence over `CARGO_BUILD_TARGET`, so repository config isn't silently collapsed by your environment. Use `--target <triple>` to force a single target for one run, or `--no-targets` to fall back to Cargo's default. See [Configured targets]({{< relref "targets/configured-targets.md#precedence" >}}).

## Can I rely on the Rust API?

No. The CLI is the supported interface. The Rust API exists for the tool's own binaries and integration tests and has no stability guarantees.

## Where's the canonical flag reference?

`cargo fc --help`, mirrored in the [CLI reference]({{< relref "commands/cli-reference.md" >}}).
