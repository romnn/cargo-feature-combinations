# Changelog

## [0.1.0]

### Breaking Changes

- Changed `cargo fc matrix` row shape. cargo-fc now owns the top-level
  `name`, `target`, `features`, and `metadata` fields. Custom package matrix
  fields from `[package.metadata.cargo-fc].matrix` or
  `[package.metadata.cargo-fc.matrix]` now appear under `metadata` instead of
  being merged at the top level.
- Added `target` to every `cargo fc matrix` row. Matrix consumers should use
  `row.target` for the effective target triple and `row.metadata.<key>` for
  user-defined fields.
- Made `targets`, workspace `target.'cfg(...)'`, and workspace `subcommands`
  cargo-fc metadata keys active. Repositories that previously used those keys
  for unrelated local data must rename them.
- Configured target lists now apply to all target-capable Cargo commands by
  default, including `build`, `check`, `clippy`, `doc`, `test`, and `run`.
  Foreign-target `test` and `run` usually fail unless narrowed with
  `--target`, disabled with `--no-targets`, or disabled for that command in
  workspace metadata.
- Unknown Cargo aliases and custom subcommands do not receive configured
  targets unless explicitly opted in with
  `[workspace.metadata.cargo-fc.subcommands.<name>] targets = true`.
- Removed the redundant public target-detector API in favor of
  `TargetEnvironment`, `parse_cli_target`, and `host_triple`.

### Added

- Added workspace-level configured targets:
  `[workspace.metadata.cargo-fc] targets = ["<triple>", ...]`.
- Added package-level configured targets:
  `[package.metadata.cargo-fc] targets = ["<triple>", ...]`.
- Added package-level target opt-out with `targets = []`, which falls back to
  the single effective Cargo target for that package.
- Added target-specific workspace package selection via
  `[workspace.metadata.cargo-fc.target.'cfg(...)']` with `exclude_packages`
  patches.
- Added per-subcommand target capability overrides via
  `[workspace.metadata.cargo-fc.subcommands.<name>] targets = true|false`.
- Added `--no-targets` to ignore configured target lists for one invocation.
- Added explicit opt-in missing target installation with
  `--install-missing-targets` and
  `[workspace.metadata.cargo-fc] install_missing_targets = true`.
- Added `--aggregate-targets` to batch compatible configured targets for the
  same package and feature combination into one Cargo invocation.
- Added per-target execution planning so `cargo fc check`, `cargo fc clippy`,
  and other target-capable commands can cover each configured target's cfg view
  in one command.
- Added per-target attribution in command headers, summaries, diagnostics-only
  output, and dedupe reporting when target selection is explicit or configured.
- Added GitHub Actions documentation for generating target-aware matrices and
  for using one `cargo fc check` or `cargo fc clippy` invocation in CI.

### Changed

- Explicit Cargo `--target <triple>` wins globally over configured target
  lists. Without an explicit CLI target, configured package/workspace targets
  take precedence over `CARGO_BUILD_TARGET`, then fall back to
  `CARGO_BUILD_TARGET`, then the host target.
- Matrix generation now goes through the same target and execution planning path
  as command execution, so target-specific package config and workspace package
  exclusions are resolved consistently.
- Matrix JSON object keys are serialized in deterministic sorted order.
- Target-specific matrix metadata merges tables recursively; arrays and scalar
  values replace the base value.
- Summary totals and pruned-combination summaries are keyed by package, target,
  and feature set so identical feature sets across targets no longer collapse.
- Invalid or unavailable target cfg evaluation errors now surface rustc's reason
  at the cfg-evaluator boundary.
