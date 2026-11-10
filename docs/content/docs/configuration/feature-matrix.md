---
title: Shaping the feature matrix
weight: 2
---

# Shaping the feature matrix

By default the matrix is the powerset of a package's features. These keys, all under `[package.metadata.cargo-fc]`, change what gets generated.

Configuration is validated **strictly**: naming a feature the package does not
declare — in any key, any scope (including `target.'cfg(...)'` sections that
don't match the current host), and any patch operation — fails before anything
runs. A typo can therefore never silently shrink or grow the matrix, and stale
entries surface immediately when features are renamed or removed. This matches
Cargo's own strictness for `--features`, which rejects even `default` when the
package declares no such feature.

## At a glance

| Key | Effect |
|---|---|
| [`exclude_features`](#exclude_features) | Remove features from the varied set. |
| [`only_features`](#only_features) | Restrict the varied set to an allowlist. |
| [`include_features`](#include_features) | Add features to *every* generated combination. |
| [`mutually_exclusive_features`](#mutually_exclusive_features) | Permit at most one feature from each group. |
| [`required_feature_groups`](#required_feature_groups) | Require at least one feature from each group. |
| [`exclude_feature_sets`](#exclude_feature_sets) | Drop specific combinations (e.g. incompatible pairs). |
| [`include_feature_sets`](#include_feature_sets) | Always add specific exact combinations. |
| [`allow_feature_sets`](#allow_feature_sets) | Replace the powerset with an exact list of sets. |
| [`isolated_feature_sets`](#isolated_feature_sets) | Build independent sub-matrices and merge them. |
| [`skip_optional_dependencies`](#skip_optional_dependencies) | Ignore implicit optional-dependency features. |
| [`no_empty_feature_set`](#no_empty_feature_set) | Never include the empty (no-features) combination. |
| [`max_combinations`](#max_combinations) | Raise the safety limit on generated combinations. |
| [`matrix`](#matrix) | Attach custom metadata to `cargo fc matrix` rows. |

## `exclude_features`

Features listed here are not varied in the matrix.

```toml
exclude_features = ["full"]
```

An umbrella such as `full` may be redundant when its component features already vary independently.
Do not exclude `default` when the matrix must cover ordinary Cargo defaults: cargo-fc disables
defaults for every invocation, so `[]` enables no features and `["default"]` enables the declared
default feature set.

## `only_features`

Restrict the combinatorial matrix to an allowlist. When set, features not listed are ignored. When empty, all features are considered.

```toml
only_features = ["core", "cli"]
```

## `include_features`

Add features to **every** generated combination. This does not restrict which features are varied — it pins features that must always be on. To restrict the varied set, use `only_features`.

```toml
include_features = ["feature-that-must-always-be-set"]
```

## `mutually_exclusive_features`

Declare groups of alternatives where each generated combination may contain at
most one member. The no-member choice is included, and features outside the
groups keep their full powerset:

```toml
mutually_exclusive_features = [
  ["cuda", "coreml", "webgpu"],
]
```

With two other features, this produces `2² × (3 + 1) = 16` base combinations:
each independent-feature subset crossed with no backend, CUDA, Core ML, or
WebGPU. A newly added feature remains visible automatically; a newly added
backend also varies freely until it is deliberately added to the group.

Multiple groups are allowed but must be disjoint. `include_feature_sets` can
still add an exact combination containing
multiple group members, while a non-empty `allow_feature_sets` remains the
complete matrix and ignores the groups.

If `include_features` pins a group member, that member becomes the only group
choice — pinned features are added to every combination, so this holds even
when `exclude_features` or `only_features` removes the member from the varied
universe. Pinning two members of the same group is an error. Universe filters
can remove members from the varied choices but never add them.

Like the feature-set keys, this setting is patchable per target and command.
Patch operations add or remove whole groups:

```toml
[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
mutually_exclusive_features = { add = [["openssl", "rustls"]] }
```

## `required_feature_groups`

Declare groups of which every generated combination must enable at least one
member. A combination that enables none of a group's features is dropped, and
features outside the groups keep their full powerset, so a newly declared
feature still enters the matrix automatically.

A combination meets a group through the features it implies, too. Here a crate
needs exactly one TLS backend, and `rustls-webpki-roots` and `default` both
enable `rustls`:

```toml
[features]
default = ["rustls"]
native-tls = []
rustls = []
rustls-webpki-roots = ["rustls"]
logging = []

[package.metadata.cargo-fc]
# Every combination selects at least one TLS backend...
required_feature_groups = [["native-tls", "rustls"]]
# ...and at most one.
mutually_exclusive_features = [["native-tls", "rustls"]]
# `default` and `rustls-webpki-roots` imply `rustls`.
exclude_feature_sets = [
  ["default", "native-tls"],
  ["native-tls", "rustls-webpki-roots"],
]
```

Together these keys make every combination enable exactly one backend. `[]`
and `[logging]` are dropped, while `[rustls-webpki-roots]`, `[default]`, and
`[logging, native-tls]` stay.

The `exclude_feature_sets` entries are needed because
`mutually_exclusive_features` only looks at the features a combination names,
not at the features they imply. Without them, `[native-tls, rustls-webpki-roots]`
would stay and enable both backends. Add an entry whenever a new feature
implies a group member.

With several groups, a combination must meet every one of them. Like the
exclusion rules, the groups do not filter `include_feature_sets`, and a
non-empty `allow_feature_sets` remains the complete matrix and ignores them.

An empty group is a configuration error, because no combination can meet it.
It is also an error when the groups drop every generated combination and
nothing else, such as `include_feature_sets`, keeps the matrix non-empty.
That usually means `exclude_features` or `only_features` removes every
feature that reaches a member, and an empty matrix would otherwise look like
a passing run.

The safety limit from [`max_combinations`](#max_combinations) counts
combinations before the groups drop any.

Like the feature-set keys, this setting is patchable per target and command.
Patch operations add or remove whole groups:

```toml
[package.metadata.cargo-fc.target.'cfg(target_os = "linux")']
required_feature_groups = { add = [["openssl", "rustls"]] }
```

## `exclude_feature_sets`

Drop groupings of features that are incompatible or don't make sense together. Any generated combination that is a superset of a listed set is removed.

```toml
# `native-tls` and `rustls` are two TLS backends — enabling both makes no sense.
exclude_feature_sets = [["native-tls", "rustls"]]
```

To exclude only the empty feature set, list it explicitly (or use [`no_empty_feature_set`](#no_empty_feature_set)):

```toml
exclude_feature_sets = [[]]
```

## `include_feature_sets`

Always add these exact combinations to the final matrix, unless one is already present. Other configuration is not applied to these sets.

```toml
# The exact stack you ship — always kept in the matrix, even if other rules
# (or pruning) would otherwise drop it.
include_feature_sets = [
  ["postgres", "rustls", "runtime-tokio"],
]
```

## `allow_feature_sets`

When non-empty, the matrix becomes **exactly** the listed sets — no powerset is generated.

```toml
allow_feature_sets = [
  ["hydrate"],
  ["ssr"],
]
```

This is the most direct way to say "only ever test these specific configurations."

## `isolated_feature_sets`

For a crate whose features fall into **independent groups** that never interact. Instead of one powerset over all features, `cargo fc` builds a sub-matrix per group and merges them (a combination appearing in more than one group is kept once).

For example, a serialization crate that supports several **formats** and, independently, several **compression** codecs. The two axes are orthogonal — the format code doesn't care which codec is enabled — so cross-testing every format subset against every codec subset adds no coverage:

```toml
isolated_feature_sets = [
  ["json", "yaml", "msgpack"],   # formats
  ["gzip", "zstd", "brotli"],    # compression codecs
]
```

The full powerset here is 2⁶ = 64 combinations; the isolated sets reduce it to 2³ + 2³ − 1 = 15 (the shared empty set is merged). Other configuration options are still respected. See [Large feature sets]({{< relref "../recipes/large-feature-sets.md" >}}).

## `skip_optional_dependencies`

Cargo generates an implicit feature for each optional dependency (e.g. an optional `serde` dependency yields an implicit `serde = ["dep:serde"]` feature). Enabling this removes those implicit features from the matrix, mirroring the flag of the same name in `cargo-all-features`.

```toml
skip_optional_dependencies = true
```

Useful when optional dependencies would otherwise blow up the matrix. See [Optional dependencies]({{< relref "../recipes/optional-dependencies.md" >}}).

## `no_empty_feature_set`

Never include the empty combination (no `--features`), even if it would otherwise be generated.

```toml
no_empty_feature_set = true
```

## `max_combinations`

`cargo fc` fails if it would generate more than a safety limit (default `100000`) of combinations. Raise it when you legitimately need more:

```toml
max_combinations = 250000
```

Mutually exclusive groups are counted after applying their constraint. For
`u` unconstrained features and groups of effective sizes `n₁, n₂, …`, the
global base count is `2ᵘ × (n₁ + 1) × (n₂ + 1) × …`.

Required feature groups are not: the limit counts combinations before
[`required_feature_groups`](#required_feature_groups) drops any.

## `matrix`

Attach custom metadata to every row a package emits in `cargo fc matrix`. See [The matrix subcommand]({{< relref "../commands/matrix.md" >}}).

```toml
matrix = { kind = "ci" }
```

```toml
[package.metadata.cargo-fc.matrix]
requires-gpu = false
```

## Automatic pruning

Some features imply others. When a `full` feature enables both `json` and `yaml`, a combination like `{full, json}` resolves — after Cargo's feature unification — to the same effective set as `{full}` alone. `cargo fc` drops the redundant combination automatically, so it isn't checked twice.

Enabling a feature of an optional dependency counts too: Cargo turns on the dependency's own feature with it, so with `tls = ["foo/bar"]`, `{foo, tls}` is equivalent to `{tls}`. The weak form `foo?/bar` enables nothing on its own. A dependency declared only under `[target.'cfg(...)'.dependencies]` is left out, because Cargo turns on its feature only for the targets where the dependency applies.

{{< cargofile "pruning/crates/app" >}}

Pruning is **on by default and needs no configuration** — whatever your features, the smaller equivalent combination is kept and the redundant supersets are dropped. `--show-pruned` reveals what was dropped and why (the `SKIP` rows are each equivalent to `[full]`):

{{< terminal name="pruned" >}}

You should not need to change this, but you can turn it off to check every generated combination regardless of unification — with `--prune-implied=false` for one run, or in config:

```toml
[workspace.metadata.cargo-fc]
prune_implied = false
```

See also [`--show-pruned` and `--prune-implied`]({{< relref "../commands/output-modes.md#pruning---show-pruned-and---prune-implied" >}}).

---

All of these keys can be refined per target and per command using the same patch syntax. That's the [override model]({{< relref "override-model.md" >}}).
