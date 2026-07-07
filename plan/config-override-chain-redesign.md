# Config Override Chain Redesign

This document is an implementation spec. It is written so that an agent
without prior context on this codebase can execute the redesign correctly.
Read it fully before writing code. Normative rules are numbered `S-*`
(semantics that must hold), `B-*` (deliberate behavior changes), `P-*`
(pitfalls), and milestones `M1`–`M5`. When this document and the current code
disagree, the document wins only where a `B-*` rule says so; everywhere else
the current behavior is the contract.

Verification commands (run after every milestone; all must pass):

```sh
task test        # cargo nextest run --workspace --all-targets
task lint        # cargo lint --workspace --all-features + ast-grep rules
```

## 1. Purpose

The README ("Override model" section) documents this model:

> Every setting resolves along **one precedence chain**, broadest to
> narrowest — workspace → package, and within each: base →
> `subcommands.<cmd>` → `target.'cfg(...)'` → `target.'cfg(...)'.subcommands.<cmd>`.

Eight scopes plus the CLI, uniform forms per setting (scalar override, set
patch, `replace`), and a matrix of where each setting is valid. The model is
simple; the implementation is not. The chain is hand-rolled six times:

1. `src/config/flags.rs::resolve_flags` — walks a `[Scope; 4]` array with the
   subcommand override interleaved per scope.
2. `src/config/flags.rs::resolve_driver_chain` — the identical walk for `driver`.
3. `src/config/flags.rs::resolve_target_capability` — the identical walk for
   `expand_targets` (with no `replace` handling).
4. `src/config/resolve.rs::resolve_config_with_flag_layers` — a different walk
   for the feature matrix (layer indices L1–L4, `apply_from <= N` arithmetic).
5. `src/plan/targets.rs::resolve_effective_exclude_packages` — a third walk
   for workspace package exclusion.
6. `src/plan/targets.rs::{workspace_effective_targets, package_target_list}` —
   a fourth walk for the `targets` list (which honors no `replace` at all).

Feeding these requires parallel plumbing: `PackageFlagLayers` (8 fields),
`ResolveCommandConfigArgs` (18 fields, four of which are passed empty at some
call sites), and five parallel `workspace_target_*` fields on `TargetPlan`.
Sibling `cfg(...)` sections are pre-merged per subcommand
(`combine_command_capability_maps`) but only for *some* fields — `features`
and `targets` are deliberately left out and read from the raw overrides by a
different pass, a trap documented by warning comments on both
`CommandCapabilities::merge` and `combine_command_capabilities`. Resolution
returns the raw schema type `Config` with three fields scrubbed
(`target_overrides` cleared, `package_targets` forced `None`, `deprecated`
reset). Adding one setting today touches ~seven places.

The redesign: implement the chain once (one scope payload view, one chain
constructor, one resolution engine, one validity matrix in validation), give
resolution its own output types, and delete the parallel walks and plumbing.

## 2. Glossary

- **Scope**: one place in TOML where settings may appear. There are eight:

  | id                      | TOML location                                                        |
  |-------------------------|----------------------------------------------------------------------|
  | `WsBase`                | `[workspace.metadata.cargo-fc]`                                      |
  | `WsCmd`                 | `[workspace.metadata.cargo-fc.subcommands.<cmd>]`                    |
  | `WsTarget`              | `[workspace.metadata.cargo-fc.target.'cfg(...)']`                    |
  | `WsTargetCmd`           | `[workspace.metadata.cargo-fc.target.'cfg(...)'.subcommands.<cmd>]`  |
  | `PkgBase`               | `[package.metadata.cargo-fc]`                                        |
  | `PkgCmd`                | `[package.metadata.cargo-fc.subcommands.<cmd>]`                      |
  | `PkgTarget`             | `[package.metadata.cargo-fc.target.'cfg(...)']`                      |
  | `PkgTargetCmd`          | `[package.metadata.cargo-fc.target.'cfg(...)'.subcommands.<cmd>]`    |

  ("cargo-fc" stands for any accepted metadata key alias; see
  `find_metadata_value`.) The CLI is a ninth, final overlay, not a scope.

- **Layer**: one scope instantiated for a concrete (package, target, command)
  resolution. A layer holds one or more **sibling entries**: for target
  scopes, every `cfg(...)` section whose expression matches the target; for
  other scopes, at most one entry.

- **Chain**: the ordered list of layers, broadest → narrowest:
  `WsBase, WsCmd, WsTarget, WsTargetCmd, PkgBase, PkgCmd, PkgTarget,
  PkgTargetCmd`, then the CLI overlay. Workspace-only resolutions use the
  first four; pre-target resolutions use the four non-target layers
  (`WsBase, WsCmd, PkgBase, PkgCmd`).

- **Settings** (the rows of the README matrix):
  - *flags*: the ~15 tri-state bools in `FlagConfig`.
  - *driver*: scalar string.
  - *expand_targets*: scalar bool, command scopes only.
  - *targets* (list): ordered target-triple list patch.
  - *exclude_packages*: string-set patch, workspace scopes only.
  - *feature matrix*: the ten `FeatureMatrixPatch` fields, package scopes only.
  - *replace*: reset marker, every scope except `WsBase`.

## 3. Behavior contract (must hold after the redesign)

These rules restate what the current code does. Every rule below is exercised
by existing tests unless noted; do not change any of them except where a
`B-*` rule explicitly overrides.

### S-1: Command layer selection

Given the raw command token (what the user typed, e.g. `t` or a custom
alias) and the resolved token (after cargo alias expansion, e.g. `test`),
a scope's `subcommands` map selects at most one entry via
`cli::selected_command_override(raw, resolved, map)`:

1. Look up the raw token: direct map hit, else the canonical name of a
   built-in short alias (`cli::builtin_command`, e.g. `t` → `test`). If found,
   use it.
2. Otherwise, if `resolved != raw`, repeat the lookup with the resolved token.
3. Otherwise, no command layer for this scope.

This is evaluated **per sibling entry**: when two `cfg(...)` sections both
match a target, each section's own `subcommands` map is consulted
independently, and the hits become the sibling entries of the
`*TargetCmd` layer. Keep `selected_command_override` and
`command_override_for_token` unchanged in `cli.rs`.

### S-2: Sibling combination (within one layer)

When a layer has multiple sibling entries (matching `cfg(...)` sections),
per-field combination rules apply, each labeled with the offending cfg
expression on error:

- **Scalar bool** (flags fields, `expand_targets`, feature bools): all
  siblings that set the field must agree, else error
  `conflicting values for `{name}` in {source_kind} `{expr}``. For flags in a
  command layer, `{name}` is prefixed `subcommands.{cmd}.` (test
  `conflicting_target_subcommand_flags_error` asserts
  `subcommands.check.pedantic`).
- **Driver**: values are trimmed before comparison; trimmed-equal values do
  not conflict; differing values error like scalar bools. (Empty rejection:
  see S-8 / B-5.)
- **Set patch** (`combine_set_patches` in `patch.rs`, keep as-is): sibling
  `override` values must be equal (as sets) or error
  `conflicting overrides for `{name}` from {source_kind} `{expr}``;
  `add`/`remove` contributions union.
- **`allow_feature_sets`**: at most one sibling may set it, else error
  `multiple matching {source_kind} entries set allow_feature_sets: {exprs}`.
