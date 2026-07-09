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
| `--no-prune-implied` | Disable automatic pruning of redundant combinations. |
| `--packages-only` | In `matrix` mode, emit one row per package-target instead of per combination. |
| `--only-packages-with-lib-target` | Only consider packages that have a library target. |
| `--aggregate-targets` | Batch a combination's configured targets into a single Cargo invocation (one `--target` each). Faster on many cores; falls back to serial for `run` and pruned summaries. |
| `--no-targets` | Ignore configured target lists for this run; use Cargo's default single target. |
| `--install-missing-targets` | Install missing Rust target components with `rustup` before running. Explicit opt-in — may mutate the toolchain and use the network. |
| `--driver <bin>` | Program invoked in place of `cargo` for each build (e.g. `cargo-zigbuild`, `cross`). See [Build drivers]({{< relref "../targets/drivers.md" >}}). |

Most boolean flags can also be set as [defaults in `Cargo.toml`]({{< relref "../configuration/flags.md" >}}); CLI flags always win for a single invocation.

## Environment variables

| Variable | Effect |
|---|---|
| `CARGO` | Program used for plain Cargo invocations. |
| `CARGO_DRIVER` | Set in child processes to the resolved driver. |
| `CARGO_FC_VERBOSE` | Boolean default for verbose `cargo fc` headers. |

## Notes

- Cargo-fc boolean flags do **not** accept an inline value (`--summary-only=false` is rejected). Configure false defaults in `Cargo.toml` instead.
- `--dedupe` implies `--diagnostics-only`. Setting `--dedupe` together with `diagnostics_only = false` in config is rejected as contradictory.
- Everything after `--` is forwarded to the invoked program and never interpreted by `cargo fc`.
