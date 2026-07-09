#!/usr/bin/env bash
# Regenerate the documentation terminal snippets under website/assets/terminals/.
#
# Each snippet is the real, colored output of `cargo fc` against the committed,
# dependency-free workspaces under examples/, converted from ANSI to HTML with
# `terminal-to-html` (https://github.com/buildkite/terminal-to-html), which mise
# provides via its go backend. This mirrors scripts/screenshots.sh, which
# produces the PNG screenshots with `freeze` from the same workspaces — same
# inputs, different renderer.
#
# The snippets are committed, so the Hugo site builds without terminal-to-html;
# run this only to regenerate them. The `terminal` shortcode inlines each snippet.
#
# `--color always` forces ANSI because output is captured through a pipe, not a
# PTY. We warm each example's target dir first so the capture is just the
# feature-combination run, and normalize the wall-clock duration so the output
# is reproducible.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fc="$repo/target/release/cargo-fc"
examples="$repo/examples"
out="$repo/website/assets/terminals"

command -v terminal-to-html >/dev/null || { echo "terminal-to-html not found — run 'mise install' (provided via the go backend)" >&2; exit 1; }
[[ -x "$fc" ]] || cargo build --release --bin cargo-fc --manifest-path "$repo/Cargo.toml"
mkdir -p "$out"

# Render one snippet. Usage: shoot <name> <example-dir> <fc args...>
shoot() {
  local name="$1" dir="$examples/$2"; shift 2
  ( cd "$dir" && "$fc" "$@" >/dev/null 2>&1 || true ) # warm the build
  {
    printf '\033[1;32m❯\033[0m cargo fc %s\n\n' "$*"
    # Capture both streams: diagnostics (warnings/errors) go to stderr, the
    # summary to stdout — a user sees them merged in the terminal.
    ( cd "$dir" && "$fc" "$@" --color always 2>&1 || true )
  } \
    | terminal-to-html \
    | sed -E 's/ in [0-9]+(\.[0-9]+)?s/ in 0.00s/g' \
    > "$out/$name.html"
  echo "wrote $out/$name.html"
}

# Diagnostics workspace — real warnings, plus a feature that fails to compile.
shoot summary     diagnostics --summary-only check --workspace
shoot diagnostics diagnostics --diagnostics-only clippy --workspace
shoot dedupe      diagnostics --dedupe clippy --workspace

# Pruning workspace — a `full` feature implies the others, so combinations prune.
shoot pruned      pruning --summary-only --show-pruned check --workspace

# Targets workspace — a feature matrix checked across multiple target triples,
# and the same run with --aggregate-targets (one invocation per combination).
shoot targets           targets --summary-only check --workspace
shoot aggregate-targets targets --aggregate-targets --summary-only check --workspace

# Clean workspace — a passing run summary, and the JSON feature matrix.
shoot check       clean --summary-only check --workspace
shoot matrix      clean matrix --pretty

# Recipe workspaces (examples/docs/recipes/*) — the real output each recipe shows.
shoot recipe-incompatible  docs/recipes/incompatible-features --summary-only check
shoot recipe-restrict      docs/recipes/restrict-matrix       --summary-only check
shoot recipe-large         docs/recipes/large-feature-sets    --summary-only check
shoot recipe-optional      docs/recipes/optional-dependencies -p store --summary-only check
shoot recipe-per-cmd-build docs/recipes/per-command           --summary-only build
shoot recipe-per-cmd-test  docs/recipes/per-command           --summary-only test