- **`matrix` (JSON)**: no conflict detection; sibling maps deep-merge in cfg
  key order (`BTreeMap` iteration order): objects merge recursively, all
  other values overwrite (`merge_matrix`, keep as-is).
- **`replace`**: OR across siblings (B-3 relaxes the old feature-pass error).

`source_kind` strings by scope (preserve, tests match on them):
`WsTarget` → `workspace target override`; `PkgTarget` → `target override`;
`PkgTargetCmd` → `target subcommand override`; `PkgCmd` →
`package subcommand override`. Single-entry scopes never emit sibling
conflicts.

### S-3: `replace` semantics

`replace = true` on a layer discards every broader layer: resolution behaves
as if the chain started at the narrowest layer where any sibling sets
`replace`. Implement once: compute
`start = max index of a layer with replace` (0 if none), fold every setting
over `layers[start..]`, values before `start` never contribute, and
accumulated warning state (S-6's ignored-diagnostics flag) restarts. A
resetting layer may not use `add`/`remove` patch ops on any patch-typed
setting it carries (`{source_kind} `{expr}` uses add/remove patch operations
while replace=true: {fields}`) — validate all sibling entries of the
resetting layer. **Superseded by F-1 (§11): the add/remove-under-replace rule
becomes section-local and moves to validation time; this sentence described
(and the initial implementation faithfully reproduced) the old layer-based
check, which has two defects.** `WsBase` cannot carry `replace` (validation
rejects it). The CLI overlay is applied regardless of `replace`.

Interactions that must come out exactly as today:

- Package-base `replace` resets inherited workspace flags and driver
  (`driver_replace_at_package_discards_inherited_workspace_driver`).
- A resetting `PkgTarget` layer discards `PkgBase` and `PkgCmd` feature
  layers but not `PkgTargetCmd`
  (`replace_resets_base_subcommand_layer_but_keeps_target_subcommand`).
- Feature fields are unaffected by workspace-layer resets since workspace
  scopes carry no feature fields — no special-casing needed; this falls out
  of the slice. (Today's code special-cases "L1 replace is not a feature
  reset"; with slicing at `PkgBase` the features base is re-applied from the
  `PkgBase` layer itself, which yields the same result — see S-7 on base
  values being patches onto defaults.)
- `exclude_packages` resolves on the workspace chain only (S-9), so package
  `replace` never un-excludes packages.

### S-4: Flag overlay and same-scope validation

Keep `FlagConfig`, the two field-list macros, `overlay`, `validate`,
`requests_diagnostics`, `mentions_diagnostics`, `ResolvedFlags`,
`from_config`, `try_from_config` in `flags.rs` **unchanged**. For reference,
the semantics that must survive:

- Each raw scope validates in isolation: `no_prune_implied` +
  `prune_implied` both set → error; `dedupe = true` + `diagnostics_only =
  false` in one scope → error.
- Overlay: a `Some` from a narrower layer replaces; plus three couplings —
  (a) `diagnostics_only = Some(false)` without `dedupe = Some(true)` also
  forces accumulated `dedupe = Some(false)`; (b) `dedupe = Some(true)` with
  no `diagnostics_only` in the same layer clears an accumulated
  `diagnostics_only = Some(false)`; (c) setting either prune spelling clears
  the accumulated other spelling.
- `ResolvedFlags::from_config`: `diagnostics_only = diagnostics_only
  .unwrap_or(false) || dedupe.unwrap_or(false)`; `no_prune_implied =
  no_prune_implied.or(prune_implied.map(|e| !e)).unwrap_or(false)`; all other
  fields `unwrap_or(false)`.
- `try_from_config` errors when the merged config has `dedupe = Some(true)`
  and `diagnostics_only = Some(false)`.

### S-5: Flag fold over the chain

For each layer from the replace-slice start, in order:

1. Combine siblings (S-2) into one `FlagConfig`; call `validate()` on it.
2. If the layer is **not** a command layer and
   `default_diagnostics_allowed == false`, gate it (S-6) before overlaying.
3. If the layer **is** a command layer: if the combined flags mention
   diagnostics (`diagnostics_only` or `dedupe` is `Some`), clear the
   ignored-diagnostics flag; overlay ungated.
4. Overlay onto the accumulator (`FlagConfig::overlay`).

Then overlay the CLI flags, run `ResolvedFlags::try_from_config`, and set
`ignored_diagnostics_config &&= !flags.diagnostics_only`.

### S-6: Diagnostics gate (non-command layers, diagnostics-unsafe commands)

`default_diagnostics_allowed` is `cli::builtin_diagnostics_safe(resolved_or_raw)`
(true for `build`, `check`, `clippy`, `doc` and their aliases). When false,
a non-command layer's flags pass through `gated_plain_diagnostics` (keep the
function): if the layer requests diagnostics (`diagnostics_only == Some(true)`
or `dedupe == Some(true)`), set the ignored flag and strip those `Some(true)`
values (leaving `Some(false)` intact, which still overlays); else if the
layer merely mentions diagnostics, clear the ignored flag. Command layers
bypass the gate entirely (a `subcommands.<cmd>` table is explicit
command-local intent). The resulting `ignored_diagnostics_config` reaches
`PackageExecutionPlan` and drives `lib.rs::warn_ignored_diagnostics_config`
(text unchanged).

### S-7: Feature-matrix fold

Feature fields exist only on package-scope layers (`PkgBase`, `PkgCmd`,
`PkgTarget`, `PkgTargetCmd`). Resolution starts from
`ResolvedFeatures::default()` (all empty/false) and folds layers from the
slice start:

- Set-like fields (`exclude_features`, `include_features`, `only_features`:
  string sets; `isolated_feature_sets`, `exclude_feature_sets`,
  `include_feature_sets`, `allow_feature_sets`: feature-set lists): combine
  siblings via `combine_set_patches`, then apply
  (override-or-current → remove → add; **add wins ties**).
- Bools (`skip_optional_dependencies`, `no_empty_feature_set`): sibling
  scalar combine; narrower `Some` wins.
- `matrix`: deep-merge each layer onto the accumulator (S-2 rule).
- The `PkgBase` layer's concrete values act as patches applied to the empty
  default — `exclude_features = ["a"]` at base is `Override({"a"})` onto `{}`,
  which reproduces today's "base config is the starting point" exactly.

Precedence pins (unit tests exist for all):
`PkgTargetCmd` > `PkgTarget` > `PkgCmd` > `PkgBase`
(`feature_layer_precedence_target_beats_subcommand`); a command-less
resolution (`raw = resolved = None`) applies no command layers; a different
command leaves command layers inert.

### S-8: Driver

Scalar fold with replace slicing; narrower layer wins; sibling combine trims
before comparing (S-2). CLI `--driver` overlays last and always wins. The
resolved value is **unnormalized**: `None` = unset, `Some("cargo")` = explicit
plain cargo. Everything downstream of resolution stays as-is:
`lib.rs::finalize_plan_drivers` (cross-target `cargo-zigbuild` default only
when some plan has `driver == None`), `finalize_driver`, `normalize_driver`
(`"cargo"` → `None` so spawn honors `$CARGO`), the runner's `CARGO_DRIVER`
env export, and the aggregate-targets serial fallback when per-target drivers
differ. Empty-driver rejection: see B-5.

