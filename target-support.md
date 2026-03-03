# Target-Specific Feature Matrix Support

## Summary

This document specifies a target-aware configuration extension for `cargo-feature-combinations` that:

- Keeps the *default* configuration format unchanged.
- Adds a Cargo-familiar `[...target.'cfg(...)']` section for per-target overrides.
- Uses a single, explicit patch model (`override` / `add` / `remove`) with **no ambiguous shorthand**.
- Supports an optional `replace = true` to start from a fresh configuration on a given target.
- Is implemented via small, modular components (target detection, cfg evaluation, config resolution/merge).

The goal is a configuration surface that is:

- **Simple** for common cases (exclude one feature on one OS).
- **Powerful** for advanced cases (cross-compile, layered cfg predicates, full reset).
- **Testable** in isolation.


## Goals

- Allow excluding/including/allowlisting features differently per target (e.g. `cuda` vs `metal`).
- Resolve target-specific configuration based on:
  - `--target <triple>` if provided
  - otherwise the host target
- Support Cargo-style `cfg(...)` predicates (not only `target_os`).
- Avoid multiple equivalent syntaxes in documentation.


## Non-goals (initial scope)

- Workspace-wide target overrides (we only extend per-package metadata initially).
- Supporting Cargo’s `[target.'cfg(...)'.package.metadata.*]` layout (see rationale below).
- Implementing a full Cargo manifest parser (we rely on `cargo metadata`).


## User-Facing TOML Design

### Base configuration (unchanged)

The existing configuration remains under:

```toml
[package.metadata.cargo-feature-combinations]
# existing keys...
```


### Target-specific overrides (new)

Target-specific overrides live under the same metadata key and mirror Cargo’s `cfg(...)` selectors:

```toml
[package.metadata.cargo-feature-combinations]
exclude_features = ["default"]

[package.metadata.cargo-feature-combinations.target.'cfg(target_os = "linux")']
exclude_features = { add = ["metal"] }

[package.metadata.cargo-feature-combinations.target.'cfg(target_os = "macos")']
exclude_features = { add = ["cuda"] }
```

Notes:

- The quoted key must be exactly the `cfg(...)` expression string.
- Multiple target sections may match simultaneously (e.g. `cfg(unix)` and `cfg(target_os="linux")`).
- Conflict rules are defined below to keep behavior deterministic.


## Patch Model (minimal + explicit)

### Why a patch model?

Without a patch model, target overrides would either:

- always fully replace the base value (forcing duplication), or
- be ambiguous whether they add or replace.

We therefore support an explicit patch table.


### Patch operations

Collection-like fields (e.g. `exclude_features`, `include_features`, `only_features`) may be specified as either:

1) A plain value (array / list) meaning **override** (replace the entire value)

```toml
exclude_features = ["cuda"]
```

2) A patch object with any combination of:

- `override = [...]` (replace)
- `add = [...]` (union)
- `remove = [...]` (subtract)

```toml
exclude_features = { add = ["cuda"], remove = ["metal"] }
```

There is **no** `clear` flag. Clearing is achieved via `override = []`.


### Important: arrays are always overrides

To avoid ambiguity, a plain array in a target override section always means **override**, never additive.

If you want “additive”, you must write `{ add = [...] }`.

This is intentionally different from my earlier draft idea where arrays defaulted to `add`.


### Why you would ever write `override = [], add = ["metal"]`

You usually shouldn’t.

The simplest expression of “set this to exactly one value” is:

```toml
exclude_features = { override = ["metal"] }
```

The combined form:

```toml
exclude_features = { override = [], add = ["metal"] }
```

is only useful to illustrate/verify the semantic order (override → remove → add) or when constructing patches programmatically (not typical in TOML). In docs, prefer the simpler `override = ["metal"]`.


### Patch support by key

Initial patch support should cover:

- `exclude_features`: set-patch
- `include_features`: set-patch
- `only_features`: set-patch
- `exclude_feature_sets`: list-patch (list of feature-sets)
- `include_feature_sets`: list-patch
- `isolated_feature_sets`: list-patch
- `allow_feature_sets`: list-patch (but see conflict rules)

Booleans remain plain booleans:

- `skip_optional_dependencies`
- `no_empty_feature_set`


## Target override reset: `replace = true`

### Problem this solves

Sometimes the base config is complex, and for a target you want a *fresh* config that does **not** inherit any base keys you didn’t explicitly mention.

