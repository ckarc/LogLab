# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Lab profiles: each radio model's default setup for each program.

An enrolled radio gets a program's profile as that program starts
(Principle X, second exception). The Agent has one built in for each; the
newest profile saved here takes its place.

Only a signed-in administrator **saves** one, in LabLog, from the values a
station's radio has now (:mod:`loglab_engine.profile_reads`), choosing which
of its settings the profile holds. Saving is the administrator's decision,
made against the profile they were shown: if that changed meanwhile,
nothing is saved. Every version is kept (Principle VII), and going back to
an earlier one saves it again as a new version (migration
``0023_lab_profile``, enforced by the database whatever code asks).
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import TYPE_CHECKING, Final

from psycopg import errors
from psycopg.types.json import Jsonb

from loglab_engine.store.lab_stock import StockSetting, check_settings
from loglab_engine.store.log import NotAuthorizedError, StoreError, check_station

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

    from loglab_engine.profile_reads import RadioRead
    from loglab_engine.store.log import Actor

# The programs the Agent puts a profile on for, by the names it uses.
PROGRAMS: Final = ("VARA", "FT8", "JS8", "fldigi", "SSTV")


class LabProfileNotFoundError(StoreError):
    """No lab profile has that number."""


class LabProfileMovedError(StoreError):
    """The profile changed since the one the administrator compared against."""


@dataclass(frozen=True, slots=True)
class LabProfile:
    """One version of a model's profile for a program."""

    id: int
    maker: str
    model: str
    program: str
    settings: tuple[StockSetting, ...]
    note: str | None
    # The station and radio it was read from, if it was read.
    station: str | None
    serial: str | None
    # The version it is a copy of, if an earlier one was saved again.
    again_of: int | None
    # The version it replaced; None: the Agent's built-in one.
    replaces: int | None
    saved_by: str
    saved_at: datetime
    # Whether it is the model's profile for the program now.
    current: bool


def _check_program(program: str) -> None:
    if program not in PROGRAMS:
        msg = f"a lab profile is for one of {', '.join(PROGRAMS)}"
        raise StoreError(msg)


def _json(settings: tuple[StockSetting, ...]) -> Jsonb:
    return Jsonb(
        [{"id": s.id, "label": s.label, "bytes": s.bytes, "shown": s.shown} for s in settings]
    )


