---
title: Large feature sets
weight: 4
---

# Large, independent feature sets

**Scenario:** a codec crate supports several serialization **formats** and, independently, several **compression** codecs. The two axes are orthogonal — the format code doesn't care which codec is enabled — so a full powerset over all six features is mostly redundant.

{{< cargofile "docs/recipes/large-feature-sets" >}}

Instead of one powerset over all six features, `cargo fc` builds a **sub-matrix per group** and merges them: the formats are varied among themselves, the codecs among themselves — but the two are never crossed. A combination appearing in more than one group is kept once.

This turns multiplicative growth into additive growth. The full powerset would be 2⁶ = 64 combinations; the isolated sets reduce it to 2³ + 2³ − 1 = 15 (the shared empty set is merged):

{{< terminal name="recipe-large" >}}

## When to raise the safety limit

`cargo fc` refuses to generate more than `max_combinations` (default `100000`). If you have a legitimately large but bounded matrix, raise it:

```toml
[package.metadata.cargo-fc]
max_combinations = 250000
```

If you hit the limit unexpectedly, that's usually a sign the matrix should be
shaped with `mutually_exclusive_features`, `isolated_feature_sets`,
`only_features`, or `skip_optional_dependencies` rather than simply raised.
Mutually exclusive groups retain cross-play with every independent feature and
are counted directly as `(group size + 1)` choices, so they are the right tool
when features are alternatives rather than independent sub-matrices.

See [`mutually_exclusive_features`]({{< relref "../configuration/feature-matrix.md#mutually_exclusive_features" >}})
and [`isolated_feature_sets`]({{< relref "../configuration/feature-matrix.md#isolated_feature_sets" >}}).
