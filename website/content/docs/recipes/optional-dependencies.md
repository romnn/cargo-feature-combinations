---
title: Optional dependencies
weight: 2
---

# Optional dependencies

**Scenario:** a `store` crate has two optional storage backends and one real `compression` feature. Cargo turns each optional dependency into an implicit feature (`redis-backend`, `sled-backend`), which would multiply the matrix — but you only want to vary `compression`.

{{< cargofile "docs/recipes/optional-dependencies/store" >}}

With `skip_optional_dependencies = true`, the implicit `redis-backend` and `sled-backend` features are removed from the matrix; only the real `compression` feature is varied. Checking just the `store` package shows two combinations:

{{< terminal name="recipe-optional" >}}

Without the flag, the same crate would vary all three features (`redis-backend`, `sled-backend`, `compression`) — 2³ = 8 combinations. (This applies to optional crates.io dependencies too, not just local path crates.)

## Still testing a few dependency combinations

If you *do* want specific combinations that include an optional dependency, pin them explicitly — they're added back regardless of `skip_optional_dependencies`:

```toml
[package.metadata.cargo-fc]
skip_optional_dependencies = true
include_feature_sets = [
  ["compression", "redis-backend"],
]
```

See [`skip_optional_dependencies`]({{< relref "../configuration/feature-matrix.md#skip_optional_dependencies" >}}) and [`include_feature_sets`]({{< relref "../configuration/feature-matrix.md#include_feature_sets" >}}).
