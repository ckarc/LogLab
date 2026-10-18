# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""QRZ logbook keys, and what was sent to each QRZ logbook.

A key belongs to the operator who saved it and serves one logbook callsign;
the database lets only that operator, signed in, change it (Principle XIII).
A QSO goes to the logbook for its STATION_CALLSIGN, or, when it has none, for
the operator who first logged it. Only QSOs entered after the key was saved
are sent. Nothing here writes to the log.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING, Final

from psycopg import errors

from loglab_engine.store.log import NotAuthorizedError, StoreError

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor

MAX_BATCH: Final = 50

# A QSO's logbook callsign: its STATION_CALLSIGN, else who first logged it.
_BOOK_CALL: Final = """
    coalesce(nullif(upper(btrim(log.field_value(v.fields, 'STATION_CALLSIGN'))), ''), o.callsign)
"""
_PENDING: Final = f"""
    FROM log.qso q
    JOIN log.qso_version v ON v.qso_id = q.id AND v.version = q.current_version
    JOIN log.qso_version f ON f.qso_id = q.id AND f.version = 1
    JOIN log.changeset c ON c.id = f.changeset_id
    JOIN log.operator o ON o.id = c.operator_id
    LEFT JOIN workspace.qrz_upload u ON u.qso_id = q.id
    WHERE q.entry_seq > %(from_seq)s
      AND {_BOOK_CALL} = %(book)s
      AND (u.qso_id IS NULL OR u.version < q.current_version)
      AND NOT (q.deleted AND u.logid IS NULL)
"""


class LogbookTakenError(StoreError):
    """Another operator already saved a key for that logbook callsign."""


@dataclass(frozen=True, slots=True)
class Logbook:
    """A QRZ logbook LogLab sends QSOs to."""

    book_call: str
    api_key: str
    from_entry_seq: int
    operator: str


@dataclass(frozen=True, slots=True)
class PendingQso:
    """A QSO whose current version QRZ.com does not have yet."""

    qso_id: uuid.UUID
    version: int
    deleted: bool
    logid: int | None


@dataclass(frozen=True, slots=True)
class LogbookCounts:
    """How a logbook's QSOs stand: sent, waiting to go, and refused by QRZ.com."""

    sent: int
    waiting: int
    refused: int
    last_refusal: str | None


class LogbookStore:
    """Reads and writes QRZ logbook keys and the record of what was sent."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def mine(self, actor: Actor) -> Logbook | None:
        """The logbook the signed-in operator saved a key for, if any."""
        row = self._conn.execute(
            """
            SELECT b.book_call, b.api_key, b.from_entry_seq, o.callsign
            FROM workspace.qrz_logbook b
            JOIN log.session s ON s.operator_id = b.operator_id
            JOIN log.operator o ON o.id = b.operator_id
            WHERE s.id = %s
            """,
            (actor.session_id,),
        ).fetchone()
        return None if row is None else Logbook(row[0], row[1], row[2], row[3])

    def all(self) -> list[Logbook]:
        """Every logbook with a key, by callsign."""
        rows = self._conn.execute(
            """
            SELECT b.book_call, b.api_key, b.from_entry_seq, o.callsign
            FROM workspace.qrz_logbook b JOIN log.operator o ON o.id = b.operator_id
            ORDER BY b.book_call
            """,
        ).fetchall()
        return [Logbook(*row) for row in rows]

    def save(self, actor: Actor, api_key: str, book_call: str) -> None:
        """Save the signed-in operator's key for ``book_call``, replacing their earlier one.

        Raises:
            LogbookTakenError: Another operator has a key for ``book_call``.
            NotAuthorizedError: The session is not live.
        """
        try:
            with self._conn.transaction():
                self._conn.execute(
                    """
                    INSERT INTO workspace.qrz_logbook
                        (operator_id, session_id, api_key, book_call)
                    SELECT operator_id, id, %(key)s, %(book)s
                    FROM log.session WHERE id = %(session)s
                    ON CONFLICT (operator_id) DO UPDATE SET
                        session_id = EXCLUDED.session_id, api_key = EXCLUDED.api_key,
                        book_call = EXCLUDED.book_call
                    """,
                    {"key": api_key, "book": book_call, "session": actor.session_id},
                )
        except errors.UniqueViolation as error:
            msg = f"another operator already sends QSOs to {book_call}'s QRZ logbook"
            raise LogbookTakenError(msg) from error
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error

    def remove(self, actor: Actor) -> bool:
        """Remove the signed-in operator's key; ``False`` if they had none.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        params = {"session": actor.session_id}
        try:
            with self._conn.transaction():
                # Claim the row for this session first: only its own session may remove it.
                claimed = self._conn.execute(
                    """
                    UPDATE workspace.qrz_logbook b SET session_id = s.id
                    FROM log.session s
                    WHERE s.id = %(session)s AND b.operator_id = s.operator_id
                    """,
                    params,
                ).rowcount
                self._conn.execute(
                    "DELETE FROM workspace.qrz_logbook WHERE session_id = %(session)s", params
                )
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
        return claimed > 0

    def pending(self, book: Logbook, limit: int = MAX_BATCH) -> list[PendingQso]:
        """QSOs for ``book`` whose current version QRZ.com does not have, oldest first."""
        rows = self._conn.execute(
            f"SELECT q.id, q.current_version, q.deleted, u.logid {_PENDING}"
            " ORDER BY q.entry_seq LIMIT %(limit)s",
            {"from_seq": book.from_entry_seq, "book": book.book_call, "limit": limit},
        ).fetchall()
        return [PendingQso(uuid.UUID(str(r[0])), r[1], r[2], r[3]) for r in rows]

    def record(
        self,
        qso: PendingQso,
        book: Logbook,
        *,
        outcome: str,
        logid: int | None,
        message: str | None = None,
    ) -> None:
        """Keep what QRZ.com did with ``qso`` at its pending version."""
        with self._conn.transaction():
            self._conn.execute(
                """
                INSERT INTO workspace.qrz_upload
                    (qso_id, book_call, version, logid, outcome, message)
                VALUES (%s, %s, %s, %s, %s, %s)
                ON CONFLICT (qso_id) DO UPDATE SET
                    book_call = EXCLUDED.book_call, version = EXCLUDED.version,
                    logid = EXCLUDED.logid, outcome = EXCLUDED.outcome, message = EXCLUDED.message
                """,
                (qso.qso_id, book.book_call, qso.version, logid, outcome, message),
            )

    def counts(self, book: Logbook) -> LogbookCounts:
        """How ``book``'s QSOs stand."""
        row = self._conn.execute(
            """
            SELECT count(*) FILTER (WHERE outcome = 'sent'),
                   count(*) FILTER (WHERE outcome = 'refused'),
                   (SELECT message FROM workspace.qrz_upload
                    WHERE book_call = %(book)s AND outcome = 'refused'
                    ORDER BY done_at DESC LIMIT 1)
            FROM workspace.qrz_upload WHERE book_call = %(book)s
            """,
            {"book": book.book_call},
        ).fetchone()
        waiting = self._conn.execute(
            f"SELECT count(*) {_PENDING}",
            {"from_seq": book.from_entry_seq, "book": book.book_call},
        ).fetchone()
        assert row is not None  # noqa: S101 - an aggregate always returns a row
        assert waiting is not None  # noqa: S101 - an aggregate always returns a row
        return LogbookCounts(row[0], waiting[0], row[1], row[2])
