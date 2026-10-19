#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Scores LogLab's own FT8 decoder on the bench: generates the scenario,
# decodes every recording with LogLab, and prints the score. Needs nothing
# from WSJT-X. Compare with run-reference.sh on the same seed.
#
# Usage: run-loglab.sh [SEED] [WORK_DIR]
#   SEED defaults to 1; WORK_DIR to /var/tmp/loglab-ft8-reference.
set -euo pipefail

SEED="${1:-1}"
WORK="${2:-/var/tmp/loglab-ft8-reference}"
AGENT="$(cd "$(dirname "$0")/../.." && pwd)"
SCENARIO="$WORK/scenario-$SEED"

cargo build --quiet --release --manifest-path "$AGENT/Cargo.toml" --bin ft8bench
BENCH="$AGENT/target/release/ft8bench"

"$BENCH" generate "$SCENARIO" --seed "$SEED" >/dev/null
"$BENCH" decode "$SCENARIO" --out "$SCENARIO/loglab"
"$BENCH" score "$SCENARIO/truth.tsv" "$SCENARIO/loglab" --name "LabLog, seed $SEED"