Example: base config uses `isolated_feature_sets`, but on one target you want to ignore that entirely and use a simple global matrix.

Without `replace`, you would have to explicitly reset multiple fields (often to empty), which is tedious and easy to miss.


### Design

Allow an optional boolean in a target override table:

```toml
[package.metadata.cargo-feature-combinations.target.'cfg(target_os = "macos")']
replace = true
exclude_features = ["default", "cuda"]
skip_optional_dependencies = true
```

### Validation when `replace = true`

When `replace = true` is enabled, the override is intended to describe a *fresh* configuration
without inheriting the base configuration.

To avoid confusion, the implementation enforces that patchable fields in a `replace = true`
override **must not** use `add` or `remove` operations.

- `exclude_features = ["cuda"]` is allowed (array syntax = override).
- `exclude_features = { override = ["cuda"] }` is allowed.
- `exclude_features = { add = ["cuda"] }` is an error.
- `exclude_features = { remove = ["cuda"] }` is an error.

Semantics:

- If **no** matching target override has `replace = true`, resolution starts from the manifest base config.
- If **exactly one** matching override has `replace = true`, resolution starts from a fresh `Config::default()` (i.e. empty config), and then applies all matching overrides.
- If **more than one** matching override has `replace = true`, resolution is an error.

This makes `replace` orthogonal: it changes the *starting point*.


## Matrix metadata (`config.matrix`) in target overrides

`cargo fc matrix` outputs a JSON array. Today, each item includes the computed package name and feature string, and also merges `config.matrix` into the output.

We keep this, but:

- We must document `matrix` in the README (base usage).
- We support adding/overriding matrix metadata per target.

### Supported syntax (support both, document one)

Support both:

1) Inline table assignment (recommended to document):

```toml
[package.metadata.cargo-feature-combinations.target.'cfg(target_os = "linux")']
matrix = { gpu = "cuda" }
```

2) Nested table form (supported but not documented as primary):

```toml
[package.metadata.cargo-feature-combinations.target.'cfg(target_os = "linux")'.matrix]
gpu = "cuda"
```

### Merge semantics

For `matrix` only, we use a **deep merge** (consistent with how `print_feature_matrix` merges into output today):

- Base `matrix` is the starting value.
- Target override `matrix` is merged in; overlapping keys overwrite base.

This avoids forcing users to duplicate base matrix metadata in each target.


## Why we do NOT use Cargo’s `[target.'cfg(...)'.package.metadata.*]` layout

Cargo’s manifest format has special support for sections like:

```toml
[target.'cfg(...)'.dependencies]
```

However, `cargo-feature-combinations` currently reads configuration from `cargo_metadata::Package::metadata`, which corresponds to `[package.metadata]` only.

Placing our configuration under:

```toml
[target.'cfg(...)'.package.metadata.cargo-feature-combinations]
```

would require one of:

- a custom Cargo.toml parser (more complex, error-prone, and a different data source than `cargo metadata`), or
- relying on Cargo exposing that data through `cargo metadata` in a stable way (not currently how we obtain metadata).

Therefore, we keep all configuration inside `[package.metadata.cargo-feature-combinations]` and add `.target` beneath it.


## Target resolution rules

We resolve overrides against the **effective build target**:

- If the user passes `--target <triple>` to `cargo fc ...`, that triple is used.
- Otherwise, we use the host triple.

This matches user expectations for cross-compiling.

`cargo fc matrix` should also honor `--target` for configuration selection.


## `cfg(...)` evaluation (Cargo-like)

### Recommended evaluator

To be accurate and future-proof (beyond `target_os`), evaluate `cfg(...)` expressions using:

- `rustc --print cfg --target <triple>` to obtain the set of active cfg values
- a `cfg` expression parser/evaluator crate (e.g. `cfg-expr`) to evaluate the `cfg(...)` string

This supports:

- `cfg(target_os = "linux")`
- `cfg(unix)`
- `cfg(any(target_os = "linux", target_os = "android"))`
- `cfg(all(target_arch = "aarch64", target_os = "macos"))`


## Deterministic merge / conflict rules

Because multiple target sections may match, we need deterministic behavior.

### Matching sections

Let `M` be the set of all target override sections whose `cfg(...)` matches.

- If any section in `M` has `replace = true`:
  - require exactly one such section
  - use `Config::default()` as the starting base
