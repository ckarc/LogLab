# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Each operator's Logbook of the World set-up: certificate, login, station location.

A set-up belongs to its operator, and the database lets only that operator,
signed in, save, change, or remove it (Principle XIII). Nothing here touches a
QSO: what was sent and confirmed is written to the QSOs through the log's
audited path (Principle VII).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, LiteralString, cast

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.log import NotAuthorizedError

if TYPE_CHECKING:
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
