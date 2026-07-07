# Config Override Chain Redesign

## Purpose

The README now documents a crisp override model:

> Every setting resolves along **one precedence chain**, broadest to narrowest —
> workspace → package, and within each: base → `subcommands.<cmd>` →
> `target.'cfg(...)'` → `target.'cfg(...)'.subcommands.<cmd>`.

That is eight scopes plus the CLI, one uniform set of forms per setting
(scalar override, set patch, `replace`), and a matrix saying where each
setting is valid. The model is simple. The implementation is not: the chain
is hand-rolled independently for each setting family, the scope payloads are
shredded into parallel struct fields and re-threaded through three modules,
and the raw schema types double as resolved output. This plan describes a
redesign that implements the documented model *once* and derives everything
else from it.

## What the current code actually does

The same broad→narrow walk is written out at least six times, each with its
own shape, its own `replace` handling, and its own error wording:

1. `flags::resolve_flags` — walks a `[Scope; 4]` array (ws, ws·target, pkg,
   pkg·target) with the selected subcommand override interleaved per scope,
   resetting a merged `FlagConfig` on `replace`.
2. `flags::resolve_driver_chain` — the identical walk, re-implemented for the
   `driver` scalar.
3. `flags::resolve_target_capability` — the identical walk again, reduced to
   the `expand_targets` bool (no `replace` handling at all).
