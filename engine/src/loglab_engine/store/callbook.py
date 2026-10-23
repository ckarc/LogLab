# SPDX-FileCopyrightText: 2026 LogLab contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Callbook storage: the cache of looked-up calls, and each operator's callbook login.

The cache is reference data, kept apart from the log (Principle VI); writing
it can never touch a QSO. A login belongs to its operator, and the database
lets only that operator, signed in, save, change, or remove it (Principle XIII).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, LiteralString, cast

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.log import NotAuthorizedError

if TYPE_CHECKING:
    from datetime import datetime, timedelta

    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.store.log import Actor


@dataclass(frozen=True, slots=True)
class CachedEntry:
    """A callbook's answer about a call, as it was when fetched."""

    found: bool
    entry: dict[str, object]
    fetched_at: datetime


@dataclass(frozen=True, slots=True)
class CachedMedia:
    """A callbook's biography (plain text) or photo of a call, as it was when fetched.

    ``origin`` is where a photo came from; a biography has none.
    """

    content: bytes
    content_type: str
    origin: str | None
    fetched_at: datetime


@dataclass(frozen=True, slots=True)
class RefreshSetting:
    """When kept profiles are re-checked in the background, and who last said so."""

    mode: str
    changed_by: str | None
    changed_at: datetime | None


@dataclass(frozen=True, slots=True)
class CallbookLogin:
    """An operator's login to a callbook, and its current session key if any."""

    username: str
    password: str
    service_key: str | None


