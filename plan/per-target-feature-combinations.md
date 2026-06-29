# Per-Target Feature Combinations

## Purpose

`cargo-fc` already owns the feature-combination axis and already understands
target-specific configuration overrides through
`[package.metadata.cargo-fc.target.'cfg(...)']`. The missing piece is letting a
workspace declare the target triples that should be checked by default, so a
plain local command such as `cargo fc check` exercises the same target cfg views
that CI exercises.

This plan adds a target axis to the existing feature matrix:

```text
selected packages x effective targets for each package x feature combinations
```

The implementation should keep local and CI behavior aligned, preserve existing
single-target behavior when no target list is configured, and keep Cargo output
live by running one Cargo invocation at a time in v1.

## Recommendation

Implement this in staged milestones:

1. Target planning and precomputed execution plans.
2. Serial per-target execution and matrix output.
3. Opt-in aggregate execution (`--aggregate-targets`).
4. Documentation and compatibility follow-through.

Precomputed plans should come before execution because they separate feature
resolution from command execution and make target-specific package selection
testable.

cargo-fc never spawns concurrent Cargo processes; it stays single-threaded and
lets Cargo parallelize within each invocation. v1 ships two execution modes over
the same plans, both serial and both with live output:

- default: one Cargo invocation per `(package, target, combo)`, giving exact
  per-target attribution (PASS/FAIL, diagnostics, dedupe),
- opt-in `--aggregate-targets`: one Cargo invocation per `(package, combo)` that
  passes every target sharing that combo as repeated `--target` flags, letting
  Cargo overlap the targets' build graphs. Faster on many-core machines, but it
  attributes results at group rather than per-target granularity.

A worker pool of concurrent Cargo processes was measured and rejected; see
"Target Execution Modes" for the numbers.

## Design Principles

- Keep target selection separate from target-specific config resolution. Target
  lists decide which targets are visited; `target.'cfg(...)'` sections decide
  the effective feature matrix for one concrete target.
- Keep planning separate from execution. Resolve package configs, target
  overrides, feature combinations, and pruning before running Cargo.
- Keep cargo-fc single-threaded and its output live. Both execution modes stream
  Cargo output directly; there is no worker pool, output buffering, or replay.
- Preserve deterministic cargo-fc order. Serial mode reports target plan order,
  then package order, then feature-combination order. Aggregate mode reports
  package order, then canonical feature-combination order, then target-group
  order.
- Do not guess that arbitrary cargo aliases are link-free. Unknown aliases need
  explicit opt-in before configured target lists apply to them.

## First-Class Feature Contract

The first complete implementation should provide:

- workspace-level target lists,
- package-level target lists and package-level opt-out,
- `cargo fc matrix` rows with `target`,
- configured multi-target execution for built-in Cargo subcommands with target
  capability and for aliases explicitly allowlisted for target support in
  config,
- clear fallback behavior for aliases that lack target capability,
- a default serial per-target mode and an opt-in `--aggregate-targets` mode,
  both single-threaded with live output,
- deterministic cargo-fc invocation order in both modes, with exact per-target
  attribution in the default serial mode.

Package-level targets should not be a partial bolt-on. They should be part of
the execution plan model from the start.

## Existing Code Shape

Relevant current structure:

- `src/config.rs`
  - `Config` stores per-package cargo-fc config.
  - `Config::target_overrides` stores cfg-keyed target override sections.
  - `WorkspaceConfig` currently stores workspace-wide `exclude_packages`.
- `src/workspace.rs`
  - Reads `[workspace.metadata.cargo-fc]`.
  - Applies workspace and root-package package exclusions.
- `src/target.rs`
  - Detects one effective target from `--target`, `CARGO_BUILD_TARGET`, or host.
- `src/cfg_eval.rs`
  - Evaluates cfg expressions for a concrete target using
    `rustc --print cfg --target <triple>`.
- `src/lib.rs`
  - Detects one target and dispatches to one-target matrix/run functions.
- `src/runner.rs`
  - `print_feature_matrix_for_target(...)`
  - `run_cargo_command_for_target(...)`
  - Per-target config resolution already happens inside these functions.

The important architectural point: target-specific config resolution already
works for one target. This change should add planning/orchestration around that,
not duplicate the target override logic.

## Proposed Architecture

This should be a full-featured target axis with a small, clean architecture. Do
not weaken the behavior to keep the patch small; keep the behavior complete by
putting each concern in the right place.

Use a ports-and-adapters style boundary:

- domain structs describe target selection and execution plans,
- adapters read Cargo metadata, CLI args, environment, and rustc,
- the runner consumes plans and writes results,
- config resolution remains a pure "base config + target -> effective config"
  concern.

Keep the flow as a direct extension of the current architecture:

```text
CLI args + cargo metadata
  -> selected packages
  -> target plans
  -> per-target config resolution
  -> feature-combination execution
```

The new first-class concept is a target plan. It says "run these package-target
assignments for this target triple." The plan is deduplicated by target triple
for stable scheduling and output, while each package assignment carries where
that package's target came from. The plan should not know how to generate
feature combinations.

Suggested domain types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSource {
    Cli,
    PackageConfig,
    WorkspaceConfig,
    CargoBuildTargetEnv,
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveTarget {
    pub triple: TargetTriple,
    pub source: TargetSource,
}

pub struct PlannedPackage<'a> {
    pub package: &'a cargo_metadata::Package,
    /// Cached base cargo-fc config for this package, loaded once before
    /// planning. Carried here so execution-plan construction never re-reads
    /// the manifest, which would duplicate deprecation warnings and reparse
    /// metadata.
    pub config: &'a crate::config::Config,
    pub target: EffectiveTarget,
}

pub struct TargetPlan<'a> {
    pub target: TargetTriple,
    pub packages: Vec<PlannedPackage<'a>>,
}

pub struct TargetPlans<'a> {
    pub plans: Vec<TargetPlan<'a>>,
    pub contains_configured_assignments: bool,
}
```

`contains_configured_assignments` is `true` when target selection was influenced
by configured target metadata or an explicit `--target` (i.e. anything other
than the implicit host/`CARGO_BUILD_TARGET` single-target fallback). This
includes package `targets = []` opt-out, even if the resulting concrete target
source is `Host` or `CargoBuildTargetEnv`. Its only consumer is output
formatting: it gates whether per-entry summaries show the `target = ...` column,
so the default single-host run keeps its current output. It must not be used to
decide whether to warn about skipped configured targets; that warning uses raw
config state before planning.

Suggested module placement:

- `workspace.rs` reads workspace metadata.
  It should expose candidate package discovery separately from applying
  workspace package exclusions, because exclusions can become target-specific.
- `package.rs` continues to read package metadata.
- `target.rs` owns target detection, target flag parsing, and target-source
  types.
- `target_plan.rs` or `planner.rs` builds `TargetPlans` from selected packages,
  workspace config, package configs, CLI target info, environment, and host
  detection.
- `config::resolve` continues to resolve one package config for one concrete
  target.
- `runner.rs` executes target plans.

Suggested ownership split:

```text
src/target.rs
  TargetTriple, TargetSource, EffectiveTarget, target flag parsing,
  host/env target adapters.

src/target_plan.rs
  TargetPlans, TargetPlan, target precedence, package/workspace target list
  selection, per-package target source tracking, target-specific workspace
  package exclusion, stable dedupe.

src/runner.rs
  ExecutionPlan, PackageExecutionPlan, command execution, live output,
  summaries.

src/config.rs and src/config/resolve.rs
  serde config fields and existing target override resolution only.

src/workspace.rs
  workspace metadata parsing, workspace-root warnings, candidate package
  discovery, and target-specific workspace exclude resolution helpers.