4. `resolve::resolve_config_with_flag_layers` — a *different* hand-rolled walk
   for the feature matrix: layers L1–L4 with a `feature_replace_layer()` index
   and `apply_from <= N` arithmetic, plus a carve-out ("Config/L1 `replace`
   concerns the flag chain, not features").
5. `targets::resolve_effective_exclude_packages` — a third hand-rolled walk
   (ws base → ws sub → ws target → ws target·sub) with `replace` implemented
   as `HashSet::clear()` inside a closure.
6. `targets::workspace_effective_targets` + `package_target_list` — a fourth
   walk for the `targets` list (ws base → ws sub → pkg base → pkg sub), which
   honors no `replace` at all.

Because each walk needs its own inputs, the scope payloads get shredded into
parallel plumbing:

- `PackageFlagLayers` — 8 fields (`package_flags`, `package_replace`,
  `package_driver`, `package_subcommands`, ×2 for the target layer).
- `ResolveCommandConfigArgs` — **18 fields**, four of which are
  default-empty at some call sites (`lib.rs::selected_packages_for_target_planning`
  passes four empty/false target-layer slots just to reuse the function).
- `TargetPlan` — five parallel `workspace_target_*` fields
  (`flags`, `replace`, `driver`, `exclude_ops`, `subcommands`) that are really
  one value: "the combined matching workspace target section".

Adding one new setting today means: a schema field in up to four structs, a
`combine_*` helper, a slot in `PackageFlagLayers`, a slot in
`ResolveCommandConfigArgs`, a slot in `TargetPlan`, threading through
`build_target_plans` and `build_execution_plans`, and a validator allowlist
entry — seven places for one setting. `driver` (commit 4b7de34 and this diff)
paid exactly that cost, which is why the diff is ~3700 lines.

Two more structural smells:

- **Half-merged `CommandCapabilities`.** Sibling `cfg(...)` sections that both
  match get pre-merged per subcommand (`combine_command_capability_maps` →
  `combine_command_capabilities`), but only *some* fields: flags, driver,
  `expand_targets`, `exclude_packages`. `features` and `targets` are
  deliberately NOT merged there and are read off the raw overrides by a
  different pass. Both `CommandCapabilities::merge` and
  `combine_command_capabilities` carry warning comments about this trap, and
  the latter constructs a `CommandCapabilities` with knowingly-default
  `features` — a value that lies about what the user wrote.
- **Raw schema as resolved output.** `resolve_config*` returns the schema type
  `Config` with `target_overrides` cleared, `package_targets` forced to
  `None`, and `deprecated` reset — three fields that exist only to be
  scrubbed. Consumers can't tell a raw config from a resolved one by type.

And the semantics have drifted between the parallel implementations:

- `replace = true` + `add`/`remove` in the same section is an **error** for
  feature-matrix fields (`validate_replace_feature_patches`) but silently
  allowed for `exclude_packages` (`resolve_effective_exclude_packages` clears
  then applies the add).
- Two sibling `cfg(...)` sections both setting `replace = true` is an
  **error** in the feature pass (`feature_replace_layer`) but a harmless
  `any()` in the flag pass.
- `replace = true` at the package base resets inherited workspace flags and
  driver, but the inherited workspace `targets` list is *not* reset (the
  targets walk never consults `replace`), contradicting the README's "discards
  everything broader".
- Empty-driver rejection is implemented twice: `combine_driver` (target
  scopes) and `normalize_driver` (base/subcommand/CLI scopes).

## The redesign, from first principles

The README's model has exactly four concepts. The code should have exactly
four mechanisms:

1. **One scope payload shape.** Every cell in the matrix — workspace base,
   package base, every `target.'cfg(...)'` section, every `subcommands.<cmd>`
   table — accepts the same settings bag. This is nearly true already:
   `CommandCapabilities` has `replace + expand_targets + exclude_packages +
   targets + driver + features + flags`, which is a superset of every other
   scope type. Make it literal:

   ```rust
   /// What one scope may say. Every field optional; absent = inherit.
   #[derive(Deserialize, ...)]
   pub struct ScopeConfig {
       pub replace: bool,
       pub driver: Option<String>,
       pub expand_targets: Option<bool>,
       pub targets: Option<TargetListPatch>,
       pub exclude_packages: Option<StringSetPatch>,
       #[serde(flatten)] pub features: FeatureMatrixPatch,
       #[serde(flatten)] pub flags: FlagConfig,
   }

   /// A scope plus its nested per-command tables.
   pub struct SectionConfig {
       #[serde(flatten)] pub settings: ScopeConfig,
       #[serde(default, rename = "subcommands")]
       pub subcommands: BTreeMap<String, ScopeConfig>,
   }

   /// A metadata root (workspace or package): base section + target sections.
   pub struct RootConfig {
       #[serde(flatten)] pub base: SectionConfig,
       #[serde(default, rename = "target")]
       pub targets: BTreeMap<String, SectionConfig>,
       #[serde(flatten)] pub(crate) deprecated: DeprecatedTomlKeys, // package roots
   }
   ```

   `Config`, `WorkspaceConfig`, `TargetOverride`, `WorkspaceTargetOverride`,
   and `CommandCapabilities` — five bespoke types — collapse into these three.
   "Workspace configs don't have feature keys" stops being a type distinction
   and becomes a validation rule, which is already the pattern used for
   `CommandCapabilities` (one shared type, scope-gated keys). Deserializing
   permissively and validating against the matrix is strictly simpler than
   maintaining five overlapping serde types, and the existing
   "flatten fields must stay disjoint" invariant shrinks to one struct + one
   test.

2. **One chain construction.** Given what is known at a call site, build the
   ordered list of applicable layers exactly once:

   ```rust
   /// Where a layer came from — for error messages, warnings, and provenance.
   #[derive(Clone, Copy)]
   pub(crate) enum ScopeId {
       WorkspaceBase, WorkspaceCommand, WorkspaceTarget, WorkspaceTargetCommand,
       PackageBase, PackageCommand, PackageTarget, PackageTargetCommand,
   }

   /// One precedence layer: its sibling sources (usually one; several when
   /// multiple cfg sections match one triple).
   pub(crate) struct Layer<'a> {
       scope: ScopeId,
       /// (source label for errors — cfg expr or "", payload)
       entries: Vec<(&'a str, &'a ScopeConfig)>,
   }

   pub(crate) struct Chain<'a> { layers: Vec<Layer<'a>> }
   ```

   Two constructors, replacing all six hand-rolled walks:

   - `Chain::base(ws, pkg, cmd)` — the four non-target layers. Used before a
     concrete target exists: pre-planning capability/`no_targets` resolution
     (today's degenerate `resolve_command_config` call in
     `selected_packages_for_target_planning`) and the `targets`-list
     resolution (target sections are excluded there by the matrix anyway —
     the circularity rule).
   - `Chain::full(ws, pkg, target, cmd, evaluator)` — all eight layers, with
     `matching_overrides` selecting the cfg sections. Used per
     (package × target × command) in `build_execution_plans`.

   Subcommand tables are ordinary layers (their `replace` is just the layer's
   `replace`), selected by the existing raw-token-first
   `selected_command_override` rule. The CLI stays an explicit final overlay
   (flags + `--driver`), not a pseudo-layer.

3. **One resolution engine over the chain.** `replace` is computed once:
   find the narrowest layer where any sibling sets `replace = true`, validate
   that resetting layers carry only plain overrides (uniformly, for *every*
   patch-typed setting), and slice — every setting then folds over
   `layers[start..]`:

   ```rust
   impl Chain<'_> {
       /// Scalars (driver, expand_targets, feature bools): siblings within a
       /// layer must agree (else error); the narrowest layer that sets the
       /// value wins. Returns the value and the scope that supplied it.
       fn scalar<T>(&self, name, get) -> Result<Option<(T, ScopeId)>>;

       /// Set-like fields: combine siblings per layer via the existing
       /// combine_set_patches (conflicting overrides error, adds/removes
       /// union), then apply layer results broad→narrow onto the base.
       fn set<P: SetPatchInput>(&self, name, get) -> Result<Option<SetPatchOps<P::Elem>>>;

       /// FlagConfig overlay with the existing dedupe/diagnostics/prune
       /// interactions and the diagnostics gate for non-command layers.
       fn flags(&self, cli, gate) -> Result<ResolvedFlagResult>;
   }
   ```

   The per-field quirks stay, but as leaf policies fed to the engine rather
   than as separate walks: the `allow_feature_sets` singleton rule, the
   `matrix` deep-merge, the diagnostics gate (applies to base/target layers,
   bypassed by command layers — expressible as `ScopeId::is_command()`), and
   the ordered-list semantics of `targets`.

   Provenance (`ScopeId` on every resolved value) replaces today's ad-hoc
   booleans: `targets_explicit` = `scalar(expand_targets)` returned a scope;
   `TargetSource::PackageConfig` vs `WorkspaceConfig` = which scope last
   touched the `targets` list; error messages get their "source kind" string
   from `ScopeId` instead of threaded `&str`s.

4. **The validity matrix lives only in validation.** Resolution never asks
   "is this setting allowed here" — absent settings simply don't contribute,
   and validation has already rejected present-but-misplaced ones. The
   README's table becomes data in one place:

   ```rust
   enum SettingKind { Flags, FeatureMatrix, ExcludePackages, TargetsList,
                      ExpandTargets, Driver, Replace }

   /// The README matrix: is `setting` valid at `scope`? Encodes exactly the
   /// four documented exceptions, each with its user-facing reason.
   fn valid_in(setting: SettingKind, scope: ScopeId) -> Result<(), &'static str>;
   ```

   The raw-JSON walk (`validate_keys` etc.) stays, but drives off this one
   function plus a `key → SettingKind` map, replacing seven allowlist consts,
   `misplaced_key_reason`, and the three allowlist-sync tests. Deprecated
   spellings (`denylist`, `skip_feature_sets`, `exact_combinations`, package
   `exclude_packages`) are a per-scope extra set, normalized into the
   `RootConfig` immediately after deserialization so nothing downstream sees
   them. Empty-driver rejection moves here too (one rule, every scope),
   deleting the `combine_driver`/`normalize_driver` split.

### Resolved output gets its own types

Resolution returns dedicated output types instead of a scrubbed `Config`:

```rust
/// Everything one (package × target × command) execution needs.
pub struct Resolved {
    pub flags: ResolvedFlags,
    pub driver: Option<String>,          // unnormalized; None = unset
    pub features: ResolvedFeatures,      // the sets/bools/matrix, nothing else
    pub ignored_diagnostics_config: bool,
}
```

`ResolvedFeatures` is today's `Config` minus `replace`, `package_targets`,
`subcommand_overrides`, `target_overrides`, `deprecated`, `flags`, `driver` —
i.e. exactly the fields `feature_combinations(&config)` and matrix output
read. `package.rs` and the pruning code switch to it mechanically.

### What the callers look like afterwards

- `TargetPlan` carries `target: TargetTriple`, `packages`, and the matched
  workspace target sections (`Vec<(String, &SectionConfig)>`) — one field
  where there were five. No pre-combining; siblings stay siblings until the
  engine folds them.
- `build_execution_plans` builds `Chain::full(...)` per planned package and
  calls `chain.resolve(cli_flags, cli_driver, gate)` — replacing the
  `resolve_config_with_flag_layers` + `PackageFlagLayers` +
  `resolve_command_config(18 fields)` + `resolve_effective_exclude_packages`
  four-step. `exclude_packages` is just another `chain.set(...)` call at the
  same site.
- `selected_packages_for_target_planning` builds `Chain::base(...)` and reads
  `flags.no_targets` / `scalar(expand_targets)` — no more fabricated empty
  target layers.
- `targets.rs` keeps everything that is genuinely target *planning* (fallback
  resolution, ordering, `show_target`, dedup by triple, source attribution)
  but gets its input list from `chain.base(...)` resolution of `targets`
  instead of `workspace_effective_targets` + `package_target_list`.

### `targets` as an order-preserving list patch

The unified schema needs one `targets` type for every scope, and order
matters (plans run in declared order). Today the workspace base is an ordered
`Vec<String>` while package `targets` is a set-based `StringSetPatch` whose
overrides/adds get *sorted*. Introduce `TargetListPatch` — same untagged
serde surface (plain array = override, `{override/add/remove}` object), but
`Vec<String>`-backed: overrides and adds keep declaration order, removes
filter, normalize dedups. This is one small type, serves all four scopes, and
declaration order is strictly more intuitive than today's sorted order.

## Behavior changes (all deliberate, all README-aligned)

The TOML surface is unchanged. Six edge behaviors change; each should get a
CHANGELOG line:

1. `replace = true` + `add`/`remove` in the same section becomes an error for
   `exclude_packages` and `targets`, matching the existing feature-matrix
   rule ("a reset has nothing to add to").
2. `replace = true` at the package base now also resets the inherited
   workspace `targets` list (README: "discards everything broader").
3. Two matching sibling cfg sections both setting `replace = true` stops
   being an error (feature pass today) and ORs, like the flag pass; sibling
   conflict detection still catches real disagreements.
4. Overridden/added `targets` run in declaration order instead of sorted.
5. `driver = ""` is rejected at validation time in every scope (today:
   resolution-time at some scopes, `combine`-time at others), so the error
   appears even when the scope doesn't match the current target.
6. Misplaced-key error wording changes slightly (now generated from the
   matrix); the per-key *reasons* are preserved.

Anything else that differs after the port is a bug: the three new integration
suites (`tests/per_subcommand.rs`, `tests/per_target.rs`, `tests/driver.rs`)
plus the existing target-override/prune/matrix suites pin the TOML-observable
behavior and are the safety net for the whole migration.

## Module layout

```
src/config/
  mod.rs       re-exports
  schema.rs    ScopeConfig, SectionConfig, RootConfig, DeprecatedTomlKeys (serde only)
  patch.rs     StringSetPatch, FeatureSetVecPatch, TargetListPatch, SetPatchOps (≈ as-is)
  flags.rs     FlagConfig field macros, ResolvedFlags, diagnostics/prune policies (shrinks ~⅔)
  scope.rs     ScopeId, Layer, Chain::{base,full}, cfg matching, command selection glue
  resolve.rs   the engine: replace slicing, scalar/set/flags folds, Resolved/ResolvedFeatures
  validate.rs  SettingKind, valid_in(), raw-JSON walk, deprecated-key normalization
```

Expected size: `resolve.rs` 1463 → ~450, `flags.rs` 1260 → ~450,
`validate.rs` 547 → ~300, `targets.rs` loses its ~350 lines of config
resolution, `execution.rs` and `lib.rs` shed the plumbing structs. Deleted
outright: `PackageFlagLayers`, `ResolveCommandConfigArgs`,
`ResolvedTargetConfig`, `WorkspaceTargetConfig`,
`CommandCapabilities::merge`, `combine_command_capability_maps`,
`combine_command_capabilities`, `combine_flag_configs`, `combine_driver`,
`resolve_driver_chain`, `resolve_target_capability`,
`feature_replace_layer`, `resolve_effective_exclude_packages`,
`workspace_effective_targets`. Net: roughly −1000 lines of src, and — the
real win — adding a future setting touches two places (schema field +
matrix row) instead of seven.

The library API is consumed only by this crate's binaries and tests, so
renaming `Config` → `RootConfig`/`ResolvedFeatures` and re-pointing the test
imports is acceptable; keep `pub use` aliases only where they cost nothing.

## Milestones

1. **Schema + validation.** Introduce `ScopeConfig`/`SectionConfig`/
   `RootConfig` and `TargetListPatch`; deserialize both metadata roots into
   them; port validation to the `SettingKind × ScopeId` matrix; normalize
   deprecated keys at load. Old resolution keeps running via temporary
   adapters from the new schema (or the old types kept alongside), so this
   lands green on the existing suites.
2. **Engine.** `scope.rs` + `resolve.rs`: chain constructors, replace
   slicing, scalar/set/flags folds with provenance, `Resolved` /
   `ResolvedFeatures`. Port the unit tests from `resolve.rs`/`flags.rs` —
   they compress substantially because one engine test covers what four
   parallel walks each needed tested.
3. **Callers.** Re-point `selected_packages_for_target_planning`, target
   planning's `targets`-list input, `TargetPlan`'s workspace-target payload,
   and `build_execution_plans` at the engine. Delete the old walks and
   plumbing structs. Integration suites must pass unmodified except for the
   six documented behavior changes.
4. **Docs + polish.** CHANGELOG entries for the behavior changes; README
   gains nothing (the model is already documented — that's the point);
   optionally assert the README table against `valid_in()` in a test so docs
   and code cannot drift.

## Open questions

- Should `replace` at a *workspace target* section also reset the workspace
  `targets` list for packages? Proposed: no — the `targets` list is resolved
  on the base chain (target sections can't touch it per the circularity
  rule), so only base/command-scope `replace` affects it. This matches the
  matrix.
- Keep `resolve_config(base, target, evaluator)` as a public convenience
  (base chain with no workspace, no command)? Proposed: yes, as a thin
  wrapper over `Chain::full` with empty workspace — it's what
  `tests/target_overrides.rs` uses.