### S-9: `exclude_packages`

Resolved per (target × command) — not per package — at execution-plan time,
over the **workspace chain only** (`WsBase, WsCmd, WsTarget, WsTargetCmd`).
The fold seeds from `base_exclude` =
`Workspace::base_workspace_exclude_packages()` (workspace base set ∪ the
deprecated root-package `exclude_packages`), then folds `WsCmd`, `WsTarget`,
`WsTargetCmd` patches with replace slicing (a resetting layer starts from the
empty set, discarding `base_exclude`). A narrower `remove` can re-include a
package excluded by a broader scope
(`subcommand_exclude_packages_remove_reincludes_for_command`). Package layers
never participate; package `replace` must not affect this fold (see S-3).
The resulting set filters `TargetPlan.packages` by package name in
`build_execution_plans`.

### S-10: `targets` list

Resolved on the four non-target layers (`WsBase, WsCmd, PkgBase, PkgCmd`) —
target sections cannot carry it (circularity; validation rejects). Ordered
semantics (until M4, keep today's exactly): start from the workspace list in
declared order; apply each layer's patch via `apply_target_patch` (override
replaces the whole list **sorted**; `remove` filters preserving order; `add`
appends new entries **sorted**; entries already present are not re-added).
After M4 (B-4) order becomes declaration order throughout.

Normalization (`normalize_targets`, keep): trim each triple, error
`empty target triple in configured `targets` list` on empty, dedup preserving
first occurrence.

Interaction with planning (all stays in `plan/targets.rs`, only the list
computation is replaced):

- Empty effective list → single fallback target (`CARGO_BUILD_TARGET` env,
  else host), with `show_target = true` iff any configured patch applied
  ("patched"), so an explicit opt-out (`targets = []`) still attributes the
  fallback as configured.
- `TargetSource` provenance: if any package-scope layer (`PkgBase`/`PkgCmd`)
  touched the list → `PackageConfig`; else if configured → `WorkspaceConfig`;
  fallback sources `CargoBuildTargetEnv`/`Host`; explicit `--target` → `Cli`.
- Explicit `--target <triple>` overrides all configured lists for every
  package (`TargetExpansion::Explicit`); denied capability ignores configured
  lists (`TargetExpansion::Denied`); empty `--target` value errors.
- Global plan order: workspace-list order first, then package-only targets in
  selected-package order, dedup by triple; per-target cfg evaluation happens
  only for planned targets (`unused_workspace_target_does_not_evaluate_overrides`).

### S-11: `expand_targets` capability and two-phase planning

`expand_targets` (bool) appears only in `subcommands` tables. Resolution is a
scalar fold over the chain's command layers with default
`default_targets_enabled`; `targets_explicit` records whether any layer
supplied the final value (see B-6/B-7 for replace/explicitness edges). It is
resolved **twice**:

1. **Pre-target phase** (`lib.rs::selected_packages_for_target_planning`):
   chain = `WsBase, WsCmd, PkgBase, PkgCmd` (no target layers exist yet);
   `default_targets_enabled` = `matrix` command or
   `cli::builtin_target_capability(resolved_or_raw)` (any built-in). Output
   per package: `ignore_configured_targets = flags.no_targets || !enabled`,
   `target_decision_explicit = flags.no_targets == true || explicit`. These
   drive `TargetExpansion::{Configured,Denied}` and
   `warn_if_configured_targets_ignored` (text unchanged).
2. **Per-target phase** (`build_execution_plans`): full chain,
   `default_targets_enabled = true` (planning already decided); if the
   assignment's source is configured (`WorkspaceConfig`/`PackageConfig`) and
   `flags.no_targets || !enabled`, the package-target is skipped
   (`target_selection_skipped`), feeding
   `warn_packages_skipped_by_target_selection` (text unchanged).

### S-12: Loading, deprecated keys, validation errors

- Metadata key aliases: `find_metadata_value` picks the first present alias;
  unchanged.
- Package deprecated keys (`skip_feature_sets` → `exclude_feature_sets`,
  `denylist` → `exclude_features`, `exact_combinations` →
  `include_feature_sets`): accepted at `PkgBase` only; each emits one
  deprecation warning naming section and package
  (`package.rs::config()`); values fold into the target field. Deprecated
  root-package `exclude_packages`: accepted at `PkgBase`, folded into the
  workspace base exclude by `base_workspace_exclude_packages` (no warning
  there; `warn_workspace_metadata_misuse` warns once).