```

If an implementation finds itself adding target-list decisions to
`config::resolve` or feature-combination generation, that is a design smell.
Target lists choose the outer execution axis; target overrides shape the config
inside one selected target.

Do not push target-list logic into `Package::feature_combinations`. Feature
generation should continue to accept an already-resolved `Config`; the target
axis is one level above that.

### Ports and Adapters

Keep side effects behind small traits so the planner is easy to test:

```rust
pub trait TargetEnvironment {
    fn cargo_build_target(&self) -> Option<String>;
    fn host_target(&self) -> eyre::Result<TargetTriple>;
}

pub struct SelectedPackage<'a> {
    pub package: &'a cargo_metadata::Package,
    pub config: &'a crate::config::Config,
}
```

The exact trait names can differ. The point is that target planning should be
unit-testable without invoking real `rustc` or spawning Cargo. The production
adapter can read `CARGO_BUILD_TARGET`, delegate host detection to `rustc -vV`,
use `RustcCfgEvaluator` for cfg matching, and share one package config loading
pass.

Planning must also receive a `CfgEvaluator`. Target-specific workspace
`exclude_packages` uses `cfg(...)` keys, so planning needs the same target cfg
data that package target overrides use. The existing evaluator trait and stub
test evaluator are the right boundary; do not make the planner shell out to
`rustc` directly.

Cache each selected package's base `Config` once before target planning and pass
those cached configs through planning and execution-plan construction.
`Package::config()` emits deprecation warnings for old keys, so calling it once
for target selection and again for feature resolution would duplicate warnings
and do unnecessary metadata parsing.

Do not hide config loading behind a planner adapter that calls
`Package::config()` on demand. The planner should consume already-loaded
`SelectedPackage`/`PlannedPackage` data.

Avoid a large abstract framework. Two or three narrow traits are enough if they
make tests deterministic and keep IO out of the planning logic.

### Planning Invariant

After target planning, every later stage should receive explicit target plans.
No later stage should need to ask "what targets should I run?" It should only
ask:

- what concrete target is this plan for?
- which package assignments belong to this target?
- should this package assignment inject `--target` into Cargo args?

This invariant is what makes the feature feel first-class instead of bolted on.

### Single-Target Compatibility

The existing public single-target functions should remain usable. Internally,
they can wrap a one-item `TargetPlans` value. In the sketch below, `packages`
means the already-loaded `(Package, Config)` pairs:

```rust
TargetPlans {
    plans: vec![TargetPlan {
        target: target.clone(),
        packages: packages.iter().map(|&(package, config)| PlannedPackage {
            package,
            config,
            target: EffectiveTarget {
                triple: target.clone(),
                source: TargetSource::Cli, // or another caller-provided source
            },
        }).collect(),
    }],
    contains_configured_assignments: false,
}
```

That lets new multi-target behavior be first-class without forcing all library
consumers to migrate immediately.

### Complexity Guardrails

- Add one target-planning abstraction, not several layers of planners.
- Keep target list selection out of target override resolution.
- Keep concurrency out of config resolution and feature generation.
- Prefer serial correctness first. Aggregate execution must be a small execution
  mode over the same plan, not a separate planning path with different
  semantics.
- Keep CLI surface small. Do not add concurrency flags in v1.

## Goals

- Allow target lists in repo config:

  ```toml
  [workspace.metadata.cargo-fc]
  targets = [
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
  ]
  ```

- Allow package-level target lists:

  ```toml
  [package.metadata.cargo-fc]
  targets = ["wasm32-unknown-unknown"]
  ```

- Compose configured targets with existing target override sections.
- Preserve single-target behavior when no configured target list exists.
- Preserve explicit `--target <triple>` as the strongest override.
- Add `target` to `cargo fc matrix` rows.
- Deduplicate target triples while preserving declaration order.
- Preserve live output by keeping cargo-fc single-threaded in both modes.
- Add an opt-in `--aggregate-targets` flag that batches a combination's targets
  into one Cargo invocation (`--target A --target B ...`) for throughput.
- Add a small command target policy so aliases can explicitly opt in to
  configured targets and cargo-fc-injected `--target` flags.

## Non-Goals

- Do not install Rust targets automatically. If a target is missing, fail with a
  clear `rustup target add <triple>` hint.
- Do not guarantee that cross-target `build`, `test`, or `run` succeeds. These
  commands may require linkers, runners, or target OS support. cargo-fc can pass
  supported flags; the toolchain prerequisites remain the user's
  responsibility.
- Do not spawn concurrent Cargo processes or add a worker pool, thread pool, or
  `--max-concurrent-targets`-style concurrency. Both execution modes are
  single-threaded; aggregate mode delegates parallelism to a single Cargo
  invocation. Feature-combination-level parallelism is also out of scope.
- Do not change the semantics of existing cfg override sections.
- Do not change existing `--diagnostics-only` or `--dedupe` command eligibility
  in v1. Preserve today's best-effort behavior for `test`, `run`, and aliases.

## Command Scope

Configured target lists should apply only when the selected cargo subcommand has
the target capability. Built-in Cargo subcommands default to this capability via
a small built-in registry. Unknown aliases default to no target capability
unless the workspace config opts them in. Workspace config may override either
default per command token.

This avoids guessing that `cargo lint` means `cargo clippy`. `lint` could be any
Cargo alias or custom command, so it must remain unknown unless the workspace
explicitly declares that its `lint` command accepts Cargo-style `--target`.

Do not extend this v1 policy to generated feature-selection flags, jobs,
message-format injection, or diagnostics parsing:

- generated feature-selection flags should preserve the current behavior:
  known cargo subcommands are normalized, and unknown aliases keep the existing
  legacy best-effort path,
- there is no target-level parallelism in v1, so cargo-fc should not compute or
  inject `--jobs`,
- `--diagnostics-only` and `--dedupe` should preserve today's behavior for
  `test`, `run`, and aliases. Do not disable them through the target capability
  policy.

Known built-in commands should have explicit built-in target allowlist entries.
The allowlist is not only for Cargo's built-in binaries; it is for command
tokens that cargo-fc knows how to reason about. For example, `clippy` must be in
the built-in allowlist because cargo-fc knows `cargo clippy` accepts Cargo's
`--target` flag. The built-in entry is a default, not a hard lock: users may set
`subcommands.<token>.targets = false` to keep a built-in command on the single
effective target while still running other commands across the configured target
list.

Encode the table below as the initial built-in registry. If implementation
tests prove that a command does not actually accept `--target`, adjust the
registry and add a regression test explaining the exception.

The initial built-in registry should track the command tokens cargo-fc already
recognizes today: `build`, `check`, `clippy`, `test`, `doc`, and `run`, plus
their existing short aliases where applicable. Do not add mutating or
non-matrix-oriented Cargo commands such as `fix` as built-ins in this plan. If
cargo-fc later starts recognizing another safe Cargo subcommand, add a built-in
target-capability entry and tests at the same time.

```text
subcommand  target capability
check       yes
clippy      yes
build       yes
doc         yes
test        yes
run         yes
unknown     no
matrix      yes
```

For `cargo fc matrix`, target capability means "use configured target planning
when generating rows." It does not imply any Cargo flag injection because no
Cargo command is spawned.

Short built-in aliases should resolve to the same target capability as their long
forms when cargo-fc already recognizes them, for example `c` -> `check`,
`b` -> `build`, `t` -> `test`, `d` -> `doc`, and `r` -> `run`.

Unknown aliases should retain the existing best-effort feature-matrix behavior
for generated feature-selection flags unless the implementation intentionally
makes a breaking change in a future release. This preserves the current cargo-fc
contract for aliases that already work. The new stricter allowlist applies only
to configured target lists and cargo-fc-injected `--target`.

Do not infer alias expansion from the alias name. In particular, do not treat
`lint` as `clippy` just because many projects use that convention. A workspace
that wants `cargo fc lint` to receive configured targets should configure:

```toml
[workspace.metadata.cargo-fc.subcommands.lint]
targets = true
```

Current code names the detected `clippy` command `CargoSubcommand::Lint`.
During implementation, either rename that enum variant to `Clippy` or keep the
variant but make the capability registry/display name clearly refer to the
literal `clippy` subcommand. The literal command token `lint` must stay
unknown unless workspace config opts it in.

`test` and `run` can accept Cargo `--target`, so they should be allowed for the
target capability. Their existing diagnostics-only behavior must not change in
v1.

`build`, `test`, and `run` may still fail for foreign targets if the user has
not installed linkers, runners, or platform support. That is acceptable: if a
repo configures targets and invokes a command with target capability, cargo-fc
should faithfully pass the target axis to Cargo and let Cargo/toolchain errors
surface clearly.

Note that the workspace `targets` list is shared across all subcommands. It is
motivated by `check`/`clippy`, but it also applies to `build`, `test`, and `run`
because they carry target capability. The failure gradient differs sharply:
cross-target `check`/`clippy`/`doc` usually succeed (they only need the target's
`rustc`), `build` needs a linker, and `test`/`run` actually execute and so
usually fail for foreign targets. This is a foot-gun: a repo that configures
`targets` for linting will, by default, make `cargo fc test` attempt every
configured triple. v1 keeps the shared list for simplicity; the escape hatch is
explicit `--target <triple>` to narrow a single run. This must be documented
prominently. Per-subcommand target lists are a possible future refinement only
if users hit this in practice.

For an unknown command or alias:

- do not inject configured target flags unless `targets = true` is configured,
- keep existing best-effort feature-matrix behavior for generated feature flags
  unless and until a separate breaking-change plan changes alias semantics,
- keep existing diagnostics-only and dedupe behavior,
- when configured targets are skipped, emit one actionable warning that explains
  the target capability was skipped and how to opt in.

Do not warn merely because a subcommand is unknown. Warn only when cargo-fc is
skipping behavior that the user requested or configured:

- configured target lists exist, but the command lacks `targets`,
- a workspace target-capability entry is malformed or refers to a built-in
  command.

Warnings must be emitted once per invocation, not once per package, target, or
feature combination. The warning should include the detected raw subcommand
token and the exact config snippet needed to opt in.

### Capability Resolution

Resolve command target capability once, immediately after parsing cargo-fc flags
and before target planning:

1. Detect the raw cargo subcommand token using the same cargo-flag skipping
   rules currently used by `cargo_subcommand`. This needs a token-extracting
   helper because the current enum collapses all unknown commands to `Other`.
2. Look up `[workspace.metadata.cargo-fc.subcommands.<token>]`.
3. For built-in short aliases, if there is no exact alias entry, also look up
   the long built-in command key.
4. If no configured entry exists and the token maps to a known built-in cargo
   subcommand, use the built-in target-capability registry.
5. If no configured entry exists for an unknown command, deny configured
   targets for that command.

The `cargo fc matrix` command is not a forwarded cargo subcommand: it has target
capability unconditionally and skips token detection entirely. The resolution
steps above apply only to forwarded cargo commands.

Represent the result as a value, not scattered checks:

```rust
pub struct ResolvedCommandTargetPolicy {
    pub command_name: String,
    pub source: CommandCapabilitySource,
    pub targets: CapabilityDecision,
}