- Otherwise, start from the manifest base config.

### Field application (order-independent)

For patchable fields:

- Combine all `add` values across `M` (union)
- Combine all `remove` values across `M` (union)
- For `override`:
  - if 0 overrides specify it: inherit base
  - if 1 override specifies it: use it
  - if >1 overrides specify it: error (unless identical; optional relaxation)

Then apply:

1) start value = base (or override value if present)
2) apply `remove`
3) apply `add`

For booleans and singleton fields:

- if multiple matching overrides set conflicting values: error
- else inherit base or take the single provided value

Special note for `allow_feature_sets`:

- `allow_feature_sets` changes matrix generation mode.
- If multiple matching overrides set it (non-empty), treat as a conflict error.


## Modular architecture (traits + files)

The implementation should be split into components to keep complexity contained and testable.

### Suggested module layout

- `src/target.rs`
  - `TargetTriple` / `TargetInfo`
  - `TargetDetector` trait
  - `RustcTargetDetector` implementation

- `src/cfg_eval.rs`
  - `CfgExpr` wrapper
  - `CfgEvaluator` trait
  - `RustcCfgEvaluator` implementation (backed by `rustc --print cfg` + `cfg-expr`)

- `src/config.rs` (existing)
  - keep the effective `Config` (used by feature generation)
  - add a `target` field that deserializes override sections into a new type (e.g. `TargetOverrides`)

- `src/config/resolve.rs` (new)
  - `ConfigResolver` trait
  - `DefaultConfigResolver` implementation
  - `resolve_config(base: &Config, overrides: &TargetOverrides, target: &TargetInfo) -> Result<Config>`

- `src/config/patch.rs` (new)
  - patch types (e.g. `SetPatch<T>`, `ListPatch<T>`)
  - combination / conflict detection

### Why traits?

- `TargetDetector` can be mocked in tests.
- `CfgEvaluator` can be mocked for unit tests (no `rustc` invocations required).
- Resolver and merger are pure logic and easy to test exhaustively.


## Integration points

### `run_cargo_command`

- Detect target once per invocation using `TargetDetector`.
- For each package:
  - load manifest config via `package.config()?` (as today)
  - resolve to an effective config via `ConfigResolver`
  - generate combinations with `package.feature_combinations(&effective_config)`

### `print_feature_matrix`

- Same as above: resolve effective config before calling `feature_matrix`.
- Ensure `--target` affects resolution for `matrix` as well.


## Testing strategy

### Unit tests (fast)

- Patch combination logic:
  - `override/add/remove` ordering
  - conflict detection (`override` from multiple matches)
- Replace behavior:
  - base vs default starting point
- Cfg evaluation:
  - parse errors
  - evaluator stub that matches specific expressions
- Target detection:
  - extracts `--target`
  - falls back to host target

### Integration tests (end-to-end)

Using `assert_fs` (like existing tests):

- Create a dummy crate with `cuda` and `metal` features.
- Provide target sections with `cfg(target_os = ...)`.
- Use a test resolver with a stub evaluator that pretends the target is linux/macos.
- Assert the final generated matrix includes/excludes the correct features.


## Documentation updates

- README:
  - Document `matrix` metadata (base usage) in the Configuration section.
  - Add a new section “Target-specific configuration” with:
    - the Cargo-like table header syntax
    - a minimal example (CUDA vs Metal)
    - mention of `--target` vs host
    - mention of `replace = true` and when to use it
  - Document only the inline `matrix = { ... }` form.


## Implementation steps (concrete)

1) **Parsing**
   - Extend `Config` deserialization to include a `.target` map keyed by `cfg(...)` strings.
   - Implement patch types for override fields.

2) **Target detection**
   - Implement `TargetDetector`:
     - parse `--target` from cargo args
     - else detect host via `rustc -vV` (`host:` line)

3) **Cfg evaluation**
   - Implement `CfgEvaluator` using `rustc --print cfg --target <triple>` and `cfg-expr`.
   - Cache cfg-set per target for the invocation.

4) **Config resolution**
   - Collect matching override sections.
   - Compute whether `replace` applies.
   - Merge patches deterministically per rules above.

5) **Wire into runtime**
   - Apply resolver in `run_cargo_command` and `print_feature_matrix`.

6) **Tests**
   - Unit-test patch+merge logic.
   - Integration tests for target overrides.

7) **Docs**
   - Update README as described.
