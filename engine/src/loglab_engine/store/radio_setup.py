# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""Radio settings by radio: its as-received record and its named setups (Principle X).

A Station Agent reads every setting its rig definition lists and sends them
here, so a radio carried to another station is recognized there: the Agent
there can say what has changed and offer to put a setup back. A radio is its
maker, model and serial number.

* The **as-received record** is the first reading of a radio. It is kept once
  and never replaced (migration ``0021_radio_setup``).
* A **setup** is a reading kept under a name, like FT8 or VARA. Keeping one
  under the same name again adds a version; the newest is the setup.

Each setting is the bytes the radio sent, in hex, with the id and label the
Agent's definition gives it. The Engine stores and hands back settings and
never sends one to a radio: putting a setup back is the Agent's, only on the
operator's confirmed request (Principle X).
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import datetime
from typing import TYPE_CHECKING, Final

from psycopg.types.json import Jsonb

from loglab_engine.store.log import StoreError, check_station

if TYPE_CHECKING:
    import psycopg
    from psycopg.rows import TupleRow

_SERIAL: Final = re.compile(r"^[A-Za-z0-9._-]{1,64}$")
_ID: Final = re.compile(r"^[a-z][a-z0-9_]{0,63}$")
_BYTES: Final = re.compile(r"^[0-9A-F]{2}( [0-9A-F]{2}){0,63}$")
_PLAIN: Final = re.compile(r"^[^\x00-\x1f\x7f]*$")
# The most settings one reading may hold; an IC-7300 has 259.
MAX_SETTINGS: Final = 2000
MAX_NAME: Final = 40


@dataclass(frozen=True, slots=True)
class Setting:
    """One setting as the radio sent it."""

    id: str
    label: str
    # The bytes, as hex pairs like "00 67"; None if the radio wouldn't say.
    bytes: str | None


@dataclass(frozen=True, slots=True)
class Radio:
    """Which radio: its maker, model and serial number."""

    maker: str
    model: str
    serial: str


@dataclass(frozen=True, slots=True)
class Reading:
    """A kept reading of a radio's settings."""

    radio: Radio
    # The setup's name; None for the as-received record.
    name: str | None
    settings: tuple[Setting, ...]
    station: str
    # Who was signed in at that station, if anyone.
    operator: str | None
    saved_at: datetime


def _check(radio: Radio, name: str | None, settings: tuple[Setting, ...]) -> None:
    for field, value in (("maker", radio.maker), ("model", radio.model)):
        if not 1 <= len(value) <= MAX_NAME or not _PLAIN.match(value):
            msg = f"the radio's {field} must be 1-{MAX_NAME} plain characters"
            raise StoreError(msg)
    if not _SERIAL.match(radio.serial):
        msg = "the radio's serial number must be 1-64 letters, digits, '.', '_' or '-'"
        raise StoreError(msg)
    if name is not None and (not 1 <= len(name) <= MAX_NAME or not _PLAIN.match(name)):
        msg = f"a setup's name must be 1-{MAX_NAME} plain characters"
        raise StoreError(msg)
    if not 1 <= len(settings) <= MAX_SETTINGS:
        msg = f"a reading holds 1 to {MAX_SETTINGS} settings"
        raise StoreError(msg)
    seen: set[str] = set()
    for setting in settings:
        if not _ID.match(setting.id) or setting.id in seen:
            msg = f"setting id {setting.id!r} is not valid, or is there twice"
            raise StoreError(msg)
        seen.add(setting.id)
        if not 1 <= len(setting.label) <= 200 or not _PLAIN.match(setting.label):  # noqa: PLR2004
            msg = f"setting {setting.id!r} needs a label of 1-200 plain characters"
            raise StoreError(msg)
        if setting.bytes is not None and not _BYTES.match(setting.bytes):
            msg = f"setting {setting.id!r}: bytes must be hex pairs like '00 67'"
            raise StoreError(msg)


