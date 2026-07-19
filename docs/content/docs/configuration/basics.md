---
title: Basics
weight: 1
---

# Configuration basics

## Where configuration lives

Configuration goes in `Cargo.toml` metadata tables. There are two scopes:

- **Package** — `[package.metadata.cargo-fc]`, in a crate's `Cargo.toml`. Shapes that package's feature matrix.
- **Workspace** — `[workspace.metadata.cargo-fc]`, in the workspace root `Cargo.toml`. Applies across the workspace (package selection, targets, flag defaults).

```toml
# crate Cargo.toml
[package.metadata.cargo-fc]
exclude_features = ["default"]
```

```toml
# workspace root Cargo.toml
[workspace.metadata.cargo-fc]
exclude_packages = ["examples"]
```

## Excluding workspace packages

Remove packages from every run in the workspace metadata:

```toml
[workspace.metadata.cargo-fc]
exclude_packages = ["package-a", "package-b"]
```

Package exclusion is a workspace-level decision — a package can't exclude its siblings.

## What can be configured where

Not every setting is meaningful in every scope. The feature-matrix keys, for instance, only make sense on a package, because a workspace has no features of its own. The full picture is the [override model]({{< relref "override-model.md" >}}); the short version:

| Setting | Workspace | Package |
|---|:--:|:--:|
| `cargo fc` flag defaults | ✓ | ✓ |
| Feature-matrix keys (`exclude_features`, `only_features`, …) |  | ✓ |
| `exclude_packages` | ✓ |  |
| `targets` (target list) | ✓ | ✓ |
| `driver` | ✓ | ✓ |
| `env` (child Cargo process) | ✓ | ✓ |

Each of these can be refined further by target and by command — that's what the rest of this section covers.

## A first example

```toml
[package.metadata.cargo-fc]
# Don't vary `default`; it's implied by everything anyway.
exclude_features = ["default"]

# These two features are mutually exclusive.
exclude_feature_sets = [["postgres", "sqlite"]]

# Ignore implicit features created for optional dependencies.
skip_optional_dependencies = true
```

Next: [shaping the feature matrix]({{< relref "feature-matrix.md" >}}).