- Validation walks the raw JSON before deserialization and rejects:
  - unknown keys: `unknown cargo-fc config key `{key}` in [{section}]` plus
    `; cargo-fc config keys use `_`, not `-`` when the key contains `-`;
  - known-but-misplaced keys with per-key reasons (preserve wording):
    feature-matrix keys/deprecated spellings in workspace scope
    ("feature-matrix settings are per-package …"); `exclude_packages` outside
    workspace scope ("… only valid in workspace scope"); `targets` anywhere
    inside a `target.'cfg(...)'` section ("not valid anywhere inside …
    circular …"); `replace` at `WsBase` ("nothing for it to reset");
    `expand_targets` outside a `subcommands` table ("per-subcommand
    capability").
  - Section labels in errors use the user's alias:
    `package.metadata.{key}[.target.'{cfg}'][.subcommands.{name}]`.
- The `dedup` spelling is a serde alias for `dedupe` and is accepted wherever
  flags are.

## 4. Target architecture

### 4.1 Module layout

```
src/config/
  mod.rs       re-exports (update as types move)
  schema.rs    serde types (M4: ScopeConfig, SectionConfig, RootConfig, DeprecatedTomlKeys)
  patch.rs     StringSetPatch, FeatureSetVecPatch, SetPatchOps, combine_set_patches (unchanged),
               + TargetListPatch (M4)
  flags.rs     FlagConfig macros, ResolvedFlags, gate + overlay policies (shrinks: the
               chain walkers move out / are deleted)
  scope.rs     NEW: ScopeId, ScopeView, Layer, Chain + constructors
  resolve.rs   REWRITTEN: the engine (replace slicing, scalar/set/flags/feature folds),
               Resolved, ResolvedFeatures, public resolve_config wrapper
  validate.rs  REWRITTEN: SettingKind + validity matrix + JSON walk
```

Unchanged modules: `patch.rs` (except M4 addition), `cfg_eval.rs`,
`target.rs`, `cargo_alias.rs`, `runner.rs`, `matrix.rs`, `tee.rs`,
`diagnostics_only.rs`, `implication.rs` (signature only: takes
`&ResolvedFeatures`), `invocation_args.rs`, `target_install.rs`, and all
warning text in `lib.rs`.

### 4.2 Core types (`scope.rs`)

```rust
/// One position in the precedence chain. Order of variants = chain order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ScopeId {
    WorkspaceBase,
    WorkspaceCommand,
    WorkspaceTarget,
    WorkspaceTargetCommand,
    PackageBase,
    PackageCommand,
    PackageTarget,
    PackageTargetCommand,
}

impl ScopeId {
    pub(crate) fn is_command(self) -> bool { /* the four *Command variants */ }
    pub(crate) fn is_package(self) -> bool { /* the four Package* variants */ }
    /// The `source_kind` string used in error messages (S-2 table).
    pub(crate) fn source_kind(self) -> &'static str;
}

/// Uniform borrowed view of what one scope said. Until M4 this is assembled
/// from the five legacy schema types; after M4 it is a trivial projection of
/// `ScopeConfig`.
#[derive(Clone, Copy, Default)]
pub(crate) struct ScopeView<'a> {
    pub(crate) replace: bool,
    pub(crate) driver: Option<&'a str>,
    pub(crate) expand_targets: Option<bool>,
    pub(crate) targets: Option<&'a StringSetPatch>,       // TargetListPatch after M4
    pub(crate) exclude_packages: Option<&'a StringSetPatch>,
    pub(crate) features: Option<&'a FeatureMatrixPatch>,
    pub(crate) flags: FlagConfig,
}

pub(crate) struct Layer<'a> {
    pub(crate) scope: ScopeId,
    /// Selected command name when `scope.is_command()`, for error labels
    /// (`subcommands.{cmd}.{field}`).
    pub(crate) command: Option<&'a str>,
    /// (label, payload). Label is the cfg expression for target scopes, ""
    /// otherwise. Non-target layers have exactly one entry.
    pub(crate) entries: Vec<(&'a str, ScopeView<'a>)>,
}

pub(crate) struct Chain<'a> {
    layers: Vec<Layer<'a>>,   // chain order; layers with no entries omitted
}
```

Constructors (until M4 these take the legacy types; adapt in place at M4):

```rust
impl<'a> Chain<'a> {
    /// WsBase, WsCmd, PkgBase, PkgCmd. `pkg = None` for workspace-only chains
    /// (then only the first two layers). Used pre-target (S-11 phase 1) and
    /// for the `targets` list (S-10).
    pub(crate) fn base(
        ws: &'a WorkspaceConfig,
        pkg: Option<&'a Config>,
        raw: Option<&'a str>,
        resolved: Option<&'a str>,
    ) -> Self;

    /// All eight layers for one (package, target, command). `ws_matched` are
    /// the workspace target sections already matched per target plan;
    /// package target sections are matched here via `matching_overrides`.
    pub(crate) fn full(
        ws: &'a WorkspaceConfig,
        ws_matched: &'a [(String, &'a WorkspaceTargetOverride)],
        pkg: &'a Config,
        pkg_matched: Vec<(&'a str, &'a TargetOverride)>,
        raw: Option<&'a str>,
        resolved: Option<&'a str>,
    ) -> Self;

    /// WsBase, WsCmd, WsTarget, WsTargetCmd only — for S-9.
    pub(crate) fn workspace(
        ws: &'a WorkspaceConfig,
        ws_matched: &'a [(String, &'a WorkspaceTargetOverride)],
        raw: Option<&'a str>,
        resolved: Option<&'a str>,
    ) -> Self;
}
```

Command layers are built per S-1 (per-sibling `selected_command_override`).
`matching_overrides` (cfg matching, `resolve.rs`) is kept and reused.

### 4.3 Engine (`resolve.rs`)

```rust
/// Feature-matrix output: exactly the fields feature generation reads.
#[derive(Debug, Clone, Default)]
pub struct ResolvedFeatures {
    pub exclude_features: HashSet<String>,
    pub include_features: HashSet<String>,
    pub only_features: HashSet<String>,
    pub isolated_feature_sets: Vec<HashSet<String>>,
    pub exclude_feature_sets: Vec<HashSet<String>>,
    pub include_feature_sets: Vec<HashSet<String>>,
    pub allow_feature_sets: Vec<HashSet<String>>,
    pub skip_optional_dependencies: bool,
    pub no_empty_feature_set: bool,
    pub matrix: serde_json::Map<String, serde_json::Value>,
}

/// Everything one (package × target × command) needs.
pub(crate) struct Resolved {
    pub(crate) flags: ResolvedFlags,
    pub(crate) ignored_diagnostics_config: bool,
    pub(crate) driver: Option<String>,
    pub(crate) targets_enabled: bool,
    pub(crate) targets_explicit: bool,
    pub(crate) features: ResolvedFeatures,
}

pub(crate) struct CliOverlay<'a> {
    pub(crate) flags: FlagConfig,
    pub(crate) driver: Option<&'a str>,
}

pub(crate) struct ResolvePolicy {
    pub(crate) default_diagnostics_allowed: bool,
    pub(crate) default_targets_enabled: bool,
}

impl Chain<'_> {
    pub(crate) fn resolve(&self, cli: CliOverlay<'_>, policy: ResolvePolicy)
        -> eyre::Result<Resolved>;

    /// The command-aware effective exclude set (S-9), seeded from base_exclude.
    pub(crate) fn exclude_packages(&self, base_exclude: &HashSet<String>)
        -> eyre::Result<HashSet<String>>;

    /// The effective ordered targets list (S-10); also reports whether any
    /// patch applied ("patched") and whether a package layer touched it.
    pub(crate) fn targets_list(&self, workspace_base: &[String])
        -> eyre::Result<TargetListResolution>;
}
```

Seeding note for `exclude_packages` and `targets_list` (until M4): the seed
argument (`base_exclude` / `workspace_base`) *is* the `WsBase` layer's value
for replace-slicing purposes — if the slice starts at any later layer, the
seed is discarded and the fold starts empty (this is what makes B-2 work).
After M4 the `WsBase` `ScopeConfig` carries these values itself and the seed
parameters can be dropped (`base_exclude` still needs the deprecated
root-package union folded into the `WsBase` value at load time).

`resolve` algorithm:

1. `start` = index of the narrowest layer with any sibling `replace` (S-3);
   validate every entry of that layer carries no `add`/`remove` on
   `targets`, `exclude_packages`, or any feature set field.
2. Flags fold per S-5/S-6 over `layers[start..]`, CLI overlay, finalize.
3. Driver scalar fold per S-8, CLI overlay.
4. `expand_targets` scalar fold; `enabled = value.unwrap_or(default)`,
   `explicit = value.is_some()`.
5. Feature fold per S-7 over package layers in `layers[start..]`.

Public wrapper kept for `tests/target_overrides.rs` and external use:

```rust
/// Resolve target-specific feature config with no workspace and no command.
pub fn resolve_config<E: CfgEvaluator>(base: &Config, target: &TargetTriple,
    evaluator: &mut E) -> eyre::Result<ResolvedFeatures>;
```

(That test also asserts flag overlays via the old return type; move those
assertions to engine unit tests and keep the integration test on features —
see §9.)

### 4.4 Caller changes

- **`plan/targets.rs`**
  - `TargetPlan<'a>` drops `workspace_target_flags`, `workspace_target_replace`,
    `workspace_target_driver`, `workspace_target_exclude_ops`,
    `workspace_target_subcommands`; gains
    `pub(crate) ws_matched: Vec<(String, &'a WorkspaceTargetOverride)>` (the
    sections whose cfg matched this triple, in map order). Tie the workspace
    borrow into `'a` (`build_target_plans(…, workspace_config: &'a
    WorkspaceConfig, …)`); all call sites already keep the workspace config
    alive long enough.
  - Delete `resolve_workspace_target_config`, `WorkspaceTargetConfig`,
    `resolve_effective_exclude_packages`, `workspace_effective_targets`,
    `WorkspaceEffectiveTargets`, `package_target_list`'s patch plumbing.
    `build_target_plans` computes each package's list via
    `Chain::base(ws, Some(pkg.config), raw, resolved).targets_list(&ws_list)`
    and keeps all fallback/ordering/source/show_target logic (S-10).
  - `TargetPlans.base_exclude` stays (threaded to execution).
- **`plan/execution.rs`**
  - Per target plan: `excluded = Chain::workspace(ws, &plan.ws_matched, raw,
    resolved).exclude_packages(&target_plans.base_exclude)?`.
  - Per package: `resolved = Chain::full(ws, &plan.ws_matched, planned.config,
    matching_overrides(&planned.config.target_overrides, &plan.target, eval)?,
    raw, resolved).resolve(cli, policy)?`. Replaces the
    `resolve_config_with_flag_layers` + `resolve_package_command_config` pair.
  - `PackageExecutionPlan.matrix` ← `resolved.features.matrix`; feature
    generation takes `&resolved.features`.
- **`package.rs`**: `Package::feature_combinations` and
  `Package::feature_matrix` change their parameter from `&Config` to
  `&ResolvedFeatures` (they only read feature fields);
  `implication::maybe_prune_with_resolved_flag` likewise. `Package::config()`
  keeps returning the raw schema type (`Config` until M4, `RootConfig`
  after).
- **`lib.rs`**
  - `selected_packages_for_target_planning` uses `Chain::base` — delete the
    18-field `ResolveCommandConfigArgs` call with fabricated empty layers.
  - Everything else (driver finalization, warnings, execution mode) unchanged.
- **Deleted outright** (after M3 nothing references them):
  `PackageFlagLayers`, `ResolvedTargetConfig`,
  `resolve_config_with_flag_layers`, `feature_replace_layer`,
  `apply_feature_layer` (subsumed), `validate_replace_feature_patches`
  (generalized into S-3 validation), `ResolveCommandConfigArgs`,
  `ResolvedCommandConfig`, `resolve_command_config`, `resolve_flags`,
  `resolve_driver_chain`, `resolve_target_capability`, `Scope` (flags.rs),
  `combine_flag_configs`, `combine_bool`, `combine_driver`, `combine_scalar`
  (re-homed into the engine as private helpers is fine),
  `combine_command_capability_maps`, `combine_command_capabilities`,
  `CommandCapabilities::merge`, `SetPatchOps::into_string_set_patch`.

### 4.5 Unified schema (M4)

```rust
/// What one scope may say. Every field optional; absent = inherit.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct ScopeConfig {
    #[serde(default)] pub replace: bool,
    #[serde(default)] pub driver: Option<String>,
    #[serde(default)] pub expand_targets: Option<bool>,
    #[serde(default)] pub targets: Option<TargetListPatch>,
    #[serde(default)] pub exclude_packages: Option<StringSetPatch>,
    #[serde(flatten)] pub features: FeatureMatrixPatch,
    #[serde(default, flatten)] pub flags: FlagConfig,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct SectionConfig {
    #[serde(flatten)] pub settings: ScopeConfig,
    #[serde(default, rename = "subcommands")]
    pub subcommands: BTreeMap<String, ScopeConfig>,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct RootConfig {
    #[serde(flatten)] pub base: SectionConfig,
    #[serde(default, rename = "target")]
    pub targets: BTreeMap<String, SectionConfig>,
    #[serde(flatten)] pub(crate) deprecated: DeprecatedTomlKeys,
}
```

`Config`, `WorkspaceConfig`, `TargetOverride`, `WorkspaceTargetOverride`,
`CommandCapabilities` are replaced by `RootConfig`/`SectionConfig`/
`ScopeConfig`. Both metadata roots deserialize as `RootConfig`; scope-validity
is validation's job (S-12), matching the existing pattern where
`CommandCapabilities` was shared and key-gated. `DeprecatedTomlKeys` gains
`exclude_packages: HashSet<String>` (the deprecated package-base flat key —
today a first-class `Config` field). Deprecated folding (package loading):
fold each deprecated collection into the corresponding
`base.settings.features` patch — into its `override` payload when present,
else into `add` (onto the empty default both are equivalent to today's
`extend`).

`TargetListPatch` (in `patch.rs`): same untagged serde surface as
`StringSetPatch` (plain array = override; `{override/add/remove}` object) but
`Vec<String>`-backed, preserving declaration order; implement `SetPatchInput`
is *not* needed — it gets a small dedicated combine (sibling overrides must
be equal as ordered-deduped lists; adds/removes concatenate in layer order,
deduped) and ordered apply (override replaces in declared order; remove
filters; add appends unseen entries in declared order).

Flatten-key disjointness: `ScopeConfig`'s own keys, `FeatureMatrixPatch`
keys, and `FlagConfig` keys share one flat TOML table. Keep one test that
serializes defaults of each and asserts pairwise disjointness (replaces the
existing comment-invariant on `TargetOverride`).

## 5. Validation (`validate.rs`)

Replace the seven allowlist consts and `misplaced_key_reason` with data:

```rust
enum SettingKind { Flag, FeatureMatrix, DeprecatedFeature, ExcludePackages,
                   TargetsList, ExpandTargets, Driver, Replace,
                   TargetTable, SubcommandsTable }

/// key → kind. Flag keys come from FLAG_KEYS (incl. "dedup"); feature keys
/// from FEATURE_MATRIX_KEYS (keep that const, it is the single list).
fn setting_kind(key: &str) -> Option<SettingKind>;

/// The README matrix. Err(reason) carries the user-facing explanation.
fn valid_in(kind: SettingKind, scope: ScopeId) -> Result<(), &'static str>;
```

The matrix (rows = kind, allowed scopes):

| kind               | allowed at                                              | reason when rejected |
|--------------------|---------------------------------------------------------|----------------------|
| Flag               | all eight                                               | — (never rejected)   |
| Driver             | all eight                                               | —                    |
| Replace            | all except `WsBase`                                     | "nothing for it to reset" (S-12 wording) |
| ExpandTargets      | `WsCmd`, `WsTargetCmd`, `PkgCmd`, `PkgTargetCmd`        | "per-subcommand capability" |
| TargetsList        | `WsBase`, `WsCmd`, `PkgBase`, `PkgCmd`                  | circularity wording  |
| ExcludePackages    | `WsBase`, `WsCmd`, `WsTarget`, `WsTargetCmd`, `PkgBase` (deprecated) | "only valid in workspace scope" |
| FeatureMatrix      | `PkgBase`, `PkgCmd`, `PkgTarget`, `PkgTargetCmd`        | "per-package"        |
| DeprecatedFeature  | `PkgBase`                                               | same as FeatureMatrix |
| TargetTable        | `WsBase`, `PkgBase`                                     | unknown-key error    |
| SubcommandsTable   | `WsBase`, `WsTarget`, `PkgBase`, `PkgTarget`            | unknown-key error    |

The walk: `validate_package_metadata(value, section)` visits `PkgBase`, each
`target.'cfg'` (`PkgTarget`), each `subcommands.<n>` (`PkgCmd`), each
`target.'cfg'.subcommands.<n>` (`PkgTargetCmd`); the workspace variant
mirrors with Ws scopes. For each key: `setting_kind` → known? check
`valid_in`, emit reason on Err; unknown → S-12 unknown-key error with the
hyphen hint. Additionally (B-5): if a `driver` value is a string that trims
empty, error `` `driver` must not be empty in [{section}] ``.

Note: the README matrix row for `expand_targets` shows ✓ in base/target
columns, but the key has only ever been accepted inside `subcommands`
tables; encode the code truth (command scopes only) and fix the README row
in M5.

Tests to keep (rewritten against the matrix): the serde-vs-allowlist sync
test becomes "every serialized `ScopeConfig`/`SectionConfig`/`RootConfig` key
has a `SettingKind`" plus "every `SettingKind` maps to at least one real
key"; keep the scope-rejection tests (`workspace_base_rejects_replace`,
`package_subcommand_rejects_exclude_packages`,
`workspace_subcommand_rejects_feature_matrix_keys`,
`targets_list_rejected_in_target_nested_subcommand`,
`misplaced_known_keys_get_scope_aware_reasons`,
`driver_is_accepted_in_every_scope`, hyphen hint) verbatim — they assert the
user-facing contract.

## 6. Deliberate behavior changes

Each lands with a CHANGELOG entry and test updates listed in §9.

- **B-1** `replace = true` + `add`/`remove` in the same section becomes an
  error for `exclude_packages` and `targets` (today: silently applies after
  the reset; features already error). One rule via S-3.
- **B-2** `replace = true` at `PkgBase`/`PkgCmd` now also resets the
  inherited workspace `targets` list (today: the targets walk ignores
  `replace`). README: "discards everything broader".
- **B-3** Two matching sibling cfg sections both setting `replace = true`
  stops erroring (today: the feature pass errors, the flag pass ORs).
  Siblings OR; real disagreements are still caught by S-2 conflicts. Delete
  the `multiple matching target overrides have replace = true` error and its
  test.
- **B-4** (M4) `targets` overrides/adds keep declaration order instead of
  sorted order; workspace base list keeps declared order as today. Update
  the two targets-patch unit tests that pin sorted output.
- **B-5** `driver = ""`/whitespace is rejected at validation time in every
  scope (today: resolution-time via `normalize_driver` at some scopes,
  sibling-combine time at target scopes — meaning an empty driver in a
  non-matching cfg section is currently *not* rejected). The
  `combine_driver_trims_whitespace_and_rejects_empty` unit test moves its
  empty-rejection half to validation tests. `normalize_driver` keeps its
  check as a backstop for the CLI value.
- **B-6** `expand_targets` now honors `replace` slicing (today
  `resolve_target_capability` ignores `replace`, so a broader explicit
  `expand_targets = false` survives a narrower reset). After a reset with no
  narrower value it falls back to `default_targets_enabled`.
- **B-7** `targets_explicit` becomes "the final value came from config"
  (provenance) instead of "any command layer ever set it, even if later
  replaced". Only observable through B-6 corner cases in
  `warn_if_configured_targets_ignored`.
- **B-8** (M4) Uniform forms per the README: patch objects (`{add/remove}`)
  become *accepted* where only plain arrays parse today (feature keys at
  `PkgBase`, `exclude_packages` at `WsBase`), applying onto the empty default.
  Explicit `targets = []` at `WsBase` becomes distinguishable from an absent
  key and counts as a configured opt-out (fallback gets `show_target = true`),
  matching the package-level opt-out semantics.

Anything else that changes observable behavior is a bug. The integration
suites (`tests/per_target.rs`, `tests/per_subcommand.rs`, `tests/driver.rs`,
`tests/target_overrides.rs`, `tests/metadata_key_aliases.rs`,
`tests/prune_implied.rs`, `tests/allow_feature_sets.rs`, and the runner
tests) pin the TOML-observable contract and must pass with no changes except
those enumerated in §9.

## 7. Milestones

Each milestone must end with `task test` and `task lint` green. Do not start
the next milestone with the previous one red. Commit per milestone
(lowercase imperative subjects, e.g.
`refactor(config): drive validation from one setting-scope matrix`; no
Co-Authored-By or generated-with trailers).

### M1 — validation rewrite (no schema change, no behavior change except B-5)

1. Add `ScopeId` in a new `src/config/scope.rs` (just the enum + helpers;
   chain types come in M2).
2. Rewrite `validate.rs` per §5, keeping `validate_package_metadata` /
   `validate_workspace_metadata` signatures and all error wording (S-12).
3. Add B-5 empty-driver validation + tests.
4. Port the existing validate tests; replace the three allowlist-sync tests
   with the matrix-coverage tests (§5).

### M2 — engine (additive; nothing switches over yet)

1. `scope.rs`: `ScopeView`, `Layer`, `Chain` with the three constructors
   building views **from the legacy schema types** (`WorkspaceConfig`,
   `Config`, `TargetOverride`, `WorkspaceTargetOverride`,
   `CommandCapabilities` → `ScopeView` field-by-field).
2. `resolve.rs`: add `ResolvedFeatures`, `Resolved`, `CliOverlay`,
   `ResolvePolicy`, `Chain::{resolve, exclude_packages, targets_list}` per
   §4.3 and S-2…S-11. Reuse `combine_set_patches`, `merge_matrix`,
   `matching_overrides`, `gated_plain_diagnostics`, `FlagConfig` policies.
3. Unit tests: port every scenario from the current `resolve.rs` and
   `flags.rs` test modules to the engine (see §9 mapping). This is the bulk
   of M2; the old tests stay in place and green alongside.

### M3 — switchover and deletion (B-1, B-2, B-3, B-6, B-7 land here)

1. `plan/targets.rs`: `TargetPlan` carries `ws_matched`; targets list via
   `Chain::base(...).targets_list(...)` (keep `apply_target_patch`'s sorted
   semantics for now by implementing the list fold with the existing
   `StringSetPatch` — B-4 waits for M4); delete the six walks' plumbing
   (§4.4).
2. `plan/execution.rs`: per-target excludes via `Chain::workspace`,
   per-package resolution via `Chain::full(...).resolve(...)`.
3. `lib.rs`: `selected_packages_for_target_planning` via `Chain::base`.
4. Delete everything in §4.4's deletion list; delete their now-moved tests.
5. Update the tests affected by B-1/B-2/B-3/B-6 (§9).

### M4 — schema unification (B-4, B-8 land here)

1. `patch.rs`: add `TargetListPatch` (§4.5) + unit tests (array=override,
   object form, ordered apply, sibling combine).
2. `schema.rs`: replace the five types with
   `ScopeConfig`/`SectionConfig`/`RootConfig`; `ScopeView` becomes a trivial
   projection; `Chain` constructors simplify; `package.rs::config()` and
   `workspace.rs::workspace_config()` return the new types; deprecated
   folding per §4.5; `base_workspace_exclude_packages` reads the deprecated
   pkg-base key from `DeprecatedTomlKeys`.
3. Flatten-disjointness test (§4.5). Keep the existing
   "flattened feature/flag key splitting" serde tests, retargeted at
   `ScopeConfig`.
4. Update B-4/B-8 tests (§9).

### M5 — docs and drift guards

1. CHANGELOG entries for B-1…B-8.
2. README: fix the `expand_targets` matrix row (command scopes only); no
   other README changes needed — the model is already documented.
3. Optional: a test that renders the README's `✓/—` matrix from `valid_in`
   and asserts it matches the table in README.md, so docs and code cannot
   drift.

## 8. Pitfalls — read before coding

- **P-1** Do not pre-merge `subcommands` maps across sibling cfg sections.
  Keep siblings as separate layer entries until each field's fold. (The old
  half-merged `CommandCapabilities` is the main defect this redesign removes.)
