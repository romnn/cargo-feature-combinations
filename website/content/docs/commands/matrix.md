---
title: The matrix subcommand
weight: 3
---

# The `matrix` subcommand

`cargo fc matrix` prints the feature matrix as JSON instead of running a command. It's the bridge to CI: emit the matrix in one job, then fan out a build/test job per row.

```bash
cargo fc matrix
cargo fc matrix --pretty
```

## Output shape

The matrix is a JSON array with one object per combination. Each object carries the package `name`, the comma-joined `features` string, the effective `target`, and any configured `metadata`:

{{< terminal name="matrix" >}}

Without `--pretty` the array is printed on a single line (what CI consumes). Pass `--features "${features}"` from a row straight into a downstream `cargo` command. (The `metadata` field is covered [below](#custom-metadata).)

## Custom metadata

Attach arbitrary metadata to every row of a package — handy for routing jobs (for example, "this combination needs a GPU runner"). Configure it in `Cargo.toml`:

```toml
[package.metadata.cargo-fc]
matrix = { kind = "ci" }
```

or as its own section:

```toml
[package.metadata.cargo-fc.matrix]
requires-gpu = false
value-for-this-crate = "shows up in the feature matrix"
```

The values appear under each row's `metadata` key:

```bash
cargo fc matrix --pretty
```

```json
[
  {
    "name": "my-crate",
    "features": "",
    "metadata": { "requires-gpu": false, "value-for-this-crate": "shows up in the feature matrix" }
  }
]
```

In a GitHub Actions matrix this is available as `matrix.package.metadata.<key>`.

## The `target` field

If you declare [configured targets]({{< relref "../targets/configured-targets.md" >}}), every row also gains a `target` field, so you can fan the CI matrix out over targets as well as combinations:

```json
{ "name": "engine", "features": "metrics", "target": "x86_64-unknown-linux-gnu" }
```

## `--packages-only`

Emit one row per package (or package-target) instead of one row per feature combination — useful when the downstream job iterates combinations itself:

```bash
cargo fc matrix --packages-only
```

## Scale

Up to 256 feature sets can be processed per GitHub Actions job. For the CI patterns that use this output, see [Continuous integration]({{< relref "../ci/_index.md" >}}).
