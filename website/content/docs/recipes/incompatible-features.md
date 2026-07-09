---
title: Incompatible features
weight: 1
---

# Incompatible features

**Scenario:** an HTTP client crate offers two TLS backends, `native-tls` and `rustls`. A consumer picks one — enabling both is a real conflict (here the code even `compile_error!`s on it). You want the matrix to skip that combination.

{{< cargofile "docs/recipes/incompatible-features" >}}

Any generated combination that is a superset of `{native-tls, rustls}` is removed. Every other combination — each backend alone, and neither — is still checked. `cargo fc check` visits exactly three combinations; the `{native-tls, rustls}` pair never appears:

{{< terminal name="recipe-incompatible" >}}

## More than one incompatible pair

`exclude_feature_sets` takes a list, so list each forbidden grouping:

```toml
[package.metadata.cargo-fc]
exclude_feature_sets = [
  ["native-tls", "rustls"],   # TLS backends
  ["tokio", "async-std"],     # async runtimes
]
```

## Only incompatible on some targets

If a pair conflicts only on one target, add it there instead of the base — use `add` so it extends rather than replaces:

```toml
[package.metadata.cargo-fc]
exclude_feature_sets = [["native-tls", "rustls"]]

[package.metadata.cargo-fc.target.'cfg(target_os = "windows")']
exclude_feature_sets = { add = [["metal", "vulkan"]] }
```

See [Per-target configuration]({{< relref "../configuration/per-target.md" >}}).
