---
title: Recipes
weight: 7
bookCollapseSection: true
---

# Recipes

Copy-paste configurations for common situations. Each recipe is a small, self-contained snippet with a short explanation. For the underlying model, see [Configuration]({{< relref "../configuration/_index.md" >}}).

- **[Incompatible features]({{< relref "incompatible-features.md" >}})** — two features that must never be enabled together.
- **[Optional dependencies]({{< relref "optional-dependencies.md" >}})** — stop the matrix exploding over optional deps.
- **[Restrict the matrix]({{< relref "restrict-matrix.md" >}})** — test only an explicit allowlist of configurations.
- **[Large feature sets]({{< relref "large-feature-sets.md" >}})** — independent feature groups without a combinatorial explosion.
- **[Per-command differences]({{< relref "per-command-differences.md" >}})** — build a feature you never test.
- **[Cross-compilation in CI]({{< relref "cross-compilation-ci.md" >}})** — check every combination on every target.
