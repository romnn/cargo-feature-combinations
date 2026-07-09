---
title: Restrict the matrix
weight: 3
---

# Restrict the matrix to specific configurations

**Scenario:** a web app can be built in three rendering modes — `hydrate`, `ssr`, `csr` — but you only ever ship `hydrate` and `ssr`. You don't want a powerset; you want exactly those two configurations.

{{< cargofile "docs/recipes/restrict-matrix" >}}

When `allow_feature_sets` is non-empty, the matrix is **exactly** those sets — no powerset, no empty set, and `csr` is never tested:

{{< terminal name="recipe-restrict" >}}

## Related knobs

`allow_feature_sets` replaces the powerset entirely. When you want to keep a powerset but shape it, reach for a different key:

| Want | Use |
|---|---|
| Only these exact sets, nothing else | `allow_feature_sets` |
| Powerset, but only over these features | `only_features` |
| Powerset, plus these exact sets | `include_feature_sets` |
| Powerset, minus these features | `exclude_features` |
| Powerset, minus these groupings | `exclude_feature_sets` |

For example, to keep the powerset but restrict it to a subset of features:

```toml
[package.metadata.cargo-fc]
only_features = ["hydrate", "ssr"]
```

See [Shaping the feature matrix]({{< relref "../configuration/feature-matrix.md" >}}).