- **P-2** The raw-token-first rule (S-1) is per sibling map. Two matching cfg
  sections may select entries under *different* names (one via raw, one via
  resolved); both become siblings of the same layer.
- **P-3** `serde(untagged)` patch enums swallow shape errors silently — do
  not "improve" them; validation catches unknown keys before deserialization.
  `serde(flatten)` is incompatible with `deny_unknown_fields`; unknown-key
  detection must stay in `validate.rs`, not serde.
- **P-4** Add wins over remove within one combined patch (`apply`:
  override-or-base → remove → add). Test:
  `add_wins_over_remove_for_same_value`.
- **P-5** The diagnostics gate strips only `Some(true)`; a gated layer's
  `diagnostics_only = Some(false)` still overlays (and via S-4 coupling can
  force `dedupe = Some(false)`). Test:
  `broad_diagnostics_true_with_dedupe_false_still_warns`.
- **P-6** `ignored_diagnostics_config` must end up false whenever the final
  flags have `diagnostics_only = true` (CLI can rescue: test
  `broad_config_diagnostics_are_gated_but_cli_flags_win`).
- **P-7** `exclude_packages` folds over the workspace chain only (S-9). If
  you fold it over the full chain, package `replace` will wrongly clear
  workspace excludes — no current test catches this, so add one (§9).
