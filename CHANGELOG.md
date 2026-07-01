# Changelog

## [0.2.2]

### Fixed

- Fixed `--aggregate-targets` for aliases that expand through `cargo run -- ...`
  wrappers, avoiding a misleading direct-`run` fallback note when target args
  are passed to the wrapped command.
- Fixed `--diagnostics-only` and `--dedupe` for aliases that expand through
  `cargo run -- ...` wrappers so the generated `--message-format` argument is
  passed to the wrapped command instead of the wrapper package.
- Fixed run aliases such as `serve = "run --package app"` so user program
  arguments after `--` do not make cargo-fc pass generated matrix arguments to
  the program instead of Cargo.
- Fixed explicit `--target` detection for aliases that expand through
  `cargo run -- ...` wrappers.
- Fixed alias-defined wrapper `cargo run` flags such as `--target` and
  `--features` so they continue to configure the wrapper package instead of
  being treated as cargo-fc target planning or target-package feature matrix
  input.
- Fixed diagnostics suppression so only Cargo's actual `--message-format` flag
  disables cargo-fc diagnostics mode; similarly named wrapper/program flags are
  left alone.
- Fixed wrapper-package `--message-format` flags so they do not suppress
  diagnostics mode for the wrapped command.
- Fixed aliases whose wrapped command has its own `--` separator so generated
  cargo-fc args are inserted before the wrapped command's program arguments.

## [0.2.1]

### Changed

- Common cargo plugin commands such as `nextest`, `audit`, `deny`, `machete`,
  `udeps`, and `leptos` no longer print configured-target or diagnostics
  capability hints by default. They still do not receive target or diagnostics
  capability unless configured explicitly.

### Fixed

- Fixed cargo aliases that expand through `cargo run --package <wrapper>
  -- ...`, including nested aliases, so cargo-fc now passes generated
  `--package`, `--target`, and `--features` arguments to the wrapper after `--`
  instead of applying them to the wrapper package itself.

## [0.2.0]

### Breaking Changes

- Simplified the Rust library API while keeping CLI, TOML, and matrix JSON
  behavior unchanged. Implementation modules such as `cli`, `runner`,
  `diagnostics_only`, and `tee` are no longer public modules.
- Replaced nested implementation paths with root-level library exports for the
  supported core API, including config types, target planning, execution
  planning, matrix row generation, and execution.
- Removed legacy Rust API helpers and shims, including `MatrixOptions`,
  `print_feature_matrix_for_target`, `run_cargo_command_for_target`,
  `ArgumentParser`, and public summary/output parsing helpers.
- Changed `build_execution_plans` to require an explicit `PlanBuildContext` and
  `FlagConfig` instead of constructing hidden defaults or accepting CLI `Options`.
- Unknown keys in cargo-fc metadata tables now fail fast instead of being
  silently ignored. This catches misspelled config such as `fail-fast` with a
  hint to use underscore-separated keys.
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

- Added automatic `cargo-zigbuild` driver selection for planned non-host targets
  so cross-compiling crates with native C dependencies can work through the same
  `cargo fc` invocation.
- Added `--driver <bin>` and `[workspace.metadata.cargo-fc] driver = "<bin>"`
  to override the build driver, including forcing plain Cargo with
  `driver = "cargo"`.
- Added Cargo alias resolution from `.cargo/config.toml`, including nested
  aliases, so aliases such as `lint = "clippy --all-targets --no-deps"` are
  rewritten before invoking wrappers like `cargo-zigbuild`.
- Added config defaults for cargo-fc boolean flags, including `summary_only`,
  `diagnostics_only`, `dedupe`, `pedantic`, `errors_only`, `packages_only`,
  `fail_fast`, `no_prune_implied`, `show_pruned`, `aggregate_targets`,
  `no_targets`, `install_missing_targets`, and
  `only_packages_with_lib_target`.
- Added resolved flag precedence across workspace, matching workspace target,
  package, matching package target, and explicit CLI scopes, with matching
  `subcommands.<name>` tables applied immediately after each config scope.
- Added package-level and target-specific `subcommands.<name>` flag defaults,
  including target+subcommand tables such as
  `[package.metadata.cargo-fc.target.'cfg(...)'.subcommands.clippy]`.
- Added per-subcommand diagnostics flag overrides so custom commands and
  diagnostics-unsafe built-ins can opt into diagnostics mode with the same
  `diagnostics_only = true` or `dedupe = true` keys used elsewhere.
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

- Missing build drivers now produce an actionable warning explaining how to
  install `cargo-zigbuild`/zig or override the driver.
- Explicit cargo-fc CLI flags now overlay config defaults last. Broad
  config-driven diagnostics apply only to built-in diagnostics-safe commands,
  while subcommand-local diagnostics flags and explicit diagnostics CLI flags
  always win.
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
