# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Connections to the Engine's PostgreSQL database."""

from __future__ import annotations

from typing import Final

import psycopg
from psycopg.rows import TupleRow

from loglab_engine.environment import setting

DATABASE_SETTING: Final = "DATABASE"
DEFAULT_DATABASE: Final = "dbname=loglab"


def default_database() -> str:
    """The connection string to use when none is given: ``$LABLOG_DATABASE`` or ``loglab``."""
    return setting(DATABASE_SETTING) or DEFAULT_DATABASE


def connect(conninfo: str) -> psycopg.Connection[TupleRow]:
    """Open an autocommit connection whose session clock is UTC (Principle VIII).

    Autocommit means nothing is ever left in an open implicit transaction; every
    change is made inside an explicit ``conn.transaction()`` block.

    Args:
        conninfo: A libpq connection string or URL, e.g. ``dbname=loglab``.
    """
    return psycopg.connect(conninfo, autocommit=True, options="-c timezone=UTC")
