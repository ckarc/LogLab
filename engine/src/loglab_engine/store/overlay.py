# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Overlays made in the editor, kept by the Engine beside the ones it ships.

The API parses each definition into the strict overlay model before it
arrives here (Principle XII); the database checks that only a signed-in
administrator saves one (Principle XXI). Every save is a new version.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, cast

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.log import NotAuthorizedError, StaleVersionError

if TYPE_CHECKING:
    from datetime import datetime

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor


@dataclass(frozen=True, slots=True)
class SavedOverlay:
    """The current version of an overlay made in the editor."""

    overlay_id: str
    version: int
    retired: bool
    definition: dict[str, object] | None
    saved_at: datetime
    saved_by: str


class OverlayStore:
    """Reads and saves overlays made in the editor."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def current(self) -> tuple[SavedOverlay, ...]:
        """The latest version of every overlay made in the editor, retired ones included."""
        rows = self._conn.execute(
            """
            SELECT DISTINCT ON (v.overlay_id)
                   v.overlay_id, v.version, v.retired, v.definition, v.saved_at, o.callsign
            FROM workspace.overlay_version v
            JOIN log.session s ON s.id = v.session_id
            JOIN log.operator o ON o.id = s.operator_id
            ORDER BY v.overlay_id, v.version DESC
            """
        ).fetchall()
        return tuple(
            SavedOverlay(
                overlay_id=r[0],
                version=r[1],
                retired=r[2],
                definition=cast("dict[str, object] | None", r[3]),
                saved_at=r[4],
                saved_by=r[5],
            )
            for r in rows
        )

    def latest_definition(self, overlay_id: str) -> dict[str, object] | None:
        """The last definition saved for ``overlay_id`` before it was retired, if any."""
        row = self._conn.execute(
            """
            SELECT definition FROM workspace.overlay_version
            WHERE overlay_id = %s AND definition IS NOT NULL
            ORDER BY version DESC LIMIT 1
            """,
            (overlay_id,),
        ).fetchone()
        return None if row is None else cast("dict[str, object]", row[0])

    def save(
        self,
        actor: Actor,
        overlay_id: str,
        version: int,
        definition: dict[str, object] | None,
    ) -> None:
        """Save ``definition`` as ``version`` of ``overlay_id``; ``None`` retires it.

        Raises:
            NotAuthorizedError: The actor is not a signed-in administrator.
            StaleVersionError: Someone saved the overlay since ``version - 1``.
        """
        try:
            with self._conn.transaction():
                self._conn.execute(
                    """
                    INSERT INTO workspace.overlay_version
                        (overlay_id, version, retired, definition, session_id)
                    VALUES (%s, %s, %s, %s, %s)
                    """,
                    (
                        overlay_id,
                        version,
                        definition is None,
                        None if definition is None else Jsonb(definition),
                        actor.session_id,
                    ),
                )
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
        except (errors.SerializationFailure, errors.UniqueViolation) as error:
            msg = f"the overlay {overlay_id} was changed by someone else; open it again"
            raise StaleVersionError(msg) from error
