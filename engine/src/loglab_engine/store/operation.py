# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Operations: a club's use of one overlay, under one station callsign.

The API validates an operation's overlay and exchange before it arrives here
(Principle XII); the database checks that only a signed-in administrator
sets one up (Principle XXI). An operation is never erased: it is ended, and
once its log is submitted it can be archived, after which neither it nor its
QSOs ever change.
"""

from __future__ import annotations

import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING, cast

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.log import ArchivedError, NotAuthorizedError, StoreError

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
    starts_at: datetime | None = None
    ends_at: datetime | None = None
    archived_at: datetime | None = None
    practice: bool = False

    def open_at(self, when: datetime) -> bool:
        """Whether ``when`` falls within the operation's hours (open ends always do)."""
        return (self.starts_at is None or when >= self.starts_at) and (
            self.ends_at is None or when <= self.ends_at
        )

    def over(self, now: datetime) -> bool:
        """Whether the operation has been ended, or its end time has passed."""
        return self.ended or (self.ends_at is not None and now > self.ends_at)


_COLUMNS = (
    "id, overlay, name, station_call, gota_call, sent, ended, created_at, changed_at,"
    " starts_at, ends_at, archived_at, practice"
)


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
        starts_at=row[9],
        ends_at=row[10],
        archived_at=row[11],
        practice=row[12],
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
    starts_at: datetime | None = None
    ends_at: datetime | None = None
    # A practice operation is for trying LabLog: its QSOs never go to LoTW or
    # QRZ.com, and only it takes contacts with pretend stations. It is set
    # when the operation is made and can't change.
    practice: bool = False


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
        except errors.ObjectNotInPrerequisiteState as error:
            raise ArchivedError(str(error).splitlines()[0]) from error
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
                (id, overlay, name, station_call, gota_call, sent, ended, session_id,
                 starts_at, ends_at, practice)
            VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
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
                setup.starts_at,
                setup.ends_at,
                setup.practice,
            ),
        )

    def update(self, actor: Actor, operation_id: uuid.UUID, setup: OperationSetup) -> Operation:
        """Change an operation, or end it.

        Raises:
            ArchivedError: The operation is archived, or the change would
                make it a practice operation or stop it being one.
            NotAuthorizedError: The actor is not a signed-in administrator.
            OperationNotFoundError: There is no such operation.
        """
        return self._write(
            f"""
            UPDATE workspace.operation
            SET overlay = %s, name = %s, station_call = %s, gota_call = %s, sent = %s,
                ended = %s, session_id = %s, starts_at = %s, ends_at = %s, practice = %s
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
                setup.starts_at,
                setup.ends_at,
                setup.practice,
                operation_id,
            ),
        )

    def archive(self, actor: Actor, operation_id: uuid.UUID) -> Operation:
        """End an operation, if it is not already, and archive it, for good.

        From then on neither the operation nor any QSO logged in it can change.

        Raises:
            ArchivedError: The operation is already archived.
            NotAuthorizedError: The actor may not change this operation.
            OperationNotFoundError: There is no such operation.
        """
        return self._write(
            f"""
            UPDATE workspace.operation
            SET ended = true, archived_at = transaction_timestamp(), session_id = %s
            WHERE id = %s
            RETURNING {_COLUMNS}
            """,  # noqa: S608 - constant columns
            (actor.session_id, operation_id),
        )
