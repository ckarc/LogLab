<!-- SPDX-FileCopyrightText: 2026 LabLog contributors -->
<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# LabLog on FieldLab's kiosk box

The kiosk box (FieldLab-Linux, 10.73.10.10) runs the Engine and PostgreSQL 16
in Docker, beside the kiosk's own containers, so every FieldLab PC reaches
LabLog at `http://10.73.10.10:8073` with or without internet (Principle V).

```sh
sudo git clone https://github.com/ckarc/LogLab /opt/lablog
sudo chown -R "$USER": /opt/lablog
/opt/lablog/deploy/kiosk/install.sh
docker compose -f /opt/lablog/deploy/kiosk/compose.yaml exec engine lablog-operator setup CALL
```

- The Engine answers on port 8073 on every address; PostgreSQL only on
  127.0.0.1. Both start again when the box boots.
- The database password is made on the first run and kept in
  `deploy/kiosk/.env`, readable only by the kiosk's user. The log lives in
  the Docker volume `lablog_db`.
- The Engine reads the kiosk's MQTT broker on 127.0.0.1:1883 for the GPS fix
  and which internet link is up, so it downloads nothing over Starlink.
- To update: `git -C /opt/lablog pull` then `install.sh` again (needs
  internet to build). Migrations run as the Engine starts.
- Logs: `docker compose -f /opt/lablog/deploy/kiosk/compose.yaml logs engine`.

FieldLab PCs need no settings for it: the Station Agent links to this Engine
by itself, as the station named after the computer (`fl-1` on FL-1), and an
administrator approves it once in LabLog under Menu > Station Agents.
