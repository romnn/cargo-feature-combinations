---
title: Flags in config
weight: 6
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
no_prune_implied = true
prune_implied = true
show_pruned = true
aggregate_targets = true
no_targets = true
install_missing_targets = true
only_packages_with_lib_target = true
```

- `dedupe = true` implies diagnostics-only output.
- `prune_implied` is the positive spelling of `no_prune_implied`; configure only one spelling in a given scope.

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

- Inline values for boolean CLI flags are rejected (`--summary-only=false` fails). Configure false defaults in `Cargo.toml` instead.
- `verbose` can also be set with the `CARGO_FC_VERBOSE` environment variable (see the [CLI reference]({{< relref "../commands/cli-reference.md" >}})).