- **P-8** Feature resolution must be identical for `raw/resolved = None`
  (matrix + command-less paths) — no command layers at all, not "command
  layers with empty name".
- **P-9** Determinism: cfg sections iterate in `BTreeMap` (lexicographic)
  order — sibling entry order, matrix deep-merge order, and error `expr`
  labels depend on it. Target plan order is workspace-list order then
  package order (S-10), not alphabetical.
- **P-10** `resolve` runs per (package × target); the workspace target
  sections are matched once per target (stored on `TargetPlan`), package
  target sections once per package-target. Do not re-run cfg evaluation for
  unplanned targets (test
  `unused_workspace_target_does_not_evaluate_overrides` uses a panicking
  evaluator).
- **P-11** Empty selection must not touch the environment or the evaluator
  (`empty_selection_skips_target_resolution` uses panicking stubs).
- **P-12** `Vec<(&str, ScopeView)>` labels: use `""` for non-target scopes
  and the cfg expression for target scopes; error messages interpolate them
  directly.
- **P-13** House style: no banner comments; comments explain *why*; never
  delete existing accurate comments — several current comments (e.g. on
  `merge_matrix`, the fallback attribution, the alias-wrapper placement)
  must survive the move into the new files. Prefer
  `#[expect(lint, reason = "…")]` over `#[allow]`.
