#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Scores WSJT-X's FT8 decoder on the LogLab bench: generates the scenario,
# decodes every recording with WSJT-X's jt9 at its deepest setting, and prints
# the score. This is the bar LogLab's own decoder has to meet.
#
# Usage: run-reference.sh [SEED] [WORK_DIR]
#   SEED defaults to 1; WORK_DIR to /var/tmp/loglab-ft8-reference, where
#   build-wsjtx.sh put jt9.
set -euo pipefail

SEED="${1:-1}"
WORK="${2:-/var/tmp/loglab-ft8-reference}"
AGENT="$(cd "$(dirname "$0")/../.." && pwd)"
JT9="$WORK/build/jt9"
SCENARIO="$WORK/scenario-$SEED"
DECODES="$SCENARIO/wsjtx"

[ -x "$JT9" ] || { echo "run build-wsjtx.sh first ($JT9 not found)" >&2; exit 1; }
cargo build --quiet --release --manifest-path "$AGENT/Cargo.toml" --bin ft8bench
BENCH="$AGENT/target/release/ft8bench"

"$BENCH" generate "$SCENARIO" --seed "$SEED" >/dev/null
mkdir -p "$DECODES"
for wav in "$SCENARIO"/slot-*.wav; do
  stem="$(basename "$wav" .wav)"
  scratch="$(mktemp -d)"
  # -8 FT8, -d 3 deepest decoding, -q no end marker.
  (cd "$scratch" && "$JT9" -8 -d 3 -q -a "$scratch" -t "$scratch" "$wav") >"$DECODES/$stem.txt"
  rm -rf "$scratch"
done
"$BENCH" score "$SCENARIO/truth.tsv" "$DECODES" --name "WSJT-X 3.0.2 (jt9 -d 3), seed $SEED"
