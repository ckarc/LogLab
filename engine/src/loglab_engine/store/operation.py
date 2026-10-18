# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operations: a club's use of one overlay, under one station callsign.

The API validates an operation's overlay and exchange before it arrives here
(Principle XII); the database checks that only a signed-in administrator
sets one up (Principle XIII). An operation is never erased: it is ended.
"""

from __future__ import annotations

import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING, cast

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.log import NotAuthorizedError, StoreError

if TYPE_CHECKING:
    from datetime import datetime

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor


class OperationNotFoundError(StoreError):
    """No operation has that ID."""


@dataclass(frozen=True, slots=True)
class Operation:
    """One operation: its overlay, its calls, and the exchange it sends."""

    id: uuid.UUID
    overlay: str
    name: str
    station_call: str
    gota_call: str | None
    sent: Mapping[str, str]
    ended: bool
    created_at: datetime
    changed_at: datetime


_COLUMNS = "id, overlay, name, station_call, gota_call, sent, ended, created_at, changed_at"


def _operation(row: TupleRow) -> Operation:
    sent = cast("dict[str, object]", row[5])
    return Operation(
        id=row[0],
        overlay=row[1],
        name=row[2],
        station_call=row[3],
        gota_call=row[4],
        sent={str(k): str(v) for k, v in sent.items()},
        ended=row[6],
        created_at=row[7],
        changed_at=row[8],
    )


@dataclass(frozen=True, slots=True)
class OperationSetup:
    """What an administrator sets for an operation."""

    overlay: str
    name: str
    station_call: str
    gota_call: str | None
    sent: Mapping[str, str]
    ended: bool = False


class OperationStore:
    """Reads and saves operations."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def list(self, *, include_ended: bool = False) -> tuple[Operation, ...]:
        """Every operation, the newest first; ended ones only with ``include_ended``."""
        rows = self._conn.execute(
            f"SELECT {_COLUMNS} FROM workspace.operation "  # noqa: S608 - constant columns
            "WHERE %s OR NOT ended ORDER BY created_at DESC, id",
            (include_ended,),
        ).fetchall()
        return tuple(_operation(r) for r in rows)

    def get(self, operation_id: uuid.UUID) -> Operation:
        """The operation with ``operation_id``.

        Raises:
            OperationNotFoundError: There is none.
        """
        row = self._conn.execute(
            f"SELECT {_COLUMNS} FROM workspace.operation WHERE id = %s",  # noqa: S608
            (operation_id,),
        ).fetchone()
        if row is None:
            raise OperationNotFoundError(f"no operation has ID {operation_id}")
        return _operation(row)

    def _write(self, sql: str, params: tuple[object, ...]) -> Operation:
        try:
            with self._conn.transaction():
                row = self._conn.execute(sql, params).fetchone()
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
        if row is None:
            raise OperationNotFoundError("no such operation")
        return _operation(row)

    def create(self, actor: Actor, setup: OperationSetup) -> Operation:
        """Set up a new operation.

        Raises:
            NotAuthorizedError: The actor is not a signed-in administrator.
        """
        return self._write(
            f"""
            INSERT INTO workspace.operation
                (id, overlay, name, station_call, gota_call, sent, ended, session_id)
            VALUES (%s, %s, %s, %s, %s, %s, %s, %s)
            RETURNING {_COLUMNS}
            """,  # noqa: S608 - constant columns
            (
                uuid.uuid4(),
                setup.overlay,
                setup.name.strip(),
                setup.station_call.upper(),
                setup.gota_call.upper() if setup.gota_call else None,
                Jsonb(dict(setup.sent)),
                setup.ended,
                actor.session_id,
            ),
        )

    def update(self, actor: Actor, operation_id: uuid.UUID, setup: OperationSetup) -> Operation:
        """Change an operation, or end it.

        Raises:
            NotAuthorizedError: The actor is not a signed-in administrator.
            OperationNotFoundError: There is no such operation.
        """
        return self._write(
            f"""
            UPDATE workspace.operation
            SET overlay = %s, name = %s, station_call = %s, gota_call = %s, sent = %s,
                ended = %s, session_id = %s
            WHERE id = %s
            RETURNING {_COLUMNS}
            """,  # noqa: S608 - constant columns
            (
                setup.overlay,
                setup.name.strip(),
                setup.station_call.upper(),
                setup.gota_call.upper() if setup.gota_call else None,
                Jsonb(dict(setup.sent)),
                setup.ended,
                actor.session_id,
                operation_id,
            ),
        )
