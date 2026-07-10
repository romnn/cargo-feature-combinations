#!/usr/bin/env bash
# Regenerate the README screenshots from the checked-in workspaces under examples/.
#
# Every screenshot is produced by `freeze` (https://github.com/charmbracelet/freeze) from real
# `cargo fc` output against small, committed, dependency-free workspaces — so they are fully
# reproducible from this repo with no external checkout. The workspaces are deliberately tiny so
# each capture stays compact and roughly square (a good centered hero image), and the diagnostics
# workspace contains real warnings/errors so the diagnostics, dedupe, and pruning shots have
# something to show.
#
# These PNGs exist for the README only: GitHub markdown can't render the docs site's live HTML
# terminals, so it needs static images. The docs *site* renders the same `cargo fc` output as HTML
# via scripts/terminals.sh, so it does not use these images — which is why the docs build does not
# depend on this script. Run it by hand (`task docs:screenshots`) to refresh the README shots.
#
# `--color always` forces ANSI because freeze captures via a pipe, not a PTY. We warm each example's
# target dir before capturing so the output is just the feature-combination run, without the
# one-time `Compiling <dep>` noise. Images are downscaled to a sane width afterwards.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fc="$repo/target/release/cargo-fc"
docs="$repo/docs/static/images"
examples="$repo/examples"
max_width=2200

command -v freeze >/dev/null || { echo "freeze not found — install charmbracelet/freeze" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq not found — install jq" >&2; exit 1; }
# ImageMagick 7 is `magick`; ImageMagick 6 (Ubuntu apt) is `convert`. Accept either.
magick_bin="$(command -v magick || command -v convert || true)"
[[ -n "$magick_bin" ]] || { echo "ImageMagick not found — install 'magick' (v7) or 'convert' (v6)" >&2; exit 1; }
[[ -x "$fc" ]] || cargo build --release --bin cargo-fc --manifest-path "$repo/Cargo.toml"
mkdir -p "$docs"

frame=(--window --shadow.blur 20 --border.radius 8 --padding 20)

# Cap the rendered width so the images are crisp in the README without being multi-megapixel.
downscale() {
  "$magick_bin" "$1" -resize "${max_width}>" "$1"
}

# Render captured ANSI to a PNG with freeze. Usage: freeze_render <output.png> <ansi>
#
# freeze rasterizes SVG→PNG through a bundled WASM renderer (wazero), which intermittently
# segfaults with a Go GC crash — nondeterministically, on inputs that render fine on the next
# attempt. Retry a few times before giving up. Input is captured by the caller (not piped straight
# from `cargo fc`) so each attempt re-feeds the same bytes.
freeze_render() {
  local out="$1" ansi="$2" attempt
  for attempt in 1 2 3 4 5; do
    if printf '%s\n' "$ansi" | freeze --language ansi --output "$out" "${frame[@]}"; then
      return 0
    fi
    echo "freeze crashed rendering $out (attempt $attempt/5); retrying" >&2
  done
  echo "freeze failed to render $out after 5 attempts" >&2
  return 1
}

# Terminal-style shot: a shell prompt line followed by the real (colored) output.
# Usage: shoot_term <example-dir> <output.png> <fc args...>
shoot_term() {
  local dir="$examples/$1" out="$docs/$2"; shift 2
  ( cd "$dir" && "$fc" "$@" >/dev/null 2>&1 || true ) # warm the build
  local ansi
  ansi="$(
    cd "$dir"
    printf '\033[1;32m$\033[0m cargo fc %s\n\n' "$*"
    # Merge streams: diagnostics go to stderr, the summary to stdout.
    "$fc" "$@" --color always 2>&1 || true
  )"
  freeze_render "$out" "$ansi"
  downscale "$out"
  echo "wrote $out"
}

# Matrix shot: the JSON feature matrix piped through `jq -c` to one row per
# combination. This keeps the image's proportions and font size in line with the
# other terminal shots (the multi-line `--pretty` form renders as a tall, narrow
# strip), while still showing a real command.
shoot_matrix() {
  local dir="$examples/$1" out="$docs/$2"
  local ansi
  ansi="$(
    cd "$dir"
    printf '\033[1;32m$\033[0m cargo fc matrix | jq -c %s\n\n' "'.[]'"
    "$fc" matrix | jq -c '.[]'
  )"
  freeze_render "$out" "$ansi"
  downscale "$out"
  echo "wrote $out"
}

# Clean workspace — the hero, plus the JSON matrix.
shoot_term   clean       check.png   check --workspace
shoot_matrix clean       matrix.png

# Targets workspace — a feature matrix checked across multiple target triples.
shoot_term   targets     targets.png --summary-only check --workspace

# Diagnostics workspace — summary with WARN/FAIL, raw diagnostics, and deduped diagnostics.
shoot_term   diagnostics summary.png     --summary-only check --workspace
shoot_term   diagnostics diagnostics.png --diagnostics-only clippy --workspace
shoot_term   diagnostics dedupe.png      --dedupe clippy --workspace

# Pruning workspace — pruned (SKIP) combinations shown explicitly.
shoot_term   pruning     pruned.png  --summary-only --show-pruned check --workspace
