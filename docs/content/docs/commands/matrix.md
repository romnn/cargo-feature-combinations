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

`cargo fc matrix` prints and exits — it never builds anything. It therefore parses its own arguments instead of forwarding them to Cargo: `cargo fc matrix --help` prints the options it accepts, and an argument the matrix cannot honor (`--release`, `--all-targets`, a misspelled `--pretyy`) is an error rather than a silently ignored token. Cargo-fc flags that only shape a *run* — `--driver`, `--env`, `--summary-only`, `--fail-fast` — stay accepted, so the same command line works against `matrix` or a real run; a note lists the ones that had no effect. Cargo's lockfile flags (`--frozen`, `--locked`, `--offline`) are also accepted: they constrain the `cargo metadata` step the matrix is built from.

## Previewing one command

The matrix depends on which command is being run, because [per-command configuration]({{< relref "../configuration/per-command.md" >}}) can add or remove features for a single subcommand. Name that command to see the matrix it would produce:

```bash
cargo fc matrix build     # exactly the combinations `cargo fc build` runs
cargo fc matrix test
cargo fc matrix lint      # cargo aliases are expanded first
```

A plain `cargo fc matrix` applies no per-command layer. Only one command may be named per invocation.

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

Pass `--target <triple>` to resolve the matrix for one specific target instead of the configured list. The triple selects the row's `target` field *and* the [`cfg(...)` target overrides]({{< relref "../configuration/per-target.md" >}}) the feature sets are resolved under, so it is the way to preview what a single CI leg will cover:

```bash
cargo fc matrix --target aarch64-apple-darwin build
```

## `--packages-only`

Emit one row per package (or package-target) instead of one row per feature combination — useful when the downstream job iterates combinations itself:

```bash
cargo fc matrix --packages-only
```

## Scale

Up to 256 feature sets can be processed per GitHub Actions job. For the CI patterns that use this output, see [Continuous integration]({{< relref "../ci/_index.md" >}}).
