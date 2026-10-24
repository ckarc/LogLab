# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Each operator's Logbook of the World set-up, and the batches they send to LoTW.

A set-up belongs to its operator, and the database lets only that operator,
signed in, save, change, or remove it (Principle XXI). Nothing here touches a
QSO: what was sent and confirmed is written to the QSOs through the log's
audited path (Principle VII).
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING, Final, LiteralString, cast

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.log import NotAuthorizedError

if TYPE_CHECKING:
    from collections.abc import Iterable
    from datetime import datetime

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor


@dataclass(frozen=True, slots=True)
class LotwAccount:
    """What an operator has saved for LoTW; any part may be missing."""

    certificate: bytes | None
    certificate_password: str | None
    username: str | None
    password: str | None
    location: dict[str, object] | None
    qsl_since: str | None


class LotwAccountStore:
    """Reads and writes the signed-in operator's LoTW set-up."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def get(self, actor: Actor) -> LotwAccount | None:
        """The signed-in operator's set-up, if they saved one."""
        row = self._conn.execute(
            """
            SELECT a.certificate, a.certificate_password, a.username, a.password,
                   a.location, a.qsl_since
            FROM workspace.lotw_account a JOIN log.session s ON s.operator_id = a.operator_id
            WHERE s.id = %s
            """,
            (actor.session_id,),
        ).fetchone()
        if row is None:
            return None
        cert, cert_password, username, password, location, since = row
        return LotwAccount(
            None if cert is None else bytes(cert),
            cert_password,
            username,
            password,
            None if location is None else cast("dict[str, object]", location),
            since,
        )

    def save_certificate(self, actor: Actor, p12: bytes, password: str) -> None:
        """Keep the operator's certificate file and its password, replacing any earlier one.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        self._upsert(
            actor,
            "certificate = EXCLUDED.certificate,"
            " certificate_password = EXCLUDED.certificate_password",
            {"certificate": p12, "certificate_password": password},
        )

    def save_login(self, actor: Actor, username: str, password: str) -> None:
        """Keep the operator's LoTW website login; a new login fetches confirmations afresh.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        self._upsert(
            actor,
            "username = EXCLUDED.username, password = EXCLUDED.password, qsl_since = NULL",
            {"username": username, "password": password},
        )

    def save_location(self, actor: Actor, location: dict[str, object]) -> None:
        """Keep the operator's station location.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        self._upsert(actor, "location = EXCLUDED.location", {"location": Jsonb(location)})

    def save_qsl_since(self, actor: Actor, since: str) -> None:
        """Remember the date to fetch confirmations from next time.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        self._write(
            """
            UPDATE workspace.lotw_account a SET qsl_since = %(since)s, session_id = s.id
            FROM log.session s WHERE s.id = %(session)s AND a.operator_id = s.operator_id
            """,
            {"since": since, "session": actor.session_id},
        )

    def remove(self, actor: Actor) -> bool:
        """Remove the operator's whole LoTW set-up; ``False`` if there was none.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        params = {"session": actor.session_id}
        try:
            with self._conn.transaction():
                # Claim the row for this session first: only its own session may remove it.
                claimed = self._conn.execute(
                    """
                    UPDATE workspace.lotw_account a SET session_id = s.id
                    FROM log.session s
                    WHERE s.id = %(session)s AND a.operator_id = s.operator_id
                    """,
                    params,
                ).rowcount
                self._conn.execute(
                    "DELETE FROM workspace.lotw_account WHERE session_id = %(session)s", params
                )
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
        return claimed > 0

    def _upsert(self, actor: Actor, update: LiteralString, values: dict[str, object]) -> None:
        columns = {
            "certificate": None,
            "certificate_password": None,
            "username": None,
            "password": None,
            "location": None,
        }
        self._write(
            """
            INSERT INTO workspace.lotw_account
                (operator_id, session_id, certificate, certificate_password,
                 username, password, location)
            SELECT operator_id, id, %(certificate)s, %(certificate_password)s,
                   %(username)s, %(password)s, %(location)s
            FROM log.session WHERE id = %(session)s
            ON CONFLICT (operator_id) DO UPDATE SET session_id = EXCLUDED.session_id, """  # noqa: S608 - callers pass constant SET clauses; values are parameters
            + update,
            {**columns, **values, "session": actor.session_id},
        )

    def _write(self, statement: LiteralString, params: dict[str, object]) -> None:
        try:
            with self._conn.transaction():
                self._conn.execute(statement, params)
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error


@dataclass(frozen=True, slots=True)
class BatchQso:
    """One QSO in a LoTW batch, as it was when the batch was prepared."""

    qso_id: uuid.UUID
    version: int
    call: str | None
    qso_start: datetime | None
    band: str | None
    mode: str | None
    sent: bool


@dataclass(frozen=True, slots=True)
class LotwBatch:
    """A batch of QSOs for LoTW: prepared, then sent or cancelled by its operator."""

    id: int
    station_call: str
    state: str
    prepared_at: datetime
    finished_at: datetime | None
    answer: str | None
    count: int
    sent: int


_BATCH_COLUMNS: Final = """
    b.id, b.station_call, b.state, b.prepared_at, b.finished_at, b.answer,
    (SELECT count(*) FROM workspace.lotw_batch_qso i WHERE i.batch_id = b.id),
    (SELECT count(*) FROM workspace.lotw_batch_qso i WHERE i.batch_id = b.id AND i.sent)
