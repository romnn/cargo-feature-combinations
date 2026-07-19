---
title: Incompatible features
weight: 1
---

# Incompatible features

**Scenario:** an HTTP client crate offers three TLS backends: `native-tls`,
`rustls`, and `boring`. A consumer picks at most one — enabling multiple
backends is a real conflict (here the code even `compile_error!`s on it). The
crate also has independent `logging` and `metrics` features that should keep
their full powerset.

{{< cargofile "docs/recipes/incompatible-features" >}}

`mutually_exclusive_features` keeps zero or one backend in every generated
combination and crosses those four choices with every subset of the two
independent features. `cargo fc check` therefore visits exactly
`(3 + 1) × 2² = 16` combinations, without ever generating a conflicting pair:

{{< terminal name="recipe-incompatible" >}}

## More than one alternative group

List each disjoint group independently:

```toml
[package.metadata.cargo-fc]
mutually_exclusive_features = [
  ["native-tls", "rustls", "boring"], # TLS backends
  ["tokio", "async-std"],              # async runtimes
]
```

Groups must not overlap. For a non-disjoint constraint such as “not `a + b`”
and “not `b + c`,” use the pairwise form:

```toml
[package.metadata.cargo-fc]
exclude_feature_sets = [["a", "b"], ["b", "c"]]
```

## Only incompatible on some targets

If alternatives conflict only on one target, add their group there instead of
the base — use `add` so it extends rather than replaces:

```toml
[package.metadata.cargo-fc]
mutually_exclusive_features = [["native-tls", "rustls", "boring"]]

[package.metadata.cargo-fc.target.'cfg(target_os = "windows")']
mutually_exclusive_features = { add = [["metal", "vulkan"]] }
```

See [Per-target configuration]({{< relref "../configuration/per-target.md" >}}).
