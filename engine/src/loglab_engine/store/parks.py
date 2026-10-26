# SPDX-FileCopyrightText: 2026 LabLog contributors
# SPDX-License-Identifier: AGPL-3.0-only

"""The Parks on the Air park list in the Engine's database, for lookups with no internet.

An import loads the file into a temporary staging table, then replaces
``reference.park`` in one transaction (Principle XIV): a lookup sees the old
list until the new one is complete, and a file that fails part way changes
nothing. A file with too many unreadable parks is refused, keeping the list
as it was (Principle XII). Every import is recorded with its publication
date (Principle VI).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Final

from loglab_engine.reference.licenses import Tally
from loglab_engine.reference.parks import Park, ParkFileError, park_reference, read_parks

if TYPE_CHECKING:
    from datetime import datetime
    from pathlib import Path

    import psycopg
    from psycopg.rows import TupleRow

MAX_SKIPPED_SHARE: Final = 0.01
MIN_SKIPPED_ALLOWED: Final = 50
MAX_FOUND: Final = 50

_COLUMNS: Final = "reference, name, active, locations, latitude, longitude, grid"


@dataclass(frozen=True, slots=True)
class ParkImport:
    """What an import did."""

    records: int
    skipped: int
    examples: tuple[str, ...] = ()


@dataclass(frozen=True, slots=True)
class ParkListState:
    """How current the park list is."""

    parks: int
    file_name: str | None
    published: datetime | None
    imported: datetime | None


def _park(row: tuple[object, ...]) -> Park:
    reference, name, active, locations, latitude, longitude, grid = row
    return Park(
        reference=str(reference),
        name=str(name),
        active=bool(active),
        locations=None if locations is None else str(locations),
        latitude=None if latitude is None else float(str(latitude)),
        longitude=None if longitude is None else float(str(longitude)),
        grid=None if grid is None else str(grid),
    )


class ParkStore:
    """Reads and replaces the park list."""

    def __init__(self, conn: psycopg.Connection[TupleRow]) -> None:
        """Use ``conn``, an autocommit connection from :func:`connect`."""
        self._conn = conn

    def find(self, reference: str) -> Park | None:
        """The park with this reference, like ``US-1234``, if the list has it."""
        wanted = park_reference(reference)
        if wanted is None:
            return None
        row = self._conn.execute(
            f"SELECT {_COLUMNS} FROM reference.park WHERE reference = %s",  # noqa: S608 - fixed columns
            (wanted,),
        ).fetchone()
        return None if row is None else _park(row)

    def search(self, text: str, limit: int = 20) -> list[Park]:
        """Parks whose reference or name starts with ``text``, or whose name holds it.

        A reference match comes first, then names that start with the text,
        then names that hold it; active parks before inactive ones.
        """
        words = " ".join(text.split())[:100]
        if not words:
            return []
        upper = words.upper()
        lower = words.lower()
        pattern = lower.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        rows = self._conn.execute(
            f"""
            SELECT {_COLUMNS} FROM reference.park
            WHERE reference LIKE %(ref)s OR lower(name) LIKE %(inside)s
            ORDER BY (reference = %(exact)s) DESC, (reference LIKE %(ref)s) DESC,
                     (lower(name) LIKE %(start)s) DESC, active DESC, reference
            LIMIT %(limit)s
            """,  # noqa: S608 - fixed columns
            {
                "exact": upper,
                "ref": pattern.upper() + "%",
                "start": pattern + "%",
                "inside": "%" + pattern + "%",
                "limit": max(1, min(limit, MAX_FOUND)),
            },
        ).fetchall()
        return [_park(row) for row in rows]

    def import_file(self, path: Path, *, published: datetime | None = None) -> ParkImport:
        """Load pota.app's ``all_parks_ext.csv``, replacing every park.

        Raises:
            ParkFileError: The file is not a park list, or too much of it
                could not be read. Nothing is changed.
        """
        tally = Tally()
        with self._conn.transaction():
            self._conn.execute(
                "CREATE TEMP TABLE stage_park (LIKE reference.park INCLUDING DEFAULTS)"
                " ON COMMIT DROP"
            )
            seen: set[str] = set()
            with self._conn.cursor().copy(f"COPY stage_park ({_COLUMNS}) FROM STDIN") as copy:
                for park in read_parks(path, tally):
                    if park.reference in seen:
                        tally.skip(f"{park.reference} listed twice")
                        continue
                    seen.add(park.reference)
                    copy.write_row(
                        (
                            park.reference,
                            park.name,
                            park.active,
                            park.locations,
                            park.latitude,
                            park.longitude,
                            park.grid,
                        )
                    )
            allowed = max(MIN_SKIPPED_ALLOWED, int(tally.read * MAX_SKIPPED_SHARE))
            if tally.skipped > allowed:
                reasons = "; ".join(tally.examples)
                raise ParkFileError(
                    f"{path.name}: {tally.skipped} of {tally.read} parks could not be read"
                    f" ({reasons})"
                )
            if not seen:
                raise ParkFileError(f"{path.name} lists no parks")
            self._conn.execute("DELETE FROM reference.park")
            records = self._conn.execute(
                f"INSERT INTO reference.park ({_COLUMNS}) SELECT {_COLUMNS} FROM stage_park"  # noqa: S608 - fixed columns
            ).rowcount
            self._conn.execute(
                "INSERT INTO reference.park_import (file_name, published, records)"
                " VALUES (%s, %s, %s)",
                (path.name[:255], published, records),
            )
        return ParkImport(records, tally.skipped, tuple(tally.examples))

    def state(self) -> ParkListState:
        """How many parks are loaded, and from which file."""
        count = self._conn.execute("SELECT count(*) FROM reference.park").fetchone()
        last = self._conn.execute(
            "SELECT file_name, published, imported_at FROM reference.park_import"
            " ORDER BY imported_at DESC, id DESC LIMIT 1"
        ).fetchone()
        return ParkListState(
            parks=0 if count is None else int(count[0]),
            file_name=None if last is None else last[0],
            published=None if last is None else last[1],
            imported=None if last is None else last[2],
        )