"""
_MINE: Final = "b.operator_id = (SELECT operator_id FROM log.session WHERE id = %(session)s)"


class LotwBatchStore:
    """Reads and writes the signed-in operator's LoTW batches.

    The database lets only a batch's own operator, signed in, prepare, send or
    cancel it, and keeps every batch (Principle XXI).
    """

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def ready(self, actor: Actor) -> LotwBatch | None:
        """The operator's batch waiting for review, if there is one."""
        found = self._find(f"{_MINE} AND b.state = 'ready'", {"session": actor.session_id}, 1)
        return found[0] if found else None

    def get(self, actor: Actor, batch_id: int) -> LotwBatch | None:
        """The operator's batch ``batch_id``; ``None`` if they have no such batch."""
        found = self._find(
            f"{_MINE} AND b.id = %(id)s", {"session": actor.session_id, "id": batch_id}, 1
        )
        return found[0] if found else None

    def recent(self, actor: Actor, limit: int) -> tuple[LotwBatch, ...]:
        """The operator's latest batches, newest first."""
        return self._find(_MINE, {"session": actor.session_id}, limit)

    def qsos(self, batch_id: int) -> tuple[BatchQso, ...]:
        """The QSOs in batch ``batch_id``, as they were when it was prepared, oldest first."""
        rows = self._conn.execute(
            """
            SELECT i.qso_id, i.version, v.call, v.qso_start, v.band, v.mode, i.sent
            FROM workspace.lotw_batch_qso i
            JOIN log.qso_version v ON v.qso_id = i.qso_id AND v.version = i.version
            JOIN log.qso q ON q.id = i.qso_id
            WHERE i.batch_id = %s
            ORDER BY q.entry_seq
            """,
            (batch_id,),
        ).fetchall()
        return tuple(BatchQso(uuid.UUID(str(r[0])), *r[1:]) for r in rows)

    def create(self, actor: Actor, station_call: str, qsos: Iterable[tuple[uuid.UUID, int]]) -> int:
        """Prepare a batch of ``qsos`` (ID and version) to send as ``station_call``.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        try:
            with self._conn.transaction():
                row = self._conn.execute(
                    """
                    INSERT INTO workspace.lotw_batch (operator_id, session_id, station_call)
                    SELECT operator_id, id, %s FROM log.session WHERE id = %s
                    RETURNING id
                    """,
                    (station_call, actor.session_id),
                ).fetchone()
                if row is None:
                    raise NotAuthorizedError("the session is not signed in")
                batch_id = int(row[0])
                with self._conn.cursor() as cur:
                    cur.executemany(
                        "INSERT INTO workspace.lotw_batch_qso (batch_id, qso_id, version)"
                        " VALUES (%s, %s, %s)",
                        [(batch_id, qso_id, version) for qso_id, version in qsos],
                    )
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
        return batch_id

    def note(self, actor: Actor, batch_id: int, answer: str) -> None:
        """Keep what LoTW last said about a batch that is still waiting.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        self._finish(actor, batch_id, "ready", answer, ())

    def mark_sent(
        self, actor: Actor, batch_id: int, answer: str, sent: Iterable[uuid.UUID]
    ) -> None:
        """Record that LoTW took batch ``batch_id``'s ``sent`` QSOs, and what it said.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        self._finish(actor, batch_id, "sent", answer, tuple(sent))

    def cancel(self, actor: Actor, batch_id: int, answer: str | None = None) -> bool:
        """Cancel the operator's waiting batch; ``False`` if it is not waiting.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        return self._finish(actor, batch_id, "cancelled", answer, ())

    def _finish(
        self,
        actor: Actor,
        batch_id: int,
        state: str,
        answer: str | None,
        sent: tuple[uuid.UUID, ...],
    ) -> bool:
        params = {"session": actor.session_id, "id": batch_id}
        try:
            with self._conn.transaction():
                if sent:
                    self._conn.execute(
                        "UPDATE workspace.lotw_batch_qso SET sent = true"
                        " WHERE batch_id = %s AND qso_id = ANY(%s)",
                        (batch_id, list(sent)),
                    )
                changed = self._conn.execute(
                    f"""
                    UPDATE workspace.lotw_batch b
                    SET state = %(state)s, answer = coalesce(%(answer)s, b.answer),
                        session_id = %(session)s
                    WHERE b.id = %(id)s AND b.state = 'ready' AND {_MINE}
                    """,  # noqa: S608 - constant fragments only; values are parameters
                    {**params, "state": state, "answer": answer},
                ).rowcount
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
        return changed > 0

    def _find(
        self, where: LiteralString, params: dict[str, object], limit: int
    ) -> tuple[LotwBatch, ...]:
        rows = self._conn.execute(
            f"SELECT {_BATCH_COLUMNS} FROM workspace.lotw_batch b WHERE {where}"  # noqa: S608 - constant fragments only; values are parameters
            " ORDER BY b.id DESC LIMIT %(limit)s",
            {**params, "limit": limit},
        ).fetchall()
        return tuple(LotwBatch(*row) for row in rows)
