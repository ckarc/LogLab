# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Each operator's workspace layout, kept by the Engine so it follows them anywhere.

The layout is opaque here: the API validates its shape before it arrives
(Principle XII), and the database checks that only its own signed-in operator
saves it (Principle XXI). Every save is kept; the latest is current.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, cast

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.log import NotAuthorizedError

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor


class WorkspaceStore:
    """Reads and saves operators' layouts."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def layout(self, actor: Actor) -> dict[str, object] | None:
        """The signed-in operator's latest layout, or ``None`` if they never saved one."""
        row = self._conn.execute(
            """
            SELECT l.layout FROM workspace.layout l
            JOIN log.session s ON s.operator_id = l.operator_id
            WHERE s.id = %s
            ORDER BY l.id DESC LIMIT 1
            """,
            (actor.session_id,),
        ).fetchone()
        return None if row is None else cast("dict[str, object]", row[0])

    def save_layout(self, actor: Actor, layout: dict[str, object]) -> None:
        """Save ``layout`` as the signed-in operator's current layout.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        try:
            with self._conn.transaction():
                self._conn.execute(
                    """
                    INSERT INTO workspace.layout (operator_id, session_id, layout)
                    SELECT operator_id, id, %s FROM log.session WHERE id = %s
                    """,
                    (Jsonb(layout), actor.session_id),
                )
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
