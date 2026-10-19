#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Sweeps FT8 decoders across simulated propagation conditions: for every
# condition in channels.txt (or those named), renders the same seed's
# recordings through that channel, decodes them with each decoder given,
# and writes one score per condition and decoder. The messages, their
# placing and the noise are the same for every condition, so conditions
# differ only in what the channel did.
#
# Usage: run-channels.sh SEED OUT-DIR DECODER... [-- CONDITION...]
#   DECODER is NAME=PATH-TO-ft8bench (LogLab's decoder as built by that
#   binary) or wsjtx=PATH-TO-jt9.
#   Environment: SLOTS (default 40), MIN_SNR (-24), MAX_SNR (-8),
#   SIGNALS (10). With SIGNALS=0 the recordings are noise alone.
set -euo pipefail

SEED="$1"; OUT="$2"; shift 2
DECODERS=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do DECODERS+=("$1"); shift; done
[ "${1:-}" = "--" ] && shift
HERE="$(cd "$(dirname "$0")" && pwd)"
AGENT="$(cd "$HERE/../.." && pwd)"
cargo build --quiet --release --manifest-path "$AGENT/Cargo.toml" --bin ft8bench
BENCH="$AGENT/target/release/ft8bench"
SLOTS="${SLOTS:-40}"; MIN_SNR="${MIN_SNR:--24}"; MAX_SNR="${MAX_SNR:--8}"; SIGNALS="${SIGNALS:-10}"

grep -v '^#' "$HERE/channels.txt" | while read -r name channel; do
  [ -n "$name" ] || continue
  if [ $# -gt 0 ] && ! printf '%s\n' "$@" | grep -qx "$name"; then continue; fi
  dir="$OUT/$name"
  "$BENCH" generate "$dir" --seed "$SEED" --slots "$SLOTS" --signals "$SIGNALS" \
    --min-snr "$MIN_SNR" --max-snr "$MAX_SNR" --channel "$channel" >/dev/null
  for decoder in "${DECODERS[@]}"; do
    who="${decoder%%=*}"; tool="${decoder#*=}"
    mkdir -p "$dir/$who"
    if [ "$who" = wsjtx ]; then
      start=$(date +%s.%N)
      ls "$dir"/slot-*.wav | xargs -P "$(nproc)" -I{} sh -c \
        's=$(mktemp -d); stem=$(basename {} .wav); (cd $s && "$0" -8 -d 3 -q -a $s -t $s {}) > "$1/$stem.txt"; rm -rf $s' \
        "$tool" "$dir/$who"
      end=$(date +%s.%N)
      echo "jt9 -d 3, $(nproc) at once: $(echo "$end - $start" | bc) s" > "$dir/$who.time"
    else
      "$tool" decode "$dir" --out "$dir/$who" > "$dir/$who.time"
    fi
    "$BENCH" score "$dir/truth.tsv" "$dir/$who" --name "$who, $name, seed $SEED" > "$dir/$who.score.md"
    echo "$name $who $(grep -o '\*\*[0-9]*\*\* | \*\*[0-9.]*\*\*' "$dir/$who.score.md" | head -1) $(grep 'False decodes' "$dir/$who.score.md")"
  done
done