class LabProfileStore:
    """Saving and finding lab profiles, on one autocommit connection."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn`` for every operation; each method is one transaction."""
        self._conn = conn

    def save(  # noqa: PLR0913 - each is a separate fact about the new version
        self,
        admin: Actor,
        station: str,
        read: RadioRead,
        program: str,
        ids: tuple[str, ...],
        *,
        against: int | None,
        note: str | None = None,
    ) -> LabProfile:
        """Make the settings ``ids``, as ``station``'s radio has them, ``program``'s profile.

        ``against`` is the profile the administrator compared with (None: the
        Agent's built-in one); if the model's profile for the program has
        changed since, nothing is saved.

        Raises:
            StoreError: The settings or note are not valid, or a setting
                chosen isn't one the radio was read for.
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
            LabProfileMovedError: The profile is not ``against`` any more.
        """
        check_station(station)
        _check_program(program)
        read_now = {s.id: s for s in read.settings}
        missing = [i for i in ids if i not in read_now]
        if missing:
            msg = f"the radio wasn't read for {', '.join(missing)}, so they can't be in a profile"
            raise StoreError(msg)
        chosen = tuple(s for s in read.settings if s.id in ids)
        check_settings(read.maker, read.model, read.serial, note, chosen)
        return self._insert(
            admin,
            read.maker,
            read.model,
            program,
            chosen,
            note=note,
            against=against,
            source=(station, read.serial, None),
        )

    def again(self, admin: Actor, profile_id: int, *, against: int | None) -> LabProfile:
        """Save an earlier version again, making it the profile once more.

        Raises:
            NotAuthorizedError: ``admin`` is not a signed-in administrator.
            LabProfileNotFoundError: No such version.
            LabProfileMovedError: The profile is not ``against`` any more.
        """
        earlier = self.get(profile_id)
        return self._insert(
            admin,
            earlier.maker,
            earlier.model,
            earlier.program,
            earlier.settings,
            note=f"version #{profile_id} again",
            against=against,
            source=(None, None, profile_id),
        )

    def _insert(  # noqa: PLR0913 - each is a separate fact about the new version
        self,
        admin: Actor,
        maker: str,
        model: str,
        program: str,
        settings: tuple[StockSetting, ...],
        *,
        note: str | None,
        against: int | None,
        source: tuple[str | None, str | None, int | None],
    ) -> LabProfile:
        station, serial, again_of = source
        conn = self._conn
        try:
            with conn.transaction():
                # One save at a time for a model's program, so two
                # administrators can't each replace the same version.
                conn.execute(
                    "SELECT pg_advisory_xact_lock(hashtext('lab_profile'), hashtext(%s))",
                    (f"{maker}\n{model}\n{program}",),
                )
                now = self._current_id(maker, model, program)
                if now != against:
                    msg = (
                        f"the {maker} {model} {program} profile changed since this was compared "
                        f"(it is now {'#' + str(now) if now else 'the built-in one'}); look again"
                    )
                    raise LabProfileMovedError(msg)
                if station is not None:
                    conn.execute(
                        "INSERT INTO log.station (name) VALUES (%s) ON CONFLICT DO NOTHING",
                        (station,),
                    )
                row = conn.execute(
                    """
                    INSERT INTO log.lab_profile (maker, model, program, settings, note,
                        station_id, serial, again_of, replaces, saved_by, session_id)
                    SELECT %s, %s, %s, %s, %s,
                        (SELECT st.id FROM log.station st WHERE st.name = %s), %s, %s, %s,
                        s.operator_id, s.id
                    FROM log.session s WHERE s.id = %s
                    RETURNING id
                    """,
                    (
                        maker,
                        model,
                        program,
                        _json(settings),
                        note,
                        station,
                        serial,
                        again_of,
                        now,
                        admin.session_id,
                    ),
                ).fetchone()
                if row is None:
                    msg = "only a signed-in administrator can change a lab profile"
                    raise NotAuthorizedError(msg)
        except errors.InsufficientPrivilege as error:
            raise NotAuthorizedError(error.diag.message_primary or str(error)) from error
        return self.get(int(row[0]))

    def current(self, maker: str, model: str) -> tuple[LabProfile, ...]:
        """The model's profile for each program that has one saved here."""
        return tuple(
            p for p in self._profiles("p.maker = %s AND p.model = %s", (maker, model)) if p.current
        )

    def get(self, profile_id: int) -> LabProfile:
        """One version.

        Raises:
            LabProfileNotFoundError: No such version.
        """
        found = self._profiles("p.id = %s", (profile_id,))
        if not found:
            msg = f"no lab profile #{profile_id}"
            raise LabProfileNotFoundError(msg)
        return found[0]

    def all(self) -> tuple[LabProfile, ...]:
        """Every version, newest first."""
        return self._profiles("TRUE", ())

    def _current_id(self, maker: str, model: str, program: str) -> int | None:
        row = self._conn.execute(
            """
            SELECT max(id) FROM log.lab_profile
            WHERE maker = %s AND model = %s AND program = %s
            """,
            (maker, model, program),
        ).fetchone()
        return None if row is None or row[0] is None else int(row[0])

    def _profiles(self, where: str, params: tuple[object, ...]) -> tuple[LabProfile, ...]:
        rows = self._conn.execute(
            f"""
            SELECT p.id, p.maker, p.model, p.program, p.settings, p.note, st.name, p.serial,
                p.again_of, p.replaces, op.callsign, p.saved_at,
                p.id = (SELECT max(c.id) FROM log.lab_profile c
                        WHERE c.maker = p.maker AND c.model = p.model
                            AND c.program = p.program)
            FROM log.lab_profile p
            LEFT JOIN log.station st ON st.id = p.station_id
            JOIN log.operator op ON op.id = p.saved_by
            WHERE {where}
            ORDER BY p.id DESC
            """,  # noqa: S608 - only this module's fixed conditions are formatted in
            params,
        ).fetchall()
        return tuple(
            LabProfile(
                id=int(pid),
                maker=str(maker),
                model=str(model),
                program=str(program),
                settings=tuple(
                    StockSetting(
                        id=str(s["id"]),
                        label=str(s["label"]),
                        bytes=str(s["bytes"]),
                        shown=None if s.get("shown") is None else str(s["shown"]),
                    )
                    for s in settings
                ),
                note=None if note is None else str(note),
                station=None if station is None else str(station),
                serial=None if serial is None else str(serial),
                again_of=None if again_of is None else int(again_of),
                replaces=None if replaces is None else int(replaces),
                saved_by=str(by),
                saved_at=saved_at,
                current=bool(current),
            )
            for (
                pid,
                maker,
                model,
                program,
                settings,
                note,
                station,
                serial,
                again_of,
                replaces,
                by,
                saved_at,
                current,
            ) in rows
        )


__all__ = [
    "PROGRAMS",
    "LabProfile",
    "LabProfileMovedError",
    "LabProfileNotFoundError",
    "LabProfileStore",
]
