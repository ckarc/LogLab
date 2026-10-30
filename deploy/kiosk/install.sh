#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

# Install or update LabLog on FieldLab's kiosk box. Run it from the
# checkout (/opt/lablog) as the kiosk's user, who can run docker:
#
#   deploy/kiosk/install.sh
#
# The first run makes the database password (kept in deploy/kiosk/.env,
# readable only by you), builds the Engine and starts it with PostgreSQL 16.
# Later runs rebuild from the checkout as it is and restart the Engine; the
# log is kept in the "lablog_db" volume and never touched. It also has cron
# run autoupdate.sh every five minutes, which keeps the box at the newest
# main by itself while the internet is the shop link.
# It needs internet only to build; LabLog then runs with none (Principle V).

set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
cd "$here"

if [[ ! -f .env ]]; then
    umask 077
    printf 'LABLOG_DB_PASSWORD=%s\n' "$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')" > .env
    echo "Made the database password, in $here/.env."
fi

docker compose build engine
docker compose up -d

# Keep it at the newest main by itself, on the shop link only (autoupdate.sh).
if command -v crontab > /dev/null; then
    { crontab -l 2> /dev/null | grep -vF "$here/autoupdate.sh" || true
      echo "*/5 * * * * $here/autoupdate.sh"; } | crontab -
    echo "Updates: checks for a newer LabLog every 5 minutes, on the shop link only."
else
    echo "Updates: this box has no cron, so run install.sh again to update."
fi

# Port 8073 open to FieldLab's network, when the box has a firewall on.
if command -v ufw > /dev/null && sudo ufw status 2> /dev/null | grep -q '^Status: active'; then
    sudo ufw allow from 10.73.10.0/24 to any port 8073 proto tcp > /dev/null
    echo "Firewall: port 8073 open to 10.73.10.0/24."
fi

echo "Waiting for the Engine to answer..."
for _ in $(seq 60); do
    if curl -fsS http://127.0.0.1:8073/api/v1/status > /dev/null 2>&1; then
        echo "LabLog is running: http://$(hostname -I | awk '{print $1}'):8073"
        echo "First install? Make the first administrator (asks for a PIN):"
        echo "  docker compose -f $here/compose.yaml exec engine lablog-operator setup CALL"
        exit 0
    fi
    sleep 2
done
echo "The Engine didn't answer in 2 minutes. See: docker compose -f $here/compose.yaml logs engine"
exit 1
