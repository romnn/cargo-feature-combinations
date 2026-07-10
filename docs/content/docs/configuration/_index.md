---
title: Configuration
weight: 5
bookCollapseSection: true
---

# Configuration

Everything about the matrix is configured from `Cargo.toml` metadata. The defaults — the powerset of a crate's features, on the host target — need no configuration at all. You reach for configuration when you want to **shape** the matrix.

Read these in order the first time:

1. **[Basics]({{< relref "basics.md" >}})** — where configuration lives.
2. **[Shaping the feature matrix]({{< relref "feature-matrix.md" >}})** — the keys that add, remove, restrict, or pin combinations.
3. **[The override model]({{< relref "override-model.md" >}})** — the single precedence chain, patch operations, and `inherit`. This is the mental model that ties everything together.
4. **[Per-target configuration]({{< relref "per-target.md" >}})** — vary the matrix by target triple with `cfg(...)` selectors.
5. **[Per-command configuration]({{< relref "per-command.md" >}})** — vary the matrix by cargo subcommand.
6. **[Flags in config]({{< relref "flags.md" >}})** — set `cargo fc` flag defaults.

> [!TIP]
> If you just want a working snippet for a specific situation, the [Recipes]({{< relref "../recipes/_index.md" >}}) section has copy-paste configurations for common scenarios.
