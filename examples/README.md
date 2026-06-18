# examples

Throwaway Cargo workspaces used **only** to generate the README screenshots, via
`task screenshots` (see `../scripts/screenshots.sh`). They are dependency-free and
deliberately small so each screenshot stays compact. Each one's `[workspace]`
table isolates it from the parent `cargo-feature-combinations` crate, and the
whole `examples/` directory is excluded from packaging — so none of this is
linted, built, tested, or published as part of the tool.

- `clean/` — two crates, every feature combination compiles cleanly (the hero shot).
- `diagnostics/` — unused-code warnings repeated across the matrix, plus a feature
  whose code does not compile, for the diagnostics / dedupe / failing-combination shots.
- `pruning/` — a `full` feature that implies the others, so redundant combinations
  get pruned.