class CallbookStore:
    """Reads and writes the callbook cache and operators' callbook logins."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def cached(self, source: str, call: str) -> CachedEntry | None:
        """What ``source`` said about ``call`` when it was last fetched, if ever."""
        row = self._conn.execute(
            "SELECT found, entry, fetched_at FROM reference.callbook_entry"
            " WHERE source = %s AND call = %s",
            (source, call),
        ).fetchone()
        if row is None:
            return None
        return CachedEntry(row[0], cast("dict[str, object]", row[1]), row[2])

    def remember(
        self, source: str, call: str, *, found: bool, entry: dict[str, str]
    ) -> CachedEntry:
        """Keep what ``source`` said about ``call``, replacing any older answer."""
        with self._conn.transaction():
            row = self._conn.execute(
                """
                INSERT INTO reference.callbook_entry (source, call, found, entry)
                VALUES (%s, %s, %s, %s)
                ON CONFLICT (source, call)
                DO UPDATE SET found = EXCLUDED.found, entry = EXCLUDED.entry
                RETURNING found, entry, fetched_at
                """,
                (source, call, found, Jsonb(entry)),
            ).fetchone()
        assert row is not None  # noqa: S101 - RETURNING always returns the row
        return CachedEntry(row[0], cast("dict[str, object]", row[1]), row[2])

    def media(self, source: str, call: str, kind: str) -> CachedMedia | None:
        """The ``kind`` (``bio`` or ``photo``) ``source`` gave for ``call``, if ever fetched."""
        row = self._conn.execute(
            "SELECT content, content_type, origin, fetched_at FROM reference.callbook_media"
            " WHERE source = %s AND call = %s AND kind = %s",
            (source, call, kind),
        ).fetchone()
        return None if row is None else CachedMedia(bytes(row[0]), row[1], row[2], row[3])

    def remember_media(  # noqa: PLR0913 - one row's columns
        self,
        source: str,
        call: str,
        kind: str,
        *,
        content: bytes,
        content_type: str,
        origin: str | None,
    ) -> CachedMedia:
        """Keep a biography or photo of ``call``, replacing any older one."""
        with self._conn.transaction():
            row = self._conn.execute(
                """
                INSERT INTO reference.callbook_media
                    (source, call, kind, origin, content_type, content)
                VALUES (%s, %s, %s, %s, %s, %s)
                ON CONFLICT (source, call, kind) DO UPDATE SET
                    origin = EXCLUDED.origin, content_type = EXCLUDED.content_type,
                    content = EXCLUDED.content
                RETURNING content, content_type, origin, fetched_at
                """,
                (source, call, kind, origin, content_type, content),
            ).fetchone()
        assert row is not None  # noqa: S101 - RETURNING always returns the row
        return CachedMedia(bytes(row[0]), row[1], row[2], row[3])

    def kept_profiles_due(self, source: str, older_than: timedelta, limit: int) -> list[str]:
        """Calls with a kept biography or photo whose entry was fetched over ``older_than`` ago.

        The longest-unchecked come first.
        """
        rows = self._conn.execute(
            """
            SELECT e.call FROM reference.callbook_entry e
            WHERE e.source = %s AND e.found
              AND e.fetched_at < transaction_timestamp() - %s
              AND EXISTS (SELECT 1 FROM reference.callbook_media m
                          WHERE m.source = e.source AND m.call = e.call)
            ORDER BY e.fetched_at, e.call
            LIMIT %s
            """,
            (source, older_than, limit),
        ).fetchall()
        return [row[0] for row in rows]

    def accepted_login(self, source: str) -> tuple[str, str] | None:
        """A username and password the callbook accepted when last tried: the earliest saved.

        For the Engine's own background work, which has no operator signed in.
        """
        row = self._conn.execute(
            """
            SELECT username, password FROM workspace.callbook_login
            WHERE source = %s AND service_key IS NOT NULL
            ORDER BY saved_at, operator_id LIMIT 1
            """,
            (source,),
        ).fetchone()
        return None if row is None else (row[0], row[1])

    def refresh_setting(self) -> RefreshSetting:
        """When kept profiles are re-checked; ``auto`` if no administrator has said."""
        row = self._conn.execute(
            """
            SELECT r.mode, o.callsign, r.changed_at FROM workspace.callbook_refresh r
            JOIN log.operator o ON o.id = r.operator_id
            """
        ).fetchone()
        return RefreshSetting("auto", None, None) if row is None else RefreshSetting(*row)

    def save_refresh_mode(self, actor: Actor, mode: str) -> None:
        """Set when kept profiles are re-checked.

        Raises:
            NotAuthorizedError: The session is not live, or its operator is not an administrator.
        """
        self._write(
            """
            INSERT INTO workspace.callbook_refresh (mode, operator_id, session_id)
            SELECT %(mode)s, operator_id, id FROM log.session WHERE id = %(session)s
            ON CONFLICT (one) DO UPDATE SET
                mode = EXCLUDED.mode, operator_id = EXCLUDED.operator_id,
                session_id = EXCLUDED.session_id
            """,
            {"mode": mode, "session": actor.session_id},
        )

    def login(self, actor: Actor, source: str) -> CallbookLogin | None:
        """The signed-in operator's login to ``source``, or ``None`` if they have none."""
        row = self._conn.execute(
            """
            SELECT l.username, l.password, l.service_key FROM workspace.callbook_login l
            JOIN log.session s ON s.operator_id = l.operator_id
            WHERE s.id = %s AND l.source = %s
            """,
            (actor.session_id, source),
        ).fetchone()
        return None if row is None else CallbookLogin(row[0], row[1], row[2])

    def save_login(self, actor: Actor, source: str, username: str, password: str) -> None:
        """Save the signed-in operator's login to ``source``, replacing any earlier one.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        self._write(
            """
            INSERT INTO workspace.callbook_login
                (operator_id, source, session_id, username, password)
            SELECT operator_id, %(source)s, id, %(username)s, %(password)s
            FROM log.session WHERE id = %(session)s
            ON CONFLICT (operator_id, source) DO UPDATE SET
                session_id = EXCLUDED.session_id, username = EXCLUDED.username,
                password = EXCLUDED.password, service_key = NULL
            """,
            {
                "source": source,
                "username": username,
                "password": password,
                "session": actor.session_id,
            },
        )

    def save_key(self, actor: Actor, source: str, key: str | None) -> None:
        """Keep the callbook's session key for the signed-in operator's login.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        self._write(
            """
            UPDATE workspace.callbook_login l SET service_key = %(key)s, session_id = s.id
            FROM log.session s
            WHERE s.id = %(session)s AND l.operator_id = s.operator_id AND l.source = %(source)s
            """,
            {"key": key, "session": actor.session_id, "source": source},
        )

    def remove_login(self, actor: Actor, source: str) -> bool:
        """Remove the signed-in operator's login to ``source``; ``False`` if there was none.

        Raises:
            NotAuthorizedError: The session is not live.
        """
        params = {"session": actor.session_id, "source": source}
        try:
            with self._conn.transaction():
                # Claim the row for this session first: only its own session may remove it.
                claimed = self._conn.execute(
                    """
                    UPDATE workspace.callbook_login l SET session_id = s.id
                    FROM log.session s
                    WHERE s.id = %(session)s AND l.operator_id = s.operator_id
                        AND l.source = %(source)s
                    """,
                    params,
                ).rowcount
                self._conn.execute(
                    """
                    DELETE FROM workspace.callbook_login
                    WHERE session_id = %(session)s AND source = %(source)s
                    """,
                    params,
                )
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
        return claimed > 0

    def _write(self, statement: LiteralString, params: dict[str, object]) -> None:
        try:
            with self._conn.transaction():
                self._conn.execute(statement, params)
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(str(error).splitlines()[0]) from error
