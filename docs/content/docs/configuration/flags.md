---
title: Flags in config
weight: 7
---

# `cargo fc` flags in config

Most `cargo fc` boolean flags can be set as defaults in `Cargo.toml`, using the flag name with `_` instead of `-`. Explicit CLI flags still win for a single invocation.

```toml
[workspace.metadata.cargo-fc]
dedupe = true
fail_fast = true

[package.metadata.cargo-fc]
pedantic = false

[package.metadata.cargo-fc.target.'cfg(target_os = "windows")']
errors_only = true

[package.metadata.cargo-fc.target.'cfg(target_os = "windows")'.subcommands.clippy]
dedupe = false

[workspace.metadata.cargo-fc.subcommands.my-custom-cmd]
dedupe = true
```

## Configurable flag keys

```toml
summary_only = true
diagnostics_only = true
dedupe = true
verbose = true
pedantic = true
errors_only = true
packages_only = true
fail_fast = true
prune_implied = true
show_pruned = true
maximal_features = true
aggregate_targets = true
no_targets = true
install_missing_targets = true
only_packages_with_lib_target = true
omit_host_target_flag = false
```

- `dedupe = true` implies diagnostics-only output.
- `prune_implied = false` turns off [automatic pruning]({{< relref "feature-matrix.md#automatic-pruning" >}}).
- `maximal_features = true` runs [only maximal feature sets]({{< relref "../commands/running-commands.md#running-only-maximal-feature-sets" >}}), resolved per package-target like every other flag.
- `omit_host_target_flag` is the one key that defaults to `true`; set it to `false` to [inject `--target` for the host]({{< relref "../targets/configured-targets.md#the-host-target-runs-as-a-plain-build" >}}) too.
- `no_targets` from config never lifts the [package-level `targets` filter]({{< relref "../targets/configured-targets.md#per-package-target-lists" >}}) for an explicit `--target`; only the command-line `--no-targets` forces that.

## Precedence

Flags resolve broad-to-narrow, with CLI flags last:

1. workspace config
2. matching workspace target config
3. package config
4. matching package target config
5. explicit CLI flags

At each config level, a matching `subcommands.<name>` table is applied **after** that level's plain flags, so command-specific defaults override broader ones. Alias config for the raw command token wins; otherwise `cargo fc` uses the resolved alias target.

## Diagnostics safety

Broad, config-driven diagnostics output only applies to commands where diagnostics-only mode is safe by default:

- **Safe** (get broad `diagnostics_only = true` and `dedupe = true` when configured): built-in `build`, `check`, `clippy`, `doc`, and aliases that resolve to them.
- **Not safe by default**: `test`, `run`, and unresolved custom commands — they aren't reliable JSON-diagnostics commands, so broad diagnostics defaults are ignored for them (silently for well-known cargo plugins, otherwise with a warning).

To opt a non-safe command in, set the behavior in that command's own table — subcommand-local diagnostics flags are explicit and honored even for commands that aren't safe by default:

```toml
[workspace.metadata.cargo-fc.subcommands.my-custom-cmd]
dedupe = true
```

`dedupe = true` implies `diagnostics_only = true`; setting `dedupe = true` together with `diagnostics_only = false` is rejected as contradictory. Use `diagnostics_only = false` or `dedupe = false` in a narrower scope to override a broader default.

## Notes

- Any default configured here can be turned back off for one invocation with an inline value on the matching CLI flag: `--summary-only=false`, `--dedupe=off`, `--omit-host-target-flag=0`. See the [CLI reference]({{< relref "../commands/cli-reference.md" >}}).
- `verbose` can also be set with the `CARGO_FC_VERBOSE` environment variable (see the [CLI reference]({{< relref "../commands/cli-reference.md" >}})).
- `--env` and `--unset-env` override the resolved [child-process environment]({{< relref "environment.md" >}}); they are value options rather than boolean flag defaults.