- **P-14** `dedup` is a serde alias of `dedupe` only; `FLAG_KEYS` includes
  the alias for validation.
- **P-15** Keep `StringSetPatch::apply_to` (single-patch fast path) — the
  engine's per-layer fold may use `SetPatchOps` uniformly instead, in which
  case delete `apply_to` and its comment; do not keep both if only one is
  used.

## 9. Test migration map

Unit tests (move + retarget at the engine; scenario must be preserved):

| current test (file::name) | disposition |
|---|---|
| resolve.rs: `additive_exclude_features`, `override_exclude_features_array_syntax`, `remove_exclude_features`, `multiple_matching_sections_combine_adds`, `add_wins_over_remove_for_same_value`, `boolean_override_no_empty_feature_set`, `feature_set_vec_patch_*`, `matrix_metadata_merge_adds_new_key`, `allow_feature_sets_singleton_conflict`, `conflicting_override_errors`, `no_match_returns_base_unchanged`, `replace_starts_from_default`, `replace_disallows_add_remove` | M2 engine tests (features via `Chain::full` with empty workspace) |
| resolve.rs: `package_subcommand_feature_override_*`, `target_subcommand_feature_override_applies`, `feature_layer_precedence_target_beats_subcommand`, `replace_resets_base_subcommand_layer_but_keeps_target_subcommand`, `replace_at_package_subcommand_resets_base_features`, `replace_at_target_subcommand_resets_all_broader_layers`, `replace_at_subcommand_disallows_add_remove` | M2 engine tests |
| resolve.rs: `boolean_override_prune_implied`, `boolean_override_diagnostics_config`, `target_flag_layers_resolve_after_package_subcommand_layers`, `conflicting_target_subcommand_flags_error`, `target_override_prune_spelling_conflict_errors` | M2 engine tests (assert on `Resolved.flags`) |
| flags.rs: all `resolve_flags` / gate / dedupe-interplay / section+subcommand replace / `resolve_command_config` / driver chain tests | M2 engine tests (`Chain` + `CliOverlay`); `flag_subset_macros_match_documented_special_cases` stays in flags.rs |
| flags.rs: `combine_driver_trims_whitespace_and_rejects_empty` | trim/conflict half → engine sibling-combine test; empty half → M1 validation test (B-5) |
| targets.rs: all planning tests | stay; only construction of `TargetPlan` in `execution.rs` tests changes (`ws_matched` instead of five fields) |
| targets.rs: `workspace_target_subcommand_exclude_and_replace_apply`, `subcommand_exclude_packages_*`, `base_exclude_applies_to_all_targets`, `workspace_target_override_excludes_only_matching_targets` | retarget at `Chain::workspace(...).exclude_packages(...)` through `build_execution_plans` (same assertions) |
| schema.rs serde-splitting tests | M4: retarget at `ScopeConfig` |
| validate.rs tests | M1 per §5 |

Expectation updates required by `B-*`:

- B-1: extend `replace_disallows_add_remove`-style tests to `exclude_packages`
  and `targets`.
- B-2: new test — package `replace = true` with a workspace `targets` list
  resolves to the fallback target.
- B-3: delete the multiple-sibling-replace error assertion; add a test that
  two matching resetting sections combine (with agreeing overrides).
- B-4 (M4): `package_targets_add_patch_extends_workspace_list` and
  `duplicate_targets_deduped_preserving_order` — adds/overrides now in
  declaration order.
- B-6: new test — command-layer `replace` resets a broader
  `expand_targets = false` back to the default.
- P-7: new test — package `replace = true` does not clear workspace
  `exclude_packages`.

Integration tests (`tests/`): must pass unchanged, except
`tests/target_overrides.rs` (uses the public `resolve_config` and
`Package::feature_combinations`/`feature_matrix`; it asserts only feature
fields and the replace=true error, so it compiles against `ResolvedFeatures`
with import changes only) and `tests/metadata_key_aliases.rs` (M4:
`config_for_toml` returns `RootConfig`). No integration test constructs
`TargetPlan` directly; only the unit tests in `plan/execution.rs` do.

## 10. Expected outcome

- `resolve.rs` ~1460 → ~450 lines; `flags.rs` ~1260 → ~450; `validate.rs`
  ~550 → ~300; `targets.rs` loses ~350 lines of config plumbing; five schema
  types → three; net ≈ −1000 lines.
