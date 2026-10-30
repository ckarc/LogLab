# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

# The LabLog Engine, for FieldLab's kiosk box. Built once while there is
# internet; after that it runs with none (Principle V).
FROM python:3.12-slim

# psycopg uses the system's libpq.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libpq5 \
    && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir uv==0.11.32

WORKDIR /app
COPY engine/ /app/
RUN uv sync --locked --no-dev
ENV PATH=/app/.venv/bin:$PATH

# Bring the database's schema up to date, then serve on every address so
# FieldLab's PCs reach it on port 8073.
CMD ["sh", "-c", "lablog-log migrate && exec lablog-engine serve --host 0.0.0.0 --port 8073"]
