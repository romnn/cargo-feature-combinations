---
title: Targets & cross-compilation
weight: 6
bookCollapseSection: true
---

# Targets & cross-compilation

By default `cargo fc` runs for a single target — the same one Cargo would pick. You can instead declare a list of target triples and turn each run into a full matrix of:

```text
selected packages × effective targets × feature combinations
```

With `targets = ["x86_64-unknown-linux-gnu", "wasm32-unknown-unknown"]` declared in `Cargo.toml`, a single `cargo fc` run checks every feature combination on every target:

{{< terminal name="targets" >}}

- **[Configured targets]({{< relref "configured-targets.md" >}})** — declare the target list and control which commands expand it.
- **[Build drivers]({{< relref "drivers.md" >}})** — how native-C dependencies cross-compile, and how to choose the driver.
- **[Installing targets]({{< relref "installing-targets.md" >}})** — opt-in installation of missing `rustup` target components.