class RadioSetupStore:
    """Keeping and finding radio readings, on one autocommit connection."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn`` for every operation; each method is one transaction."""
        self._conn = conn

    def keep(
        self,
        station: str,
        radio: Radio,
        name: str | None,
        settings: tuple[Setting, ...],
    ) -> tuple[Reading, bool]:
        """Keep a reading sent by ``station``'s Agent, and whether it was new.

        With ``name`` None it is the radio's as-received record: kept only if
        the radio has none, otherwise the one it has is returned unchanged.
        With a name, it becomes the newest version of that setup.

        Raises:
            StoreError: Something in the reading is not valid.
        """
        check_station(station)
        _check(radio, name, settings)
        conn = self._conn
        with conn.transaction():
            conn.execute(
                "INSERT INTO log.station (name) VALUES (%s) ON CONFLICT DO NOTHING", (station,)
            )
            if name is None:
                existing = self._as_received(radio)
                if existing is not None:
                    return existing, False
            row = conn.execute(
                """
                INSERT INTO log.radio_setup
                    (maker, model, serial, name, settings, station_id, session_id)
                SELECT %s, %s, %s, %s, %s, st.id,
                    (SELECT s.id FROM log.session s
                     WHERE s.station_id = st.id AND log.session_live(s.id)
                     ORDER BY s.started_at DESC LIMIT 1)
                FROM log.station st WHERE st.name = %s
                RETURNING id
                """,
                (
                    radio.maker,
                    radio.model,
                    radio.serial,
                    name,
                    Jsonb([{"id": s.id, "label": s.label, "bytes": s.bytes} for s in settings]),
                    station,
                ),
            ).fetchone()
            if row is None:  # pragma: no cover - the station was just made
                msg = f"station {station!r} is missing"
                raise StoreError(msg)
            kept = self._readings("r.id = %s", (row[0],))
        return kept[0], True

    def as_received(self, radio: Radio) -> Reading | None:
        """The radio's as-received record, if it has one."""
        return self._as_received(radio)

    def setups(self, radio: Radio) -> tuple[Reading, ...]:
        """The radio's setups, the newest version of each, by name."""
        return self._readings(
            """
            r.maker = %s AND r.model = %s AND r.serial = %s AND r.name IS NOT NULL
            AND r.id = (SELECT max(n.id) FROM log.radio_setup n
                        WHERE n.maker = r.maker AND n.model = r.model
                          AND n.serial = r.serial AND n.name = r.name)
            """,
            (radio.maker, radio.model, radio.serial),
        )

    def _as_received(self, radio: Radio) -> Reading | None:
        found = self._readings(
            "r.maker = %s AND r.model = %s AND r.serial = %s AND r.name IS NULL",
            (radio.maker, radio.model, radio.serial),
        )
        return found[0] if found else None

    def _readings(self, where: str, params: tuple[object, ...]) -> tuple[Reading, ...]:
        rows = self._conn.execute(
            f"""
            SELECT r.maker, r.model, r.serial, r.name, r.settings, st.name, o.callsign,
                r.saved_at
            FROM log.radio_setup r
            JOIN log.station st ON st.id = r.station_id
            LEFT JOIN log.session s ON s.id = r.session_id
            LEFT JOIN log.operator o ON o.id = s.operator_id
            WHERE {where}
            ORDER BY r.name NULLS FIRST, r.id
            """,  # noqa: S608 - only this module's fixed conditions are formatted in
            params,
        ).fetchall()
        return tuple(
            Reading(
                radio=Radio(str(maker), str(model), str(serial)),
                name=None if name is None else str(name),
                settings=tuple(
                    Setting(
                        id=str(s["id"]),
                        label=str(s["label"]),
                        bytes=None if s["bytes"] is None else str(s["bytes"]),
                    )
                    for s in settings
                ),
                station=str(station),
                operator=None if operator is None else str(operator),
                saved_at=saved_at,
            )
            for maker, model, serial, name, settings, station, operator, saved_at in rows
        )


__all__ = ["MAX_SETTINGS", "Radio", "RadioSetupStore", "Reading", "Setting"]
