# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Station keys: how a Station Agent proves which station it is (Principle XXI).

An administrator makes a key for a station and gives it to the Agent there.
The Agent sends the key with every report of its radio; :meth:`authenticate`
answers which station it belongs to. Only the key's SHA-256 is stored, as with
session tokens. A station has at most one working key: making a new one ends
the old one. The database enforces that only a signed-in administrator makes
or ends keys (migration ``0015_agent_key``).

A station key grants no operator permission. It lets an Agent say what its
radio is doing, and nothing else: it cannot log, and it can never transmit.
"""

from __future__ import annotations

import hashlib
import secrets
import uuid
from typing import TYPE_CHECKING, Final

from psycopg import errors

from loglab_engine.store.log import NotAuthorizedError, StoreError, check_station

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor

# Station keys start with this, so one is easy to tell from a session token.
PREFIX: Final = "lls_"
_KEY_BYTES: Final = 32


class AgentKeyError(StoreError):
    """A station key is unknown or has been ended."""


def _digest(key: str) -> str:
    return hashlib.sha256(key.encode()).hexdigest()


class AgentKeyStore:
    """Making, ending, and checking station keys, on one autocommit connection."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn`` for every operation; each method is one transaction."""
        self._conn = conn

    def make(self, admin: Actor, station: str) -> str:
        """Make a new key for ``station``, ending any key it had, and return it.

        The key is shown once, here; only its digest is kept.

        Raises:
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
            StoreError: The station name is not valid.
        """
        check_station(station)
        key = PREFIX + secrets.token_urlsafe(_KEY_BYTES)
        conn = self._conn
        try:
            with conn.transaction():
                conn.execute(
                    "INSERT INTO log.station (name) VALUES (%s) ON CONFLICT DO NOTHING",
                    (station,),
                )
                self._end_all(admin, station)
                conn.execute(
                    """
                    INSERT INTO log.agent_key
                        (id, station_id, key_sha256, made_by, session_id, made_at)
                    SELECT %s, st.id, %s, s.operator_id, s.id, transaction_timestamp()
                    FROM log.station st, log.session s
                    WHERE st.name = %s AND s.id = %s
                    """,
                    (uuid.uuid4(), _digest(key), station, admin.session_id),
                )
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(error.diag.message_primary or str(error)) from error
        return key

    def end(self, admin: Actor, station: str) -> bool:
        """End ``station``'s key, so its Agent is refused. Whether it had one.

        Raises:
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
        """
        check_station(station)
        try:
            with self._conn.transaction():
                return self._end_all(admin, station) > 0
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(error.diag.message_primary or str(error)) from error

    def _end_all(self, admin: Actor, station: str) -> int:
        cursor = self._conn.execute(
            """
            INSERT INTO log.agent_key_end (key_id, ended_by, session_id, ended_at)
            SELECT k.id, s.operator_id, s.id, transaction_timestamp()
            FROM log.agent_key k
            JOIN log.station st ON st.id = k.station_id
            CROSS JOIN log.session s
            WHERE st.name = %s AND s.id = %s
              AND NOT EXISTS (SELECT 1 FROM log.agent_key_end e WHERE e.key_id = k.id)
            """,
            (station, admin.session_id),
        )
        return cursor.rowcount

    def authenticate(self, key: str) -> str:
        """The station a working key belongs to.

        Raises:
            AgentKeyError: The key is unknown or has been ended.
        """
        row = self._conn.execute(
            """
            SELECT st.name FROM log.agent_key k
            JOIN log.station st ON st.id = k.station_id
            WHERE k.key_sha256 = %s
              AND NOT EXISTS (SELECT 1 FROM log.agent_key_end e WHERE e.key_id = k.id)
            """,
            (_digest(key),),
        ).fetchone()
        if row is None:
            msg = "this station key is unknown or has been replaced; ask an administrator"
            raise AgentKeyError(msg)
        return str(row[0])

    def stations(self) -> tuple[str, ...]:
        """The stations with a working key, by name."""
        rows = self._conn.execute(
            """
            SELECT st.name FROM log.agent_key k
            JOIN log.station st ON st.id = k.station_id
            WHERE NOT EXISTS (SELECT 1 FROM log.agent_key_end e WHERE e.key_id = k.id)
            ORDER BY st.name
            """,
        ).fetchall()
        return tuple(str(r[0]) for r in rows)


__all__ = ["PREFIX", "AgentKeyError", "AgentKeyStore"]