- Adding a future chain-resolved setting = one `ScopeConfig` field + one
  `SettingKind` row (+ its fold call if it needs a dedicated output), instead
  of ~seven touch points.
- One `replace` implementation, one sibling-combine implementation, one
  place that knows the chain order, and a validity matrix that can be
  asserted against the README.

## 11. Follow-up work (post-implementation review)

The redesign is implemented. A review of the result surfaced three findings;
this section is the normative spec for addressing them. Same ground rules as
the main plan: `task test` and `task lint` green after each item, CHANGELOG
entries for user-visible changes, no new façade layers.

### F-1 (medium): make the add/remove-under-replace rule section-local, at validation time

**Finding.** The implementation validates the rule per *layer*, not per
*section*: `replace_start()` (`src/config/resolve.rs:157`) locates the
narrowest layer containing any `replace = true` and calls
`validate_reset_layer()` (`src/config/resolve.rs:170`), which checks **every
sibling entry of that one layer**. Two defects follow:

1. *Missed errors*: a broader section with `replace = true` + `add`/`remove`
   is never validated when a narrower reset exists (the broader layer is
   sliced away before the check), and never validated at all when its cfg
   expression doesn't match the current target/command — so whether the
   config errors depends on what you build.
2. *False positives*: a sibling cfg section in the resetting layer that uses
   `add`/`remove` is rejected even though it did **not** set
   `replace = true` itself — and that same section may be perfectly
   meaningful for other targets where no reset happens.

The CHANGELOG (B-1 entry, CHANGELOG.md ~line 29) already states the intended
rule: "`replace = true` combined with `add` or `remove` **in the same
section**". The root cause is the main plan's S-3, which said "validate all
sibling entries of the resetting layer" — that sentence encoded the old
resolution-time behavior instead of the rule B-1 announced. The
implementation followed the spec; the spec was wrong.

**Decision.** Adopt the section-local rule, enforced in `validate.rs` at
load time (not the "same combined layer" alternative — that rule is harder
to state to a user, keeps the false-positive sibling rejection, and keeps
error surfacing dependent on which cfg happens to match). The rule is purely
syntactic — one TOML table, no chain context needed — which is exactly what
the raw-JSON validation walk is for.

**Normative rule.** A section (one TOML table = one scope) that sets
`replace = true` may not use `add`/`remove` patch operations in any of its
own patch-typed keys (`targets`, `exclude_packages`, and the feature-set
keys). This constrains only that table's own keys: sibling cfg sections,
broader/narrower sections, and nested `subcommands` tables (which are their
own sections with their own `replace`) are unaffected. An `add`/`remove` in
a *non-replacing* section that shares a layer with a replacing sibling is
legal and applies onto the reset base (add onto defaults).

**Implementation steps.**

1. `validate.rs`: in the per-scope walk (which already visits every section
   with its `ScopeId`), when the raw table has `"replace": true`, collect
   every patch-typed key in the same table whose value is an object with a
   non-empty `add` or `remove` array, and error listing the section path and
   offending fields. Keep the substrings existing tests match
   (`add/remove`, `replace`); a good shape:
   `` `{field, …}` use add/remove patch operations in [{section}] with replace = true ``.
   Check the raw JSON only — the deprecated feature spellings fold into
   `add` *after* validation and must not retro-trigger this rule.
2. `resolve.rs`: delete `validate_reset_layer` and
   `collect_invalid_feature_patches`; `replace_start` reduces to the
   `rposition` computation and becomes infallible (`fn(&[Layer]) -> usize`),
   simplifying its three call sites (`resolve`, `exclude_packages`,
   `targets_list`).
3. Tests:
   - Move the engine tests asserting the error
     (`replace_disallows_add_remove`-family) to `validate.rs`, asserting at
     `validate_package_metadata` / `validate_workspace_metadata` level.
   - New: a broader section with `replace = true` + `add` errors even when a
     narrower section also sets `replace = true`, and even when the broader
     section's cfg would not match any planned target (deterministic,
     load-time).
   - New: a non-replacing sibling cfg with `add` alongside a replacing
     sibling passes validation, and resolution applies the add onto the
     reset base (assert the resolved outcome).
   - New: `replace = true` on a section does not constrain patch ops inside
     its nested `subcommands` tables (and vice versa).
4. CHANGELOG: note the strictness increase — configs whose invalid
   replacing section was previously shadowed (by a narrower reset or a
   non-matching cfg) now error at load. This is the B-1 rule actually
   enforced as worded.

### F-2 (low): document the deprecated package-base `exclude_packages` carve-out

**Finding.** The README matrix (README.md ~line 185) shows `—` for
`exclude_packages` in all four package columns, but `validate.rs` accepts it
at `PackageBase` (the deprecated root-package compatibility spelling, folded
into the workspace base exclude set). The plan's own §5 matrix documents
"`PkgBase` (deprecated)"; the README should say the same.

**Steps.** Extend README footnote 2: the bare `pkg` scope accepts
`exclude_packages` only as a **deprecated** root-package spelling kept for
backwards compatibility — it is folded into the workspace base set (with a
deprecation warning) and is rejected in `pkg·target`, `pkg·sub`, and
`pkg·tgt·sub`. Optionally mark the `pkg` cell `—*`. The key stays accepted
(no removal in this cycle); if it is ever removed, that is a separate
breaking change. Docs only — no code.

### F-3 (architecture): shrink and de-stabilize the public surface — no façade

**Finding.** The raw nested schema (`ScopeConfig`/`SectionConfig`/root
config) is re-exported as public API, partly at the crate root
(`src/lib.rs:40-57`). The reviewer suggests separating raw config internals
from a stable public resolved view.

**Decision.** Agree with the concern, reject the façade. This crate is a
CLI; the Rust API exists for its own two binaries and its integration-test
suite, and nothing else consumes it. Building a stable public view (wrapper
types, conversion layers, semver guarantees) would reintroduce exactly the
kind of parallel-shape plumbing this redesign deleted. The resolved view
already exists — `ResolvedFeatures` / `ResolvedFlags` — and needs no
duplicate. Instead, make the API's status explicit and keep the root
namespace small:

1. `lib.rs`: remove crate-root re-exports of raw schema and patch types
   (`config::patch::*`, the schema types, and any other internals not needed
   by the binaries). Keep them `pub` **under their module paths** — the
   integration tests in `tests/` compile against the public API and already
   import module paths for most items. Retain a minimal root: `run`,
   `Package`, `resolve_config`, `ResolvedFeatures`, `TargetTriple`,
   `CfgEvaluator` (what tests use most and what a curious user would reach
   for first). Update test imports mechanically.
2. Crate-level docs (`lib.rs` `//!` header) and a one-line README note: the
   Rust API is an implementation detail of the `cargo-fc` CLI with no
   stability guarantees; the CLI is the interface. This removes the pressure
   to design the schema types as if they were a contract.
3. Explicitly out of scope: `#[doc(hidden)]` sprinkling, a separate
   `api`/`facade` module, sealed wrappers, or making the schema types
   `pub(crate)` (the external `tests/` directory needs them `pub`).

**Acceptance for §11 overall.** `task test` and `task lint` green; the three
new F-1 tests present; README footnote updated; crate root exports reduced
with tests compiling against module paths; CHANGELOG entries for F-1
(strictness) and F-3 (API surface note).
