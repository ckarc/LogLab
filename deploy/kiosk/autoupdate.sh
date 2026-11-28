#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

# Keeps LabLog on the kiosk box at the newest main. install.sh has cron run
# it every five minutes as the kiosk's user. Each run, when origin/main has
# moved: pulls it (fast-forward only), and when the Engine or this folder
# changed, rebuilds the Engine and restarts it (migrations run as it starts).
#
# It only looks while FieldLab's gateway says the internet is the shop link
# (asked of the Engine: lablog-engine uplink), never on Starlink, which has
# a data budget, or the phone tether. No internet, or no word from the
# gateway: it tries again next time; LabLog keeps running as it is
# (Principle V). A build that fails
# restarts nothing: the checkout goes back to the commit that is running and
# that commit of main is not tried again (the next merge is). A checkout not
# on main, or with local edits, is left alone.
#
# What it did: ~/.local/state/lablog-kiosk/autoupdate.log

set -u -o pipefail
export PATH="/usr/local/bin:/usr/bin:/bin:$PATH"

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
state="$HOME/.local/state/lablog-kiosk"
log="$state/autoupdate.log"
mkdir -p "$state"

say() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >> "$log"; }

exec 9> "$state/lock"
flock -n 9 || exit 0

# Keep the log short.
if [[ -f "$log" ]] && (($(wc -l < "$log") > 2000)); then
    tail -n 1000 "$log" > "$log.tmp" && mv "$log.tmp" "$log"
fi

cd "$repo" || exit 1
[[ "$(git rev-parse --abbrev-ref HEAD)" == main ]] || exit 0
git diff --quiet && git diff --cached --quiet || exit 0

compose=(docker compose -f "$here/compose.yaml")
"${compose[@]}" exec -T engine lablog-engine uplink > /dev/null 2>&1 || exit 0
timeout 120 git fetch -q origin main 2> /dev/null || exit 0

head=$(git rev-parse HEAD)
target=$(git rev-parse origin/main)
[[ "$head" == "$target" ]] && exit 0
[[ "$(cat "$state/failed" 2> /dev/null)" == "$target" ]] && exit 0

changed=$(git diff --name-only "$head" "$target")
if ! git merge -q --ff-only "$target" >> "$log" 2>&1; then
    say "main can't be fast-forwarded to ${target:0:7}; left alone"
    echo "$target" > "$state/failed"
    exit 1
fi

if ! grep -qE '^(engine/|deploy/kiosk/)' <<< "$changed"; then
    say "pulled ${target:0:7}; nothing for the Engine"
    exit 0
fi

if "${compose[@]}" build engine >> "$log" 2>&1 && "${compose[@]}" up -d >> "$log" 2>&1; then
    say "deployed ${target:0:7}"
else
    git reset -q --hard "$head"
    echo "$target" > "$state/failed"
    say "build of ${target:0:7} failed; still running ${head:0:7}"
    exit 1
fi
