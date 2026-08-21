---
title: CLI reference
weight: 4
---

# CLI reference

The authoritative reference is `cargo fc --help`. This page mirrors it.

## Usage

```text
cargo fc [+toolchain] [SUBCOMMAND] [SUBCOMMAND_OPTIONS]
cargo fc [+toolchain] [OPTIONS] [CARGO_OPTIONS] [CARGO_SUBCOMMAND]
```

## Subcommands

| Subcommand | Description |
|---|---|
| `matrix` | Print the JSON feature-combination matrix to stdout. Add `--pretty` for indented JSON. |
| `version` | Print version information. |

## Options

| Flag | Description |
|---|---|
| `-h`, `--help` | Print help information. |
| `-V`, `--version` | Print version information. |
| `--manifest-path <path>` | Path to the `Cargo.toml` to inspect. |
| `-p`, `--package <name>` | Include only this workspace package (repeatable). |
| `--exclude <name>` | Exclude a workspace package (repeatable). Pairs with `--workspace` for Cargo-compatible selection. |
| `--diagnostics-only` | Show only diagnostics (warnings/errors) per combination. Requires a command that emits rustc JSON diagnostics (`build`, `check`, `clippy`, `doc`, or an equivalent alias/wrapper). |
| `--dedupe` | Like `--diagnostics-only`, but also deduplicate identical diagnostics across combinations. |
| `--summary-only` | Hide cargo output; show only the final summary. |
| `--fail-fast` | Stop on the first failing combination. |
| `--errors-only` | Allow all warnings, show errors only (`-A warnings`). Appends to `RUSTFLAGS`. |
| `--pedantic` | Treat warnings like errors in the summary and under `--fail-fast`. |
| `--show-pruned` | Show pruned (redundant) combinations in the summary. |
| `--prune-implied` | Automatic pruning of redundant combinations, on by default; `=false` disables it for the run. |
| `--maximal-features` | Run only maximal feature sets: combinations that are not a subset of another generated combination. An unconstrained matrix collapses into a single all-features invocation per package-target, while matrix constraints keep one invocation per alternative. |
| `--packages-only` | In `matrix` mode, emit one row per package-target instead of per combination. |
| `--only-packages-with-lib-target` | Only consider packages that have a library target. |
| `--aggregate-targets` | Batch a combination's configured targets into a single Cargo invocation (one `--target` each). Faster on many cores; falls back to serial for `run` and pruned summaries. |
| `--no-targets` | Ignore configured target lists for this run; use Cargo's default single target (`--target`, then `CARGO_BUILD_TARGET`, then host). With an explicit `--target <triple>` this also lifts package-level `targets` constraints, forcing every selected package onto that triple. |
| `--install-missing-targets` | Install missing Rust target components with `rustup` before running. Explicit opt-in — may mutate the toolchain and use the network. |
| `--omit-host-target-flag` | Build a configured target that is the host without an injected `--target`, sharing `target/debug` with an ordinary `cargo build`. On by default; `=false` builds it under `target/<triple>/` like every other configured target. See [Configured targets]({{< relref "../targets/configured-targets.md#the-host-target-runs-as-a-plain-build" >}}). |
| `--driver <bin>` | Program invoked in place of `cargo` for each build (e.g. `cargo-zigbuild`, `cross`). See [Build drivers]({{< relref "../targets/drivers.md" >}}). |
| `--env <KEY=VALUE>` | Set a variable in every matrix-cell Cargo process (repeatable; the last value for a key wins). Overrides scoped [`env` config]({{< relref "../configuration/environment.md" >}}). |
| `--unset-env <KEY>` | Remove a variable from every matrix-cell Cargo process (repeatable). Applied before CLI `--env` additions. |

Most boolean flags can also be set as [defaults in `Cargo.toml`]({{< relref "../configuration/flags.md" >}}); CLI flags always win for a single invocation.

## Environment variables

| Variable | Effect |
|---|---|
| `CARGO` | Program used for plain Cargo invocations. Set in child processes to the `+toolchain` cargo when an override is given, unless child `env` config overrides it. |
| `CARGO_DRIVER` | Set in child processes to the resolved driver unless child `env` config or CLI overrides it. |
| `RUSTUP_TOOLCHAIN` | Set in child processes to the `+toolchain` override unless child `env` config overrides it. |
| `CARGO_FC_VERBOSE` | Boolean default for verbose `cargo fc` headers. |

## Notes

- Cargo-fc boolean flags take an optional inline value: `--summary-only` means `--summary-only=true`, and `--summary-only=false` turns a `Cargo.toml` default back off for one invocation. `no`, `off`, and `0` are accepted alongside `false` (and `yes`/`on`/`1` alongside `true`). There are no `--no-<flag>` spellings — those tokens belong to cargo (`--no-fail-fast` for `test`, `--no-dedupe` for `tree`) and cargo-fc forwards them untouched.
- Switches with no configurable default — `--workspace`, `--pretty`, `--help`, `--version` — take no value.
- `--dedupe` implies `--diagnostics-only`. Setting `--dedupe` together with `diagnostics_only = false` in config is rejected as contradictory.
- Everything after `--` is forwarded to the invoked program and never interpreted by `cargo fc`.
- `--env` requires `KEY=VALUE`, split at the first `=`; `KEY=` sets an empty value.
