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
# `--color always` forces ANSI because freeze captures via a pipe, not a PTY. We warm each example's
# target dir before capturing so the output is just the feature-combination run, without the
# one-time `Compiling <dep>` noise. Images are downscaled to a sane width afterwards.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fc="$repo/target/release/cargo-fc"
docs="$repo/docs"
examples="$repo/examples"
max_width=2200

command -v freeze >/dev/null || { echo "freeze not found — install charmbracelet/freeze" >&2; exit 1; }
command -v magick >/dev/null || { echo "magick not found — install ImageMagick" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq not found — install jq" >&2; exit 1; }
[[ -x "$fc" ]] || cargo build --release --bin cargo-fc --manifest-path "$repo/Cargo.toml"
mkdir -p "$docs"

frame=(--window --shadow.blur 20 --border.radius 8 --padding 20)

# Cap the rendered width so the images are crisp in the README without being multi-megapixel.
downscale() {
  magick "$1" -resize "${max_width}>" "$1"
}

# Terminal-style shot: a shell prompt line followed by the real (colored) output.
# Usage: shoot_term <example-dir> <output.png> <fc args...>
shoot_term() {
  local dir="$examples/$1" out="$docs/$2"; shift 2
  ( cd "$dir" && "$fc" "$@" >/dev/null 2>&1 || true ) # warm the build
  (
    cd "$dir"
    {
      printf '\033[1;32m❯\033[0m cargo fc %s\n\n' "$*"
      "$fc" "$@" --color always || true
    } | freeze --language ansi --output "$out" "${frame[@]}"
  )
  downscale "$out"
  echo "wrote $out"
}

# Matrix shot: the JSON feature matrix piped through `jq -c` to one row per
# combination. This keeps the image's proportions and font size in line with the
# other terminal shots (the multi-line `--pretty` form renders as a tall, narrow
# strip), while still showing a real command.
shoot_matrix() {
  local dir="$examples/$1" out="$docs/$2"
  (
    cd "$dir"
    {
      printf '\033[1;32m❯\033[0m cargo fc matrix | jq -c %s\n\n' "'.[]'"
      "$fc" matrix | jq -c '.[]'
    } | freeze --language ansi --output "$out" "${frame[@]}"
  )
  downscale "$out"
  echo "wrote $out"
}

# Clean workspace — the hero, plus the JSON matrix.
shoot_term   clean       check.png   check --workspace
shoot_matrix clean       matrix.png

# Diagnostics workspace — summary with WARN/FAIL, raw diagnostics, and deduped diagnostics.
shoot_term   diagnostics summary.png     --summary-only check --workspace
shoot_term   diagnostics diagnostics.png --diagnostics-only clippy --workspace
shoot_term   diagnostics dedupe.png      --dedupe clippy --workspace

# Pruning workspace — pruned (SKIP) combinations shown explicitly.
shoot_term   pruning     pruned.png  --summary-only --show-pruned check --workspace
