#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Rebuilds tests/data/ft8-wsjtx-tones.tsv: the tones WSJT-X's ft8code sends
# for a thousand random bench messages, hand-picked edge cases and ARRL Field
# Day exchanges. Messages WSJT-X does not send as standard or Field Day
# messages are left out.
#
# Usage: make-tone-fixture.sh [WORK_DIR]   (where build-wsjtx.sh built ft8code)
set -euo pipefail

WORK="${1:-/var/tmp/loglab-ft8-reference}"
AGENT="$(cd "$(dirname "$0")/../.." && pwd)"
FT8CODE="$WORK/build/ft8code"
OUT="$AGENT/tests/data/ft8-wsjtx-tones.tsv"

[ -x "$FT8CODE" ] || { echo "run build-wsjtx.sh first ($FT8CODE not found)" >&2; exit 1; }
cargo build --quiet --release --manifest-path "$AGENT/Cargo.toml" --bin ft8bench
BENCH="$AGENT/target/release/ft8bench"

{
  echo "# FT8 tones from WSJT-X's ft8code (WSJT-X commit 967c85a), one message per line:"
  echo "# message<TAB>79 tones. Regenerate with tools/ft8-reference/make-tone-fixture.sh."
  {
    "$BENCH" messages --seed 2026 --count 1000
    cat "$(dirname "$0")/edge-messages.txt"
    grep -v '^#' "$(dirname "$0")/field-day-messages.txt"
  } | while IFS= read -r message; do
    out="$("$FT8CODE" "$message")"
    if sed -n 3p <<<"$out" | grep -qE "Standard msg|ARRL Field Day"; then
      tones="$(grep -E '^[0-7]{7} [0-7]{29} [0-7]{7} [0-7]{29} [0-7]{7}' <<<"$out" | tr -d ' ' | tail -1)"
      printf '%s\t%s\n' "$message" "$tones"
    fi
  done
} >"$OUT"
echo "wrote $(grep -vc '^#' "$OUT") messages to $OUT"
