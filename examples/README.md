# examples

Throwaway Cargo workspaces used to generate the README screenshots (via
`task screenshots`, see `../scripts/screenshots.sh`) and the documentation
terminal snippets (via `task docs:terminals`, see `../scripts/terminals.sh`).
They are dependency-free and deliberately small. Each one's `[workspace]` table
isolates it from the parent `cargo-feature-combinations` crate, and the whole
`examples/` directory is excluded from packaging — so none of this is linted,
built, tested, or published as part of the tool.

Screenshot / top-level workspaces:

- `clean/` — two crates, every feature combination compiles cleanly (the hero shot).
- `diagnostics/` — unused-code warnings repeated across the matrix, plus a feature
  whose code does not compile, for the diagnostics / dedupe / failing-combination shots.
- `pruning/` — a `full` feature that implies the others, so redundant combinations
  get pruned.
- `targets/` — declares a `targets` matrix (a couple of triples), so the run fans
  each feature combination out across every target, for the multi-target shots.

Documentation recipe workspaces under `docs/recipes/` — each backs one page in
the docs' Recipes section. The docs render their `Cargo.toml` **verbatim** (via a
Hugo mount) and embed the real `cargo fc` output, so the examples can never drift
from the docs — breaking the CLI changes what the docs show.

- `docs/recipes/incompatible-features/` — two mutually-exclusive TLS backends.
- `docs/recipes/optional-dependencies/` — a store crate with optional path-dependency backends.
- `docs/recipes/restrict-matrix/` — an allowlist of exactly the shipped configurations.
- `docs/recipes/large-feature-sets/` — independent format × compression axes.
- `docs/recipes/per-command/` — a `gpu` feature built everywhere but excluded from `test`.