pub enum CommandCapabilitySource {
    BuiltIn,
    WorkspaceConfig,
    Unknown,
}

pub enum CapabilityDecision {
    Allowed,
    Denied,
}
```

The exact type names can differ. The important design rule is that the runner
consumes one resolved policy object instead of repeatedly asking whether a
subcommand is known.

Built-in commands should not require workspace config. Workspace config is still
allowed to override their default target policy, because "should cargo-fc apply
the configured target axis for this command?" is a repo policy decision. This
supports workflows such as linting every configured target while keeping
`cargo fc build` on the single effective target.

The target warning must be driven from raw config state, not from the planned
targets after capability filtering. Before planning, compute whether any
workspace or selected package declares a non-empty target list. If configured
targets exist and the selected command lacks target capability, emit the warning
and build the normal single-target plan.

Future work may introduce a broader capability policy for diagnostics or other
cargo-fc-injected flags. If it does, it must be treated as a compatibility
change: `test`, `run`, and aliases currently work on a best-effort basis with
`--diagnostics-only`/`--dedupe`, and that behavior should not be removed without
an explicit migration path.

## Configuration Schema

All new metadata keys should work under the existing metadata aliases:

- `[workspace.metadata.cargo-fc]`
- `[workspace.metadata.fc]`
- `[workspace.metadata.cargo-feature-combinations]`
- `[workspace.metadata.feature-combinations]`
- matching `[package.metadata.*]` sections

Follow the existing alias precedence instead of introducing target-specific
lookup rules.

### Workspace Config

Add fields to `WorkspaceConfig`:

```rust
pub struct WorkspaceConfig {
    pub exclude_packages: HashSet<String>,
    #[serde(default, rename = "targets")]
    pub workspace_targets: Vec<String>,
    #[serde(default, rename = "target")]
    pub target_overrides: BTreeMap<String, WorkspaceTargetOverride>,
    #[serde(default, rename = "subcommands")]
    pub subcommand_overrides: BTreeMap<String, CommandTargetCapability>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WorkspaceTargetOverride {
    pub exclude_packages: Option<StringSetPatch>,
}
```

Suggested defaults:

- `targets = []`
- `target = {}`
- `subcommands = {}`

An empty workspace target list means "no configured target list"; behavior falls
back to the existing effective target detection path.

Workspace-only keys should be read only from the workspace root, matching the
current `exclude_packages` behavior. This includes `targets`, workspace
`target.'cfg(...)'` overrides, and `subcommands`. If a non-root package appears
to contain one of these
`[workspace.metadata.cargo-fc]` keys, warn that workspace metadata is only read
from the workspace root.

### Target-Specific Workspace Package Selection

Allow workspace package exclusions to vary by target:

```toml
[workspace.metadata.cargo-fc]
targets = [
  "x86_64-unknown-linux-gnu",
  "wasm32-unknown-unknown",
]

[workspace.metadata.cargo-fc.target.'cfg(target_arch = "wasm32")']
exclude_packages = { add = ["native-cli"] }

[workspace.metadata.cargo-fc.target.'cfg(target_os = "linux")']
exclude_packages = { add = ["wasm-app"] }
```

This is intentionally narrow. Workspace target overrides may patch
`exclude_packages` only. Do not allow these sections to change `targets`,
`subcommands`, or command target capability. Target lists choose the outer axis;
workspace target overrides only decide which workspace packages participate for
one already-selected target.

Use the same patch semantics as package target overrides for set-like fields:

- array syntax is an override,
- `{ override = [...] }` replaces the base set,
- `{ add = [...] }` unions with the base set,
- `{ remove = [...] }` subtracts from the base set,
- matching cfg sections merge deterministically by cfg key order,
- conflicting overrides are errors.

Use the same cfg evaluator and validation rules as package target overrides.
In particular, `cfg(feature = "...")` must remain unsupported in workspace
target override keys; these sections select by target cfg only.

The effective excluded package set for one target is:

```text
workspace.exclude_packages
  + deprecated root-package exclude_packages
  patched by matching [workspace.metadata.cargo-fc.target.'cfg(...)']
```

Then that effective set is applied to the target's package list.

Workspace target overrides apply to every concrete effective target, including
single-target invocations selected by explicit `--target`, `CARGO_BUILD_TARGET`,
or host fallback. They are not limited to configured multi-target runs.

Implementation impact: split workspace package discovery from workspace package
exclusion. `packages_for_fc()` currently filters `exclude_packages` globally,
but target-specific exclusions require target planning to start from candidate
workspace packages and apply the effective exclude set per target. Preserve the
existing deprecation warnings for root-package `exclude_packages`; just fold
those values into the base exclude set before target-specific patches.

### Command Target Capability Config

Add a workspace-level target-capability table for cargo subcommands:

```toml
[workspace.metadata.cargo-fc.subcommands.lint]
targets = true
```

This means: when the detected cargo subcommand is `lint`, cargo-fc is allowed to
expand configured target lists and inject `--target <triple>`.

The same table may override built-in defaults:

```toml
[workspace.metadata.cargo-fc.subcommands.build]
targets = false
```

This means: `cargo fc build` uses the single effective target unless the user
passes an explicit Cargo `--target`, while other commands can still use the
workspace/package target lists.

This table must not change generated feature-selection behavior or diagnostics
behavior in v1.

Suggested Rust shape:

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CommandTargetCapability {
    #[serde(default)]
    pub targets: bool,
}
```

A plain `bool` (default `false`) is enough in v1. Unknown aliases default to
deny, while built-ins default according to the registry. A present
`targets = false` entry is an explicit opt-out and should suppress the unknown
command "how to opt in" warning for that token.

For unknown aliases the default is therefore `targets = false` (denied).
Built-in commands get their default from code, and this table may override it.

Keep this table in workspace metadata only. Command aliases are an invocation
property, not a package property. Per-package command target capability would
make a single `cargo fc lint` invocation ambiguous when selected packages
disagree.

Built-in target capability should be provided by code. Do not require users to
configure standard Cargo subcommands such as `check`, `clippy`, or `build`, but
do allow workspace config to override those defaults intentionally.

The `targets` capability name describes cargo-fc behavior: cargo-fc may expand
configured target lists and add `--target <triple>`.

When configured targets exist and the selected command lacks target capability
by default, cargo-fc should skip the target expansion and warn. For example:

```text
warning: not passing --target to cargo alias `lint` because it has no configured targets capability
hint: add [workspace.metadata.cargo-fc.subcommands.lint] targets = true if this alias accepts --target
```

When the command is explicitly configured with `targets = false`, cargo-fc
should skip target expansion without warning.

### Package Config

Add package-level targets to `Config`:

```rust
pub struct Config {
    #[serde(default, rename = "targets")]
    pub package_targets: Option<Vec<String>>,
    // existing fields...
}
```

Use `Option<Vec<String>>`, not `Vec<String>`, so the implementation can
distinguish these cases:

- missing `targets`: inherit workspace target list
- `targets = []`: explicit package-level opt-out of workspace targets, using
  the fallback single effective target instead
- `targets = ["..."]`: package-specific target list

This matters for mixed workspaces where most packages should run on the
workspace target list, but one package is host-only or wasm-only.

`targets` is a selection field, not a feature-matrix field. Do not add it to
`TargetOverride`, and do not allow `[package.metadata.cargo-fc.target.'cfg(...)']`
to change the target list. Target override sections are evaluated after a
concrete target has already been selected.

If `Config` is cloned by `config::resolve`, either preserve `targets`
harmlessly or clear it from the resolved config. The critical rule is that
`Package::feature_combinations` must not read `targets`.

### CLI Flags

Add exactly one new cargo-fc flag in v1: `--aggregate-targets`. It is a boolean
flag, drained before forwarding args to Cargo (like `--summary-only` and the
other cargo-fc flags), and selects aggregate execution mode (one Cargo
invocation per `(package, combo)` carrying all that combo's targets). The default
(flag absent) is serial per-target. There are no concurrency flags.

The existing forwarded Cargo `--target` flag becomes more important because it
overrides configured target lists. Parse it only from forwarded Cargo args
before `--`; arguments after `--` belong to tests or binaries and must not
affect cargo-fc target planning. This fixes the latent case:

```bash
cargo fc run -- --target value-for-the-binary
```

The value above must not be treated as Cargo's target triple.

## Target Precedence

Target planning must be capability-aware. If the selected command lacks the
`targets` capability, configured workspace/package target lists are ignored for
that invocation and cargo-fc falls back to the current single effective target
path: explicit CLI `--target`, then `CARGO_BUILD_TARGET`, then host.

When the selected command has the `targets` capability, target planning should
use this precedence for each selected package:

1. Explicit Cargo CLI `--target <triple>` or `--target=<triple>`.
2. Package-level `targets`, when present for that package.
3. Workspace-level `targets`, when non-empty.
4. `CARGO_BUILD_TARGET`.
5. Host target from `rustc -vV`.

This is intentionally different from the current single-target detector once a
repo has configured targets. Repository config should not be silently collapsed
by a developer's ambient `CARGO_BUILD_TARGET`. This is also intentionally
different from Cargo's own precedence for `[build].target`: for cargo-fc,
configured workspace/package target lists are the declarative matrix. If a user
wants one target for a run, explicit `--target <triple>` is the override. This
must be documented because users familiar with Cargo may expect
`CARGO_BUILD_TARGET` to beat file config.

When explicit `--target` is present, it wins globally and all package/workspace
configured target lists are ignored for that invocation.

Every package-target assignment must carry its `TargetSource`. This keeps later
decisions simple:

- `Cli`: Cargo already received the target flag from the user.
- `PackageConfig` or `WorkspaceConfig`: cargo-fc must inject `--target`.
- `CargoBuildTargetEnv`: Cargo will see the env var; cargo-fc does not need to
  inject a target.
- `Host`: keep current no-`--target` behavior.

A target plan itself is deduplicated by triple, not by source. This matters when
the same target triple is reached through different precedence levels for
different packages, for example when one package inherits a workspace target and
another package uses `targets = []` and falls back to the host target. The
runner must decide target injection per package execution from that package's
`EffectiveTarget::source`, not from a plan-wide source.

This source field avoids boolean drift like `is_configured`, `should_inject`,
and `is_multi_target` scattered through the runner.

Prefer methods on the enum for repeated policy checks:

```rust
impl TargetSource {
    fn should_inject_target_arg(self) -> bool {
        matches!(self, Self::PackageConfig | Self::WorkspaceConfig)
    }

    fn is_configured(self) -> bool {
        matches!(self, Self::PackageConfig | Self::WorkspaceConfig)
    }
}
```

This keeps policy near the type that owns the meaning.

## Effective Target Planning

Introduce a planning layer before calling the runner.

Top-level flow should be:

1. Discover candidate workspace packages without applying workspace
   `exclude_packages`.
2. Apply CLI package selection filters.
3. Load and cache each selected package's base cargo-fc config once.
4. Resolve target plans for those candidate packages.
5. Apply the effective workspace `exclude_packages` set per target plan.

This is a larger refactor than simply adding a target loop. Today package
discovery/exclusion happens before target detection, but target-specific
workspace exclusions apply even for explicit single-target invocations.

Build `TargetPlans` from the candidate packages in two passes:

1. Resolve the effective target list for each selected package using cached
   package config.
2. Deduplicate triples within each package list while preserving order.
3. Build the global target order:
   - explicit CLI target: the single CLI target,
   - workspace targets: workspace target order first,
   - package-only targets: first-seen order by selected package order and then
     by that package's target list,
   - fallback target: the single `CARGO_BUILD_TARGET` or host target.
4. For each target in the global order, attach selected packages whose effective
   target list contains that target, preserving each package's `TargetSource`
   for that target.
5. Resolve the effective workspace `exclude_packages` set for that target and
   remove excluded package assignments from that target plan.
6. Drop empty target plans.

This supports package-level target lists without requiring the runner to process
all packages for all targets. Deduplicate target triples for scheduling and
output, but do not discard per-package target source information.

Example:

```toml
[workspace.metadata.cargo-fc]
targets = ["linux", "windows"]

[package.metadata.cargo-fc]
targets = ["wasm32-unknown-unknown"]
```

If the package above is `web`, and another package inherits the workspace list,
the target plans become:

```text
linux:   inherited packages
windows: inherited packages
wasm32:  web
```

Use real triples in tests; the names above are illustrative only.

Target-specific workspace `exclude_packages` is applied after package-level
target selection. That means a package can declare that it supports a target,
while the workspace can still exclude it for that target in a central policy.

### Package-Level Target Semantics

Package-level `targets` should override, not merge with, workspace-level
`targets`. This is simpler and less surprising:

- no package `targets`: inherit workspace targets,
- package `targets = []`: opt out of workspace targets and use the fallback
  single effective target,
- package `targets = ["..."]`: run only those package targets.

Do not add package/workspace target merging in the first implementation. If a
package needs workspace targets plus one extra target, it should list the full
package target set explicitly. That keeps target planning inspectable from the
manifest alone.

### Target List Validation

Keep validation intentionally small:

- trim strings,
- reject empty target triples,
- deduplicate while preserving order.

Do not try to validate triples against rustc target lists during config parsing.
The authoritative check is the existing `rustc --print cfg --target <triple>`
path and the eventual Cargo invocation. If a target is not installed, surface a
clear hint.

### Target Availability Errors

Do not add a separate "validate all installed targets" preflight in the first
implementation. It would duplicate rustc/Cargo behavior and add another slow
toolchain call.

Instead, classify failures at the existing adapter boundaries:

- when `RustcCfgEvaluator` runs `rustc --print cfg --target <triple>`,
- when a Cargo command fails before emitting useful diagnostics because a target
  is missing.

Wrap recognized missing-target failures with:

```text
target `<triple>` is not installed
hint: run `rustup target add <triple>`
```

Keep the original error as context. Do not hide non-target toolchain failures
behind the rustup hint.

## Injecting Cargo Targets

Today, when `--target` is provided by the user, Cargo sees it because the
original arg is forwarded. For configured targets, the runner must add
`--target <triple>` to each spawned Cargo command that corresponds to a
configured package-target assignment.

Rules:

- If the package assignment came from user-provided CLI `--target`, do not
  inject another target.
- If the package assignment came from package/workspace config and the command
  has the `targets` capability, inject `--target <triple>`.
- If a package assignment came from package/workspace config but the command
  lacks the `targets` capability, target planning should not have produced a
  configured multi-target assignment. Treat that as a planner bug.
- If the package assignment came from `CARGO_BUILD_TARGET`, do not inject; Cargo
  will see the environment variable.
- If the package assignment came from host detection, keep current
  no-`--target` behavior.

Make the injection decision per package execution, not per target plan. A
single target plan can contain package assignments with different sources when
the same triple appears through multiple precedence paths. Configured package
assignments are only valid for commands with the `targets` capability.

## Matrix Output

Change `cargo fc matrix` so every row includes `target` and nests user-defined
matrix fields under `metadata`:

```json
{
  "features": "serde,cli",
  "metadata": {
    "kind": "ci"
  },
  "name": "my-crate",
  "target": "x86_64-pc-windows-msvc"
}
```

cargo-fc owns top-level row fields: `name`, `target`, `features`, and
`metadata`. Store `config.matrix` unchanged under `metadata`, so user keys such
as `target`, `features`, or `name` can not collide with cargo-fc-owned fields.
This is a schema change for existing `cargo fc matrix` consumers and must be
called out in the release notes and final README docs.

Target-specific `matrix` metadata merge semantics: JSON/TOML tables merge
recursively; arrays and scalar values replace the base value.

For `--packages-only`, emit one row per package-target pair with
`features = "default"` as today.

## Summary Output

Serial per-target execution gives exact per-target attribution: every diagnostic,
PASS/FAIL, and dedupe note belongs to exactly one Cargo invocation, so cargo-fc
always knows which target produced it. Surface that attribution in two places.

First, the per-combination command/progress header (printed before each Cargo
invocation, ahead of its streamed diagnostics) must include the target, so that
diagnostics streamed under it tell the user which target they belong to:

```text
    Checking my-crate ( target = x86_64-pc-windows-msvc, features = [serde] )
```

Second, add target to each executed summary entry:

```text
PASS my-crate (target = x86_64-pc-windows-msvc, 0 errors, 0 warnings, features = [serde])
```

Show the `target = ...` field (in both the header and the summary) only when the
run is not the implicit single-host default — that is, when targets are
configured, explicitly selected, or more than one target is planned
(`TargetPlans::contains_configured_assignments`). This keeps the default
single-host run's output unchanged.

In aggregate mode (`--aggregate-targets`) one invocation covers several targets,
so a summary entry is keyed by `(package, combo, target-group)` and shows
`targets = [t1, t2, ...]` instead of a single `target`. Its PASS/FAIL and counts
are the combined result for that invocation; per-target attribution is not
available for that multi-target group. Singleton groups should keep the serial
shape (`target = ...`) because exact attribution is still available and there is
no throughput benefit from making the output less precise.

Also include target in pruned summaries. A feature combination pruned for one
target is not necessarily pruned for another target because target overrides can
change the effective config. To keep v1 simple and correct,
`--aggregate-targets` must fall back to serial per-target execution whenever
pruned summaries are requested, either by CLI `--show-pruned` or by any resolved
package config. Pruned-summary display is inherently per `(package, target)`;
do not invent grouped pruned summaries in v1.

The global finished line should mention targets when more than one target was
planned:

```text
Finished 42 feature combinations for 3 packages across 2 targets in 18.20s
```

Implementation details that must change:

- `Summary` needs target display context, not just `Option<TargetTriple>`.
  Suggested shape:

  ```rust
  enum SummaryTarget {
      Hidden,
      Single(TargetTriple),
      Group(Vec<TargetTriple>),
  }
  ```

  `Hidden` preserves implicit single-host output, `Single` prints
  `target = ...`, and `Group` prints `targets = [...]`.
- The per-combination header (`print_package_cmd`) must take the target and
  print the same target display context when applicable, so streamed diagnostics
  in `--diagnostics-only` mode are attributable to a target or aggregate target
  group.
- Serial summary counting must key executed combinations by `(package, target,
  features)`, not `(package, features)`, or identical feature sets across
  targets will collapse.
- Aggregate summary counting must key executed combinations by `(package,
  canonical-combo, target-group)`. Counts are group-summed from the single Cargo
  invocation and are not directly comparable to serial per-target counts.
- `append_pruned_summaries` must match pruned entries against executed summaries
  scoped to the same `(package, target)` in serial mode. If pruned summaries are
  enabled, aggregate mode is not used.
- Serial summary sorting must preserve target plan order, then package order,
  then feature order. Aggregate summary sorting must preserve package order,
  canonical combo order, then target-group order. Do not globally sort a
  package's summaries by features if that scrambles either execution mode's
  output order.
- Exit-code aggregation across target plans should be deterministic: for
  non-fail-fast serial execution, keep running all planned combinations but
  return the first failing exit code in execution-plan order.

## Dedupe Semantics

In serial per-target mode, `--dedupe` should deduplicate diagnostics across the
full executed product:

```text
package x target x feature combination
```

Current global `seen_diagnostics` behavior can be extended across target plans
in execution-plan order. This means an identical rendered diagnostic that
appears for multiple targets is printed once, attributed to the first planned
target whose combination emits it, and later occurrences are counted as
suppressed duplicates.

That global behavior is intentional for v1 because it preserves the meaning of
`--dedupe`: reduce repeated diagnostics across the executed product. It has a
tradeoff: a diagnostic common to several targets is not rendered once per
target. Make the target visible in command/progress output and summaries so the
first attribution is inspectable, and document the tradeoff.

In aggregate mode, counts are for the whole target group, not per target:

- Without `--dedupe`, a warning common to three targets may be rendered three
  times by Cargo inside the one aggregate invocation and counted three times in
  that group summary. It will not be labeled per target.
- With `--dedupe`, the existing per-invocation dedup (`seen_this_run`) folds
  those repeated rendered diagnostics to one shown diagnostic for the
  `(package, combo, group)` invocation. Because they are folded inside one
  invocation, the suppressed count can differ from serial mode, where later
  targets are suppressed by the global `seen` set.

The pass/fail verdict is expected to match serial for clean/failing
combinations, but per-entry warning/error/suppressed counts are not guaranteed
to match serial. Aggregate diagnostics are usually most useful with `--dedupe`;
document that recommendation.

Aggregate mode is deterministic at the cargo-fc invocation level: package order,
combo order, target-group order, and summary ordering are controlled by
cargo-fc. Within one aggregate Cargo invocation, Cargo owns rustc scheduling and
message ordering. Do not promise per-target diagnostic ordering or attribution
inside an aggregate invocation; that is the explicit tradeoff for throughput.

## Output

Both modes keep the current live-output model: cargo-fc runs one Cargo invocation
at a time and streams its stdout/stderr (and diagnostics-only output) directly to
the terminal. There is no output buffering, sink abstraction, or replay — those
only existed to serve concurrent workers, which v1 does not have.

Progress uses the existing scheme: compute the total invocation count up front
from the execution plan, then increment a counter as invocations run in
deterministic order (target-plan order, then package, then combo for serial;
package, then combo, then target-group for aggregate). No preassigned
per-combination indices are needed without parallel replay.

## Target Execution Modes

cargo-fc never spawns concurrent Cargo processes. It runs one Cargo invocation at
a time and relies on Cargo's own scheduler to use the cores. v1 has two
single-threaded modes over the same execution plans:

- **serial per-target (default).** One invocation per `(package, target, combo)`,
  passing a single `--target` when the source requires injection. Gives exact
  per-target attribution: every PASS/FAIL, diagnostic, and dedupe note belongs to
  one target.
- **aggregate (`--aggregate-targets`).** One invocation per `(package, combo)`
  that passes every target sharing that combo as repeated `--target` flags. Cargo
  builds them together, overlapping their job graphs. Faster on many-core
  machines, but attribution is per group, not per target for groups with more
  than one target.

Both stream live output and use deterministic cargo-fc invocation order. Neither
uses threads. Aggregate mode still inherits Cargo's internal diagnostic ordering
inside each multi-target invocation.

### Why no worker pool

Running concurrent Cargo processes was measured and rejected. With `cargo 1.96`,
on this repo (`cargo clippy`, two targets, full rebuilds via `cargo clean`) and a
dependency-heavy sample crate (`regex`+`serde_json`+`url`); 2-core figures use
`taskset -c 0,1`:

| approach | 48 cores, 2 targets | 2 cores, 2 targets | attribution | extra cost |
| --- | --- | --- | --- | --- |
| serial per-target (default) | 6.1 s | 11.0 s | exact | none |
| concurrent processes, shared `target/` | 6.0 s (no gain) | ~serial | exact | none |
| concurrent processes, per-target `CARGO_TARGET_DIR` | 4.5 s | 14.2 s (slower) | exact | 2x deps + 2x disk |
| aggregate: one invocation, `--target A --target B` | 4.1 s | 10.8 s | group-level | none |

- Concurrent processes sharing the default `target/` do not parallelize: Cargo
  locks per `target/<triple-or-host>/<profile>/.cargo-lock` and also takes the
  host `target/<profile>/.cargo-lock` for build scripts/proc-macros/host deps, so
  two `--target` builds (or two `--features` builds) serialize on the shared host
  lock. (Debug vs release escape only because they use different profile dirs;
  that does not generalize to triples.)
- Per-`CARGO_TARGET_DIR` workers do parallelize but recompile shared host
  artifacts and deps once per target: a modest win on many cores, but 28% slower
  than serial on 2 cores, plus 2x disk and the whole buffered-capture /
  ordered-replay / job-budgeting apparatus. Net loss for the common small CI
  runner.
- A single multi-target invocation (aggregate mode) is fastest and cheapest — one
  process, one lock, host artifacts shared once, Cargo fills the tail across
  targets (3 targets: 4.1 s vs 7.4 s serial on 48 cores; ~tied on 2 cores, never
  slower). Its only cost is attribution (below).

### Aggregate mode details

- **Grouping.** For each selected package, group its executed combinations by a
  canonical feature-combination key, preferably the sorted `Vec<String>` already
  used by the matrix planner rather than a display string. Targets whose
  resolved matrix contains a given combo share one invocation for it.
  Target-specific `cfg(...)` overrides that change the feature set simply
  produce different combo keys and fall into different invocations automatically
  — no explicit "feature-matrix grouping" logic is needed beyond keying by the
  canonical combo.
- **Injection.** Aggregate mode applies only to configured multi-target runs, so
  every batched target has source `PackageConfig`/`WorkspaceConfig` and gets a
  `--target <triple>`. Explicit-`--target`, env, and host fallbacks are
  single-target and run as serial.
- **Attribution.** Cargo's JSON diagnostics carry no triple (a `compiler-message`
  is `{reason, package_id, manifest_path, target, message}`, where `target` is the
  lib/bin, not the triple) and one invocation returns one exit code. So results
  for multi-target groups are reported per `(package, combo, group)` with
  `targets = [...]`. Singleton groups should use the normal `target = ...`
  output. Group-level attribution is the documented tradeoff for speed; the
  default serial mode is the choice for exact per-target attribution everywhere.
- **Command applicability.** Aggregate requires a command that accepts repeated
  `--target`. `run` does not (Cargo rejects multiple `--target` for `run`), so
  `--aggregate-targets` falls back to serial per-target for `run` with a one-line
  note. The observed Cargo error is `error: only one --target argument is
  supported`. `test` accepts it (builds all, runs host; foreign-target test
  binaries fail to execute, exactly as in serial). `matrix` is unaffected: it
  always emits per-target rows regardless of the flag. Unknown aliases opted
  into target capability are assumed to accept repeated `--target`; if they do
  not, Cargo's own error surfaces. V1 intentionally has one target capability
  bit, not a separate "accepts repeated --target" capability; the built-in `run`
  exception is hardcoded from Cargo's behavior.
- **Fallbacks and no-ops.** Aggregate mode falls back to serial when `run` is the
  selected subcommand, when pruned summaries are enabled, or when the effective
  run has only one target. For `cargo fc matrix`, `--aggregate-targets` has no
  effect because matrix output is always per target. When the user explicitly
  passed `--aggregate-targets` and it has no effect or falls back, emit one short
  note explaining why.
- **Determinism.** Iterate packages in plan order, combos in sorted order, and
  targets within an invocation in target-plan order. `--fail-fast` stops at the
  first failing invocation. `--dedupe` uses the same global `seen` set across
  invocations.

### Architecture impact

These decisions remove every concurrency abstraction the earlier drafts carried
under the worker-pool assumption: no thread/worker pool, no `OutputSink`, no
buffered capture or ordered replay, no per-worker job budgeting, no
`max_concurrent_targets`/`--max-concurrent-targets`, and no cross-worker
exit-code coordination. Preassigned progress indices also go away — the existing
up-front-total-plus-runtime-counter scheme is enough. What remains is one extra
boolean flag, one `TargetExecutionMode` enum, a `SummaryTarget` generalization,
and a per-package canonical combo-to-targets transpose in the executor;
planning, config resolution, and feature generation are identical for both
modes. The summary/output generalization is the main real cost of aggregate
mode; do not hide that complexity in the executor.

## Runner API Changes

Keep the existing single-target functions for compatibility:

```rust
pub fn print_feature_matrix_for_target(...)
pub fn run_cargo_command_for_target(...)
```

Draw the new public boundary at `ExecutionPlan`, not `TargetPlan`. Planning
(target selection, per-target config resolution, feature generation/pruning)
needs the cached package configs and the `CfgEvaluator`; execution does not.
Building the execution plans in the caller keeps config loading in one place (no
duplicated deprecation warnings, no re-parsing) and lets the executor be pure:

```rust
// Planning: needs cached configs (via PlannedPackage::config) + evaluator.
pub fn build_execution_plans(
    target_plans: &[TargetPlan<'_>],
    options: &Options,
    evaluator: &mut impl CfgEvaluator,
) -> eyre::Result<Vec<ExecutionPlan<'_>>>

// Execution: consumes prebuilt plans; needs neither config nor evaluator.
// `mode` selects serial per-target vs aggregate; both are single-threaded.
pub fn run_execution_plans(
    plans: &[ExecutionPlan<'_>],
    cargo_args: Vec<&str>,
    options: &Options,
    mode: TargetExecutionMode,
) -> eyre::Result<ExitCode>

pub fn print_matrix_for_execution_plans(
    plans: &[ExecutionPlan<'_>],
    options: &MatrixOptions,
) -> eyre::Result<ExitCode>
```

The exact names can differ, but keep three properties: target planning is
separate from feature-combination planning; base configs are loaded once and
flow through `PlannedPackage::config`; and the executor takes already-resolved
plans, so it needs no `CfgEvaluator` and re-reads no manifests. This is also why
the earlier API sketch of `run_cargo_command_for_targets(plans: &[TargetPlan],
…, evaluator)` was dropped: passing `TargetPlan`s into the runner would force it
to re-resolve configs and re-trigger deprecation warnings.

## Execution Plan Model

To keep execution simple and deterministic, introduce a second plan after target
planning:

```rust
pub struct ExecutionPlan<'a> {
    pub target: TargetTriple,
    pub package_plans: Vec<PackageExecutionPlan<'a>>,
    pub show_pruned: bool,
}

pub struct PackageExecutionPlan<'a> {
    pub package: &'a cargo_metadata::Package,
    pub target: EffectiveTarget,
    pub combinations: Vec<Vec<String>>,
    pub pruned: Vec<crate::implication::PrunedCombination>,
}
```

The exact container shape can differ. A flat ordered list of resolved
`PackageTargetExecutionPlan` units is also acceptable if it makes the executor
simpler. The invariant matters more than the struct names: after
`build_execution_plans`, execution owns a deterministic sequence of resolved
`(package, target, combinations, pruned)` units, with no need to load configs or
evaluate cfg again.

Build `ExecutionPlan`s serially before running Cargo (this is
`build_execution_plans` from "Runner API Changes"):

1. For each `TargetPlan`, resolve each package assignment's target-specific
   config from the cached `PlannedPackage::config` (never re-read the manifest).
2. Generate and prune feature combinations.
3. Store owned feature strings so execution does not borrow from temporary
   configs.
4. Compute the total invocation count up front (serial: total combos across all
   `(package, target)`; aggregate: number of distinct `(package, combo)`
   invocations). The progress counter increments at runtime in iteration order.

This mirrors the existing `plan_feature_combinations` function but lifts it from
"one target" to "all planned targets". The runner should then execute an
already-built plan.

Both execution modes consume these same plans. Serial iterates them directly
(`(package, target)` then combo). Aggregate transposes them per package into
`canonical combo -> targets` and emits one invocation per `(package, combo)`; no
separate plan type is required.

Why this matters:

- target override resolution stays deterministic and easy to test,
- the executor consumes owned plans and needs neither `CfgEvaluator` nor package
  configs, so it does no metadata parsing and is trivial to test,
- fail-fast and progress counts can be defined before execution starts,
- both the serial and aggregate executors consume the same plan, differing only
  in how many `--target` flags each invocation carries.

## Execution Strategy

Both modes execute the same `ExecutionPlan` list on a single thread:

```rust
pub enum TargetExecutionMode {
    /// Default: one invocation per (package, target, combo).
    SerialPerTarget,
    /// `--aggregate-targets`: one invocation per (package, combo), all targets.
    Aggregate,
}
```

Both:

- stream live Cargo output (no buffering or replay),
- use one shared diagnostic dedupe set,
- honor `--fail-fast` by stopping at the first failing invocation,
- report cargo-fc-controlled invocation and summary order deterministically.

In aggregate mode, Cargo controls diagnostic ordering inside the one
multi-target invocation. cargo-fc must not claim per-target diagnostic
attribution or ordering there.

The mode only changes how targets map onto invocations and how summary entries
are keyed; planning, config resolution, and feature generation are identical.

## Implementation Milestones

### Milestone 1: Config and Target Planning

- Add `WorkspaceConfig::workspace_targets` (`targets` in TOML).
- Add `WorkspaceConfig::target_overrides` (`target` in TOML) with
  `WorkspaceTargetOverride`.
- Add `Config::package_targets: Option<Vec<String>>` (`targets` in TOML).
- Add workspace `subcommands.<token>.targets` for alias target opt-in.
- Add target-source domain types.
- Add raw cargo subcommand token extraction for capability lookup.
- Add target planning module with narrow test adapters.
- Split workspace package discovery from workspace package exclusion so
  exclusions can be applied per target.
- Cache selected package configs once and reuse them for planning and execution
  planning.
- Evaluate workspace target override cfg keys through `CfgEvaluator`.
- Parse forwarded Cargo `--target` only before `--`.
- Detect raw configured target lists before capability filtering so skipped
  target warnings have a reliable data source.
- Add tests for serde behavior:
  - absent package targets inherit workspace targets,
  - empty package targets opt out to fallback single target,
  - package targets override workspace targets,
  - duplicate targets are removed while preserving first occurrence,
  - workspace target overrides patch `exclude_packages`.
- Add tests for target precedence, target plan ordering, and source tracking.
- Add a regression test where the same target triple is reached through
  different sources for different packages, and verify injection decisions are
  preserved per package assignment.
- Add tests for non-root workspace-only metadata warnings for `targets`,
  workspace target overrides, and `subcommands`.

### Milestone 2: Multi-Target Matrix

- Add `print_matrix_for_execution_plans`.
- Build `ExecutionPlan`s for matrix output instead of recomputing target
  selection inside the matrix function.
- Include `target` in every matrix row.
- Include user matrix metadata under each row's `metadata` object.
- Ensure target-specific overrides are resolved per package-target pair.
- Add integration tests for:
  - workspace targets,
  - package-level targets,
  - target-specific workspace `exclude_packages`,
  - `--target` overriding configured lists,
  - target override sections changing feature rows for matching targets.

### Milestone 3: Serial Multi-Target Execution

- Add the execution-plan executor (`run_execution_plans`) for serial per-target
  mode.
- Build `ExecutionPlan`s before execution.
- Inject `--target <triple>` for configured targets when the command has the
  `targets` capability.
- Add `SummaryTarget`-style target display context to `Summary`.
- Add target to command/progress display.
- Add target-aware summary output.
- Count summaries by `(package, target, features)`.
- Scope pruned summary attachment by `(package, target)`.
- Compute invocation totals up front; increment the progress counter at runtime
  in plan order.
- Extend `--dedupe` across serial target plans.
- Keep `--fail-fast` serial and deterministic.
- Preserve existing `--diagnostics-only` and `--dedupe` eligibility for
  `test`, `run`, and aliases.
- Aggregate exit codes deterministically by returning the first failure in
  execution-plan order after non-fail-fast execution completes.

This milestone delivers the core serial feature.

### Milestone 4: Aggregate Execution Mode

- Add the `--aggregate-targets` flag (boolean, drained before forwarding).
- Add `TargetExecutionMode` and route `run_execution_plans` by mode.
- In aggregate mode, group each package's executed combinations by canonical
  combo key and emit one Cargo invocation per `(package, combo)` with a
  `--target` per target that has that combo.
- Key summaries by `(package, combo, target-group)` and show `targets = [...]`.
  Singleton target groups should keep the normal `target = ...` summary shape.
- Fall back to serial per-target for `run` (Cargo rejects multiple `--target`),
  with a one-line note.
- Fall back to serial per-target when pruned summaries are enabled
  (`--show-pruned` or resolved config `show_pruned = true`), with a one-line
  note.
- Treat `--aggregate-targets` as a no-op for matrix and single-target runs, with
  a one-line note when the user explicitly passed the flag.
- Document and implement group-level count semantics: aggregate warning/error
  counts are for the Cargo invocation's target group and may differ from serial
  per-target counts. Recommend pairing `--aggregate-targets` with `--dedupe` for
  diagnostics-heavy output.
- Reuse the existing per-invocation and global dedupe; add no new output path.

### Milestone 5: Docs

- Leave `README.md` unchanged until the implementation is complete.
- Once the full feature lands, update `README.md` with the final shipped design
  for configured targets, target capability, and alias target opt-in.
- Document workspace targets.
- Document target-specific workspace `exclude_packages`.
- Document package-level targets and package opt-out with `targets = []`.
- Document built-in target capability and alias opt-in.
- Document the two execution modes: serial per-target (default, exact
  attribution) and `--aggregate-targets` (one multi-`--target` invocation per
  combo, group-level attribution, faster on many cores), why there is no worker
  pool, and that `--aggregate-targets` falls back to serial for `run`; include a
  short summary of the measurements.
- Document that `--aggregate-targets` falls back to serial when pruned summaries
  are enabled, is a no-op for matrix/single-target runs, and reports group-level
  counts that may differ from serial per-target counts. Recommend `--dedupe`
  when combining aggregate mode with diagnostics-only output.
- Document that the workspace `targets` list also applies to `test`/`run`, that
  foreign-target `test`/`run` typically fail, and that `--target` narrows a run.
- Document rustup target prerequisites.
- Document that configured target lists intentionally take precedence over
  `CARGO_BUILD_TARGET`; use explicit `--target` to select one target.
- Document the `cargo fc matrix` schema change: `target` is top-level and user
  matrix metadata is nested under `metadata`.
- Update GitHub Actions docs to show a single `cargo fc clippy` or
  `cargo fc check` invocation.

### No further parallelism milestone

Aggregate mode (the only candidate that survived the measurements in "Target
Execution Modes") is part of v1 as Milestone 4. A worker pool of concurrent Cargo
processes was measured and rejected, so no parallelism work remains.

## Testing Checklist

- Unit tests:
  - target flag parsing recognizes `--target x` and `--target=x` before `--`,
    and ignores `--target` after `--`,
  - target precedence,
  - target list dedupe with stable order,
  - target plan order preserves workspace target order before package-only
    targets,
  - same-triple package assignments can keep different target sources,
  - target-specific workspace `exclude_packages` removes packages only from
    matching target plans,
  - target-specific workspace `exclude_packages` applies to explicit
    single-target invocations,
  - workspace/package target resolution,
  - built-in `clippy` has target capability,
  - unknown `lint` lacks target capability unless configured,
  - configured-target warning is emitted once per invocation and is based on raw
    config state before capability filtering,
  - package configs are loaded once and deprecation warnings are not duplicated,
  - summary formatting includes target,
  - serial summary counts key by package, target, and features,
  - serial pruned summaries are attached only within the same package and target,
  - aggregate summaries key by package, canonical combo, and target group,
  - aggregate mode falls back to serial when pruned summaries are enabled,
  - invocation totals are computed up front and the progress counter increments
    in plan order,
  - implicit single-host runs do not print `target = ...` in headers or
    summaries,
  - per-combination diagnostics header includes the target when targets are
    configured, explicitly selected, or more than one is planned,
  - matrix rows include target and a `metadata` object,
  - package matrix metadata keys that match cargo-fc-owned top-level row keys
    are preserved under `metadata`,
  - aggregate mode batches targets sharing a combo into one invocation with a
    `--target` per target,
  - aggregate mode groups by canonical feature-combination key, not display
    text,
  - aggregate mode keeps targets with divergent feature matrices in separate
    per-combo invocations,
  - `--aggregate-targets` falls back to serial for `run`,
  - `--aggregate-targets` is a no-op for single-target execution and matrix.

- Integration tests:
  - no configured targets preserves existing behavior,
  - workspace targets multiply matrix rows,
  - package targets override workspace targets,
  - workspace target overrides change package participation per target,
  - workspace target overrides apply when `--target` selects a single matching
    target,
  - package `targets = []` opts out of workspace targets,
  - explicit `--target` ignores configured target lists,
  - target override sections still apply correctly,
  - configured target missing from rustup produces a clear failure,
  - `--diagnostics-only`/`--dedupe` behavior for `test`, `run`, and aliases is
    not regressed,
  - matrix output for a workspace with no configured targets still includes
    `target` = host on every row (additive schema change),
  - `--aggregate-targets` does not change matrix output,
  - serial and aggregate modes produce the same pass/fail verdict for a clean and
    a failing combination across two targets,
  - aggregate diagnostics counts are group-level and may differ from serial
    counts; with `--dedupe`, repeated common diagnostics are rendered once for
    the group,
  - aggregate summaries show `targets = [...]` for batched multi-target
    invocations and `target = ...` for singleton groups.

## Readiness Criteria

The v1 feature is complete when configured targets, package-level targets,
target-specific workspace exclusions, target capability, the serial per-target
and aggregate (`--aggregate-targets`) execution modes, matrix output, tests, and
final README documentation all land together.

Closed decisions:

- cargo-fc is single-threaded and never spawns concurrent Cargo processes. The
  only execution flag is `--aggregate-targets` (default off); there is no
  `--max-concurrent-targets` flag, no `max_concurrent_targets` config key, and no
  worker/thread pool.
- Target plans are deduplicated by target triple, while package-target
  assignments carry `TargetSource` for per-package injection decisions.
- Workspace target overrides may patch only `exclude_packages`.
- Command target capability is workspace-level policy. Built-in `clippy` is
  known by default; literal `lint` is unknown unless configured; any command
  token may be explicitly overridden with `targets = true/false`.
- Unknown aliases retain legacy best-effort feature selection, but configured
  targets require explicit target capability.
- Existing `--diagnostics-only`/`--dedupe` eligibility is preserved in v1.
- Execution model is resolved: two single-threaded modes — serial per-target
  (default, exact attribution) and aggregate `--aggregate-targets` (one
  multi-`--target` invocation per combo, group-level attribution for batched
  multi-target groups, group-level counts, serial fallback for `run` and pruned
  summaries). A worker pool of concurrent Cargo processes was measured and
  rejected (no gain on a shared `target/`, a net loss on small runners). See
  "Target Execution Modes."
- Leave `README.md` unchanged until the feature is fully implemented, then
  document the final shipped design.

## Error Messages

Missing Rust target:

```text
target `x86_64-pc-windows-msvc` is not installed
hint: run `rustup target add x86_64-pc-windows-msvc`
```

Configured targets with a command that lacks target capability:

```text
warning: not passing configured targets to cargo alias `lint` because it has no targets capability
hint: add [workspace.metadata.cargo-fc.subcommands.lint] targets = true if this alias accepts --target
```

`--aggregate-targets` with `run` (falls back to serial):

```text
note: --aggregate-targets does not apply to `run` (cargo runs one target at a time); running targets serially
```

`--aggregate-targets` with pruned summaries enabled:

```text
note: --aggregate-targets is disabled because pruned summaries are target-specific; running targets serially
```

`--aggregate-targets` with no effect:

```text
note: --aggregate-targets has no effect for a single target; running normally
note: --aggregate-targets has no effect for matrix output; matrix rows are always per target
```
